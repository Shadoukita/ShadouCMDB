//! Impact analysis against a real PostgreSQL (SHAA-883 spec §6.1).

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body as HttpBody;
use axum::http::{HeaderMap, Request, header};
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

use super::schemas::{AnalysisDirection, ImpactAnalysis, ImpactQuery, TruncatedReason, Visibility};
use super::{ImpactState, service};
use crate::api::context::RequestContext;
use crate::api::route::{Query, QueryInput};
use crate::api::schemas::{QueryBool, UuidList};
use crate::auth::permissions::{ClassRights, Permissions};
use crate::auth::{Credential, Principal};
use crate::config::ImpactConfig;
use crate::db::scratch;
use crate::http::error::ErrorCode;
use crate::modules::api_tokens::tests::{Creds, app, call};

// ---------------------------------------------------------------------------
// Fixture: classes, relationship types (one per impact direction) and CIs
// ---------------------------------------------------------------------------

struct Fixture {
    app: Router,
    session: Creds,
    pool: PgPool,
    /// Class ids by key
    classes: HashMap<&'static str, Uuid>,
    /// Relationship type ids by key: t2s (target_to_source), s2t, both, none, peer (non-directional, both)
    types: HashMap<&'static str, Uuid>,
}

const CLASSES: &[(&str, Option<&str>)] =
    &[("app", None), ("db", None), ("svc", None), ("secret", None), ("base", None), ("sub", Some("base"))];

async fn fixture(db: &scratch::Scratch) -> Fixture {
    let app = app(db.pool.clone());
    let setup = json!({ "username": "owner", "displayName": "Owner", "password": "correct horse battery", "setupToken": crate::auth::setup_token::TEST_TOKEN });
    let (status, me, headers) = call(&app, "POST", "/api/v1/setup", &Creds::default(), Some(setup)).await;
    assert_eq!(status, 201, "{me}");
    let cookie = headers
        .get_all(header::SET_COOKIE)
        .iter()
        .map(|v| v.to_str().unwrap().split(';').next().unwrap().to_owned())
        .collect::<Vec<_>>()
        .join("; ");
    let session = Creds { cookie: Some(cookie), csrf: me["csrfToken"].as_str().map(str::to_owned), bearer: None };

    let mut classes = HashMap::new();
    for (key, parent) in CLASSES {
        let mut body = json!({ "key": key, "name": key.to_uppercase() });
        if let Some(p) = parent {
            body["parentId"] = json!(classes[p]);
        }
        let (status, v, _) = call(&app, "POST", "/api/v1/ci-classes", &session, Some(body)).await;
        assert_eq!(status, 201, "{v}");
        classes.insert(*key, v["id"].as_str().unwrap().parse().unwrap());
    }

    let pool = db.pool.clone();
    let mut types = HashMap::new();
    for (key, directional, impact) in [
        ("t2s", true, "target_to_source"),
        ("s2t", true, "source_to_target"),
        ("both", true, "both"),
        ("none", true, "none"),
        ("peer", false, "both"),
    ] {
        let id: Uuid = sqlx::query_scalar(
            "INSERT INTO relationship_types (key, name, forward_label, reverse_label, is_directional, impact_direction)
             VALUES ($1, $1, $1 || ' of', 'has ' || $1, $2, $3) RETURNING id",
        )
        .bind(key)
        .bind(directional)
        .bind(impact)
        .fetch_one(&pool)
        .await
        .unwrap();
        // Any class may be source and target (rules match descendants too).
        sqlx::query(
            "INSERT INTO relationship_type_rules (relationship_type_id, source_class_id, target_class_id)
             SELECT $1, a.id, b.id FROM ci_classes a, ci_classes b WHERE a.parent_id IS NULL AND b.parent_id IS NULL",
        )
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
        types.insert(key, id);
    }
    Fixture { app, session, pool, classes, types }
}

impl Fixture {
    async fn ci(&self, class: &str) -> Uuid {
        let body = json!({ "classId": self.classes[class] });
        let (status, v, _) = call(&self.app, "POST", "/api/v1/configuration-items", &self.session, Some(body)).await;
        assert_eq!(status, 201, "{v}");
        v["id"].as_str().unwrap().parse().unwrap()
    }

    async fn cis(&self, class: &str, n: usize) -> Vec<Uuid> {
        let mut out = Vec::with_capacity(n);
        for _ in 0..n {
            out.push(self.ci(class).await);
        }
        out
    }

    /// An edge `source -type-> target`, created after every earlier one.
    async fn edge(&self, kind: &str, source: Uuid, target: Uuid) -> Uuid {
        sqlx::query_scalar(
            "INSERT INTO ci_relationships (relationship_type_id, source_ci_id, target_ci_id, created_at)
             VALUES ($1, $2, $3, clock_timestamp()) RETURNING id",
        )
        .bind(self.types[kind])
        .bind(source)
        .bind(target)
        .fetch_one(&self.pool)
        .await
        .unwrap()
    }

    /// `x` affects `y` (downstream from x reaches y): y t2s-depends on x.
    async fn affects(&self, x: Uuid, y: Uuid) -> Uuid {
        self.edge("t2s", y, x).await
    }

    /// Edges in bulk (source, target) of one type, in order.
    async fn edges(&self, kind: &str, pairs: &[(Uuid, Uuid)]) {
        let (s, t): (Vec<Uuid>, Vec<Uuid>) = pairs.iter().copied().unzip();
        sqlx::query(
            "INSERT INTO ci_relationships (relationship_type_id, source_ci_id, target_ci_id, created_at)
             SELECT $1, s, t, clock_timestamp() + n * interval '1 microsecond'
             FROM unnest($2::uuid[], $3::uuid[]) WITH ORDINALITY AS u(s, t, n)",
        )
        .bind(self.types[kind])
        .bind(&s)
        .bind(&t)
        .execute(&self.pool)
        .await
        .unwrap();
    }
}

fn admin() -> RequestContext {
    RequestContext::system("test", "impact-test")
}

/// A user who may view only these classes.
fn viewer(classes: &[Uuid]) -> RequestContext {
    let permissions = Permissions {
        classes: classes.iter().map(|id| (*id, ClassRights { view: true, ..Default::default() })).collect(),
        ..Default::default()
    };
    let principal =
        Principal { user_id: Uuid::new_v4(), username: "viewer".into(), credential: Credential::Token, permissions };
    RequestContext::user(Arc::new(principal), "impact-viewer".into())
}

fn q(direction: AnalysisDirection, depth: i32) -> ImpactQuery {
    ImpactQuery {
        direction,
        depth: Some(depth),
        relationship_type_id: None,
        include_inactive: QueryBool::True,
        max_nodes: None,
    }
}

fn down(depth: i32) -> ImpactQuery {
    q(AnalysisDirection::Downstream, depth)
}

fn up(depth: i32) -> ImpactQuery {
    q(AnalysisDirection::Upstream, depth)
}

async fn run(f: &Fixture, ctx: &RequestContext, root: Uuid, query: &ImpactQuery) -> ImpactAnalysis {
    let state = Arc::new(ImpactState::default());
    run_with(f, ctx, &state, root, query).await
}

async fn run_with(
    f: &Fixture,
    ctx: &RequestContext,
    state: &Arc<ImpactState>,
    root: Uuid,
    query: &ImpactQuery,
) -> ImpactAnalysis {
    match service::analyse(&f.pool, ctx, state, root, query).await {
        Ok(a) => a.result,
        Err(e) => panic!("analysis failed: {e:?}"),
    }
}

/// Hops by CI id.
fn hops(a: &ImpactAnalysis) -> BTreeMap<Uuid, i32> {
    a.items.iter().map(|i| (i.id, i.hops)).collect()
}

fn expect(pairs: &[(Uuid, i32)]) -> BTreeMap<Uuid, i32> {
    pairs.iter().copied().collect()
}

// ---------------------------------------------------------------------------
// Semantics
// ---------------------------------------------------------------------------

/// Each impact direction, downstream, upstream and both; a non-directional
/// type with both; a `none` type is ignored even when asked for.
#[tokio::test]
async fn impact_flows_along_each_types_direction() {
    let Some(db) = scratch::database("impact_flows_along_each_types_direction").await else { return };
    let f = fixture(&db).await;
    let [root, a, b, c, d, e] = f.cis("app", 6).await[..] else { unreachable!() };
    f.edge("t2s", a, root).await; // root fails -> a (a runs on root)
    f.edge("s2t", root, b).await; // root fails -> b (root hosts b)
    f.edge("both", c, root).await; // either way
    f.edge("none", d, root).await; // never
    f.edge("peer", root, e).await; // non-directional, both
    let x = f.ci("app").await;
    f.edge("t2s", root, x).await; // x fails -> root: upstream of root

    let ctx = admin();
    let r = run(&f, &ctx, root, &down(1)).await;
    assert_eq!(hops(&r), expect(&[(a, 1), (b, 1), (c, 1), (e, 1)]));
    let r = run(&f, &ctx, root, &up(1)).await;
    assert_eq!(hops(&r), expect(&[(c, 1), (e, 1), (x, 1)]));

    let both = run(&f, &ctx, root, &q(AnalysisDirection::Both, 1)).await;
    let dirs: BTreeMap<Uuid, Vec<String>> = both
        .items
        .iter()
        .map(|i| {
            (i.id, i.directions.iter().map(|d| serde_json::to_value(d).unwrap().as_str().unwrap().to_owned()).collect())
        })
        .collect();
    assert_eq!(dirs[&a], ["downstream"]);
    assert_eq!(dirs[&x], ["upstream"]);
    assert_eq!(dirs[&c], ["downstream", "upstream"]);
    assert_eq!(dirs[&e], ["downstream", "upstream"]);
    assert!(!dirs.contains_key(&d));

    // Asking for the `none` type is accepted and contributes nothing.
    let mut only_none = down(1);
    only_none.relationship_type_id = Some(UuidList(vec![f.types["none"]]));
    let r = run(&f, &ctx, root, &only_none).await;
    assert!(r.items.is_empty());
    assert_eq!(r.parameters.relationship_type_ids, [f.types["none"]]);
    let mut only_t2s = down(1);
    only_t2s.relationship_type_id = Some(UuidList(vec![f.types["t2s"], f.types["none"]]));
    assert_eq!(hops(&run(&f, &ctx, root, &only_t2s).await), expect(&[(a, 1)]));

    // The via of a hop names the edge and reads from the parent.
    let r = run(&f, &ctx, root, &down(1)).await;
    let item_a = r.items.iter().find(|i| i.id == a).unwrap();
    assert_eq!((item_a.via.parent_id, item_a.via.edge_source_id, item_a.via.edge_target_id), (root, a, root));
    assert_eq!(item_a.via.relationship_type.key, "t2s");
    db.drop().await;
}

/// Cycles end by themselves; each CI appears once at its shortest distance;
/// the root is never listed; a CI reached two ways counts both edges.
#[tokio::test]
async fn cycles_terminate_and_the_shortest_path_wins() {
    let Some(db) = scratch::database("cycles_terminate_and_the_shortest_path_wins").await else { return };
    let f = fixture(&db).await;
    let [root, a, b, c, far] = f.cis("app", 5).await[..] else { unreachable!() };
    // root -> a -> b -> c -> a (3-cycle) and c -> root (back to the root)
    f.affects(root, a).await;
    f.affects(a, b).await;
    f.affects(b, c).await;
    f.affects(c, a).await;
    f.affects(c, root).await;
    // far: root -> far directly and root -> a -> b -> far
    f.affects(b, far).await;
    f.affects(root, far).await;

    let r = run(&f, &admin(), root, &down(10)).await;
    assert_eq!(hops(&r), expect(&[(a, 1), (far, 1), (b, 2), (c, 3)]));
    assert!(!r.items.iter().any(|i| i.id == root));
    let by_id: HashMap<Uuid, _> = r.items.iter().map(|i| (i.id, i)).collect();
    assert_eq!(by_id[&far].via.parent_id, root);
    assert_eq!(by_id[&far].reached_by_count, 2, "root -> far and b -> far");
    assert_eq!(by_id[&a].reached_by_count, 2, "root -> a and c -> a");
    assert_eq!(by_id[&b].reached_by_count, 1);
    assert!(!r.truncated && !r.has_more_beyond_depth);
    // Every via chain resolves inside the items plus the root.
    for i in &r.items {
        assert!(i.via.parent_id == root || by_id.contains_key(&i.via.parent_id));
    }
    // Ordered by hops, then name.
    assert!(r.items.windows(2).all(|w| w[0].hops <= w[1].hops));
    db.drop().await;
}

/// Exact hop counts, hasMoreBeyondDepth, and the depth bounds.
#[tokio::test]
async fn depth_bounds_the_walk_and_reports_more_beyond_it() {
    let Some(db) = scratch::database("depth_bounds_the_walk_and_reports_more_beyond_it").await else { return };
    let f = fixture(&db).await;
    let chain = f.cis("app", 5).await;
    for w in chain.windows(2) {
        f.affects(w[0], w[1]).await;
    }
    let ctx = admin();
    let r = run(&f, &ctx, chain[0], &down(2)).await;
    assert_eq!(hops(&r), expect(&[(chain[1], 1), (chain[2], 2)]));
    assert!(r.has_more_beyond_depth && !r.truncated, "depth is not truncation");
    let r = run(&f, &ctx, chain[0], &down(4)).await;
    assert_eq!(r.items.len(), 4);
    assert!(!r.has_more_beyond_depth, "the chain ends at depth 4");
    let r = run(&f, &ctx, chain[0], &down(10)).await;
    assert!(!r.has_more_beyond_depth);
    assert_eq!(
        r.summary.by_hops.iter().map(|h| (h.hops, h.count)).collect::<Vec<_>>(),
        [(1, 1), (2, 1), (3, 1), (4, 1)]
    );

    // A hidden CI beyond the last hop is not "more".
    let secret = f.ci("secret").await;
    f.affects(chain[4], secret).await;
    let restricted = viewer(&[f.classes["app"]]);
    assert!(!run(&f, &restricted, chain[0], &down(4)).await.has_more_beyond_depth);
    assert!(run(&f, &ctx, chain[0], &down(4)).await.has_more_beyond_depth);

    // Out of range: refused, never lowered.
    for bad in ["depth=0", "depth=21", "maxNodes=0", "maxNodes=10001", "direction=sideways"] {
        let Err(err) = Query::<ImpactQuery>::parse(Some(bad)) else { panic!("{bad} parsed") };
        assert_eq!(err.code, ErrorCode::ValidationError, "{bad}");
    }
    let state = Arc::new(ImpactState::default());
    for (query, field) in [
        (ImpactQuery { depth: Some(11), ..down(1) }, "depth"),
        (ImpactQuery { max_nodes: Some(2001), ..down(1) }, "maxNodes"),
        (
            ImpactQuery { relationship_type_id: Some(UuidList((0..51).map(|_| Uuid::new_v4()).collect())), ..down(1) },
            "relationshipTypeId",
        ),
        (ImpactQuery { relationship_type_id: Some(UuidList(vec![Uuid::new_v4()])), ..down(1) }, "relationshipTypeId"),
    ] {
        let err = service::analyse(&f.pool, &ctx, &state, chain[0], &query).await.err().unwrap();
        assert_eq!(err.code, ErrorCode::ValidationError);
        assert_eq!(err.details.unwrap()[0].field, field);
    }
    db.drop().await;
}

/// maxNodes and the edge budget cut the walk deterministically; a deadline
/// answers 200 with what it had.
#[tokio::test]
async fn budgets_and_the_deadline_truncate_deterministically() {
    let Some(db) = scratch::database("budgets_and_the_deadline_truncate_deterministically").await else { return };
    let f = fixture(&db).await;
    let root = f.ci("app").await;
    let fan = f.cis("app", 30).await;
    f.edges("t2s", &fan.iter().map(|c| (*c, root)).collect::<Vec<_>>()).await;
    let ctx = admin();

    let small = ImpactQuery { max_nodes: Some(10), ..down(3) };
    let first = run(&f, &ctx, root, &small).await;
    assert_eq!((first.truncated, first.truncated_reason), (true, Some(TruncatedReason::MaxNodes)));
    assert_eq!(first.items.len(), 10);
    let again = run(&f, &ctx, root, &small).await;
    assert_eq!(hops(&first), hops(&again), "the same partial result");
    // The first edges win (created_at, id).
    let kept: Vec<Uuid> = fan[..10].to_vec();
    assert!(first.items.iter().all(|i| kept.contains(&i.id)));

    // A hub: more edges than 5 × maxNodes.
    let hub = f.ci("app").await;
    let spokes = f.cis("db", 12).await;
    f.edges("t2s", &spokes.iter().map(|c| (*c, hub)).collect::<Vec<_>>()).await;
    let r = run(&f, &ctx, hub, &ImpactQuery { max_nodes: Some(2), ..down(3) }).await;
    assert_eq!(r.truncated_reason, Some(TruncatedReason::MaxEdges));

    // A deadline that has passed (as IMPACT_TIMEOUT_MS=1 on any real graph,
    // without racing the clock): 200, truncated by timeout.
    let state = Arc::new(ImpactState::new(ImpactConfig { timeout: Duration::ZERO, ..ImpactConfig::default() }));
    let r = run_with(&f, &ctx, &state, root, &down(10)).await;
    assert_eq!((r.truncated, r.truncated_reason), (true, Some(TruncatedReason::Timeout)));
    db.drop().await;
}

// ---------------------------------------------------------------------------
// Hidden CIs (spec §4.2)
// ---------------------------------------------------------------------------

/// (a) a hidden root answers like a missing one; (b) A -> H -> C with H
/// hidden: C is absent and nothing names H; (c) with A -> B -> C too, C is
/// reached through B; (e) a grant on a class does not cover its subclasses.
#[tokio::test]
async fn hidden_cis_are_neither_returned_nor_walked_through() {
    let Some(db) = scratch::database("hidden_cis_are_neither_returned_nor_walked_through").await else { return };
    let f = fixture(&db).await;
    let restricted = viewer(&[f.classes["app"], f.classes["base"]]);
    let state = Arc::new(ImpactState::default());

    // (a)
    let h_root = f.ci("secret").await;
    let hidden = service::analyse(&f.pool, &restricted, &state, h_root, &down(1)).await.err().unwrap();
    let missing_id = Uuid::new_v4();
    let missing = service::analyse(&f.pool, &restricted, &state, missing_id, &down(1)).await.err().unwrap();
    assert_eq!((hidden.code, missing.code), (ErrorCode::NotFound, ErrorCode::NotFound));
    assert_eq!(hidden.message.replace(&h_root.to_string(), "X"), missing.message.replace(&missing_id.to_string(), "X"));

    // (b)
    let a = f.ci("app").await;
    let h = f.ci("secret").await;
    let c = f.ci("app").await;
    f.affects(a, h).await;
    f.affects(h, c).await;
    let r = run(&f, &restricted, a, &down(5)).await;
    assert!(r.items.is_empty(), "{:?}", hops(&r));
    assert_eq!(r.summary.total, 0);
    assert_eq!(r.visibility, Visibility::Restricted);
    let text = serde_json::to_string(&r).unwrap();
    assert!(!text.contains(&h.to_string()) && !text.contains(&c.to_string()));
    // Everything is there for an administrator.
    assert_eq!(hops(&run(&f, &admin(), a, &down(5)).await), expect(&[(h, 1), (c, 2)]));

    // (c) A -> B -> C too: C comes through B; the edge from H is not counted.
    let b = f.ci("app").await;
    f.affects(a, b).await;
    f.affects(b, c).await;
    let r = run(&f, &restricted, a, &down(5)).await;
    assert_eq!(hops(&r), expect(&[(b, 1), (c, 2)]));
    let item_c = r.items.iter().find(|i| i.id == c).unwrap();
    assert_eq!((item_c.via.parent_id, item_c.reached_by_count), (b, 1));
    assert!(!serde_json::to_string(&r).unwrap().contains(&h.to_string()));

    // (e) View on "base" does not cover "sub".
    let child = f.ci("sub").await;
    let parent = f.ci("base").await;
    f.affects(a, parent).await;
    f.affects(a, child).await;
    let r = run(&f, &restricted, a, &down(1)).await;
    assert!(r.items.iter().any(|i| i.id == parent));
    assert!(!r.items.iter().any(|i| i.id == child));
    let err = service::analyse(&f.pool, &restricted, &state, child, &down(1)).await.err().unwrap();
    assert_eq!(err.code, ErrorCode::NotFound);
    db.drop().await;
}

/// Release gate (d): two datasets that differ only in hidden CIs (and their
/// relationships) give a restricted caller byte-identical answers, apart from
/// elapsedMs. `visibility` is restricted whether or not anything is hidden.
#[tokio::test]
async fn hidden_cis_do_not_change_a_restricted_answer() {
    let Some(db) = scratch::database("hidden_cis_do_not_change_a_restricted_answer").await else { return };
    let f = fixture(&db).await;
    let visible = [f.classes["app"], f.classes["db"]];
    let restricted = viewer(&visible);
    let apps = f.cis("app", 6).await;
    let dbs = f.cis("db", 3).await;
    let root = apps[0];
    for (x, y) in [(0, 1), (1, 2), (2, 3), (3, 1), (0, 4), (4, 5)] {
        f.affects(apps[x], apps[y]).await;
    }
    f.affects(apps[2], dbs[0]).await;
    f.edge("both", dbs[1], apps[5]).await;
    f.edge("s2t", dbs[2], apps[0]).await; // upstream of the root

    let answer = |a: ImpactAnalysis| {
        let mut v = serde_json::to_value(&a).unwrap();
        v.as_object_mut().unwrap().remove("elapsedMs");
        v.to_string()
    };
    let queries: Vec<ImpactQuery> =
        vec![down(3), q(AnalysisDirection::Both, 10), up(10), ImpactQuery { max_nodes: Some(3), ..down(10) }, down(1)];
    let mut before = Vec::new();
    for query in &queries {
        let r = run(&f, &restricted, root, query).await;
        assert_eq!(r.visibility, Visibility::Restricted, "nothing hidden yet, still restricted");
        before.push(answer(r));
    }
    let csv_before = service::csv_of(&f.pool, &restricted, root, &down(3)).await;

    // Hidden CIs everywhere: between visible ones, hanging off them, as a hub,
    // in cycles, and one hop beyond every depth.
    let secrets = f.cis("secret", 12).await;
    for (i, s) in secrets.iter().enumerate() {
        let v = apps[i % apps.len()];
        f.affects(v, *s).await;
        f.affects(*s, apps[(i + 3) % apps.len()]).await;
        f.edge("both", *s, dbs[i % dbs.len()]).await;
        f.edge("s2t", *s, root).await;
    }
    for w in secrets.windows(2) {
        f.affects(w[0], w[1]).await;
    }
    let sub = f.ci("sub").await; // a class the viewer has no grant on at all
    f.affects(apps[5], sub).await;

    for (query, expected) in queries.iter().zip(&before) {
        assert_eq!(&answer(run(&f, &restricted, root, query).await), expected);
    }
    assert_eq!(service::csv_of(&f.pool, &restricted, root, &down(3)).await, csv_before);
    // The administrator does see them.
    assert!(run(&f, &admin(), root, &down(3)).await.items.iter().any(|i| secrets.contains(&i.id)));
    db.drop().await;
}

// ---------------------------------------------------------------------------
// Deleted and retired data, configuration changes
// ---------------------------------------------------------------------------

#[tokio::test]
async fn deleted_data_is_skipped_and_retired_types_still_propagate() {
    let Some(db) = scratch::database("deleted_data_is_skipped_and_retired_types_still_propagate").await else { return };
    let f = fixture(&db).await;
    let [root, a, b, c] = f.cis("app", 4).await[..] else { unreachable!() };
    let e_a = f.affects(root, a).await;
    f.affects(a, b).await;
    f.affects(root, c).await;
    let ctx = admin();
    assert_eq!(hops(&run(&f, &ctx, root, &down(3)).await), expect(&[(a, 1), (c, 1), (b, 2)]));

    // A soft-deleted CI is not traversed (its edges went with it).
    let (status, v, _) = call(&f.app, "DELETE", &format!("/api/v1/configuration-items/{c}"), &f.session, None).await;
    assert_eq!(status, 204, "{v}");
    assert_eq!(hops(&run(&f, &ctx, root, &down(3)).await), expect(&[(a, 1), (b, 2)]));
    // A soft-deleted edge is not either.
    sqlx::query("UPDATE ci_relationships SET deleted_at = now() WHERE id = $1")
        .bind(e_a)
        .execute(&f.pool)
        .await
        .unwrap();
    assert!(run(&f, &ctx, root, &down(3)).await.items.is_empty());
    // A soft-deleted root answers 404.
    let state = Arc::new(ImpactState::default());
    let err = service::analyse(&f.pool, &ctx, &state, c, &down(1)).await.err().unwrap();
    assert_eq!(err.code, ErrorCode::NotFound);

    // A retired type still propagates; a changed direction applies at once.
    let d = f.ci("app").await;
    f.affects(root, d).await;
    let t2s = f.types["t2s"];
    let (status, v, _) = call(
        &f.app,
        "PATCH",
        &format!("/api/v1/relationship-types/{t2s}"),
        &f.session,
        Some(json!({ "isActive": false })),
    )
    .await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(hops(&run(&f, &ctx, root, &down(3)).await), expect(&[(d, 1)]));
    let (status, v, _) = call(
        &f.app,
        "PATCH",
        &format!("/api/v1/relationship-types/{t2s}"),
        &f.session,
        Some(json!({ "impactDirection": "source_to_target" })),
    )
    .await;
    assert_eq!((status, v["impactDirection"].as_str()), (200, Some("source_to_target")), "{v}");
    assert!(run(&f, &ctx, root, &down(3)).await.items.is_empty());
    assert_eq!(hops(&run(&f, &ctx, root, &up(3)).await), expect(&[(d, 1)]));

    // Inactive CIs: returned with active=false, or neither returned nor followed.
    let (status, v, _) = call(
        &f.app,
        "PATCH",
        &format!("/api/v1/configuration-items/{d}"),
        &f.session,
        Some(json!({ "validUntil": "2020-01-01T00:00:00Z", "validFrom": "2019-01-01T00:00:00Z" })),
    )
    .await;
    assert_eq!(status, 200, "{v}");
    let e = f.ci("app").await;
    f.edge("s2t", d, e).await;
    let r = run(&f, &ctx, root, &up(3)).await;
    assert_eq!(hops(&r), expect(&[(d, 1)]));
    let r = run(&f, &ctx, d, &down(3)).await;
    assert_eq!(hops(&r), expect(&[(root, 1), (e, 1)]));
    let r = run(&f, &ctx, e, &up(3)).await;
    assert_eq!(hops(&r), expect(&[(d, 1)]));
    assert!(!r.items[0].active);
    let r = run(&f, &ctx, e, &ImpactQuery { include_inactive: QueryBool::False, ..up(3) }).await;
    assert!(r.items.is_empty(), "d is inactive: neither returned nor followed");
    db.drop().await;
}

/// Non-directional types: only none or both, through the API and in the database.
#[tokio::test]
async fn non_directional_types_propagate_both_ways_or_not_at_all() {
    let Some(db) = scratch::database("non_directional_types_propagate_both_ways_or_not_at_all").await else { return };
    let f = fixture(&db).await;
    let peer = f.types["peer"];
    let (status, v, _) = call(
        &f.app,
        "PATCH",
        &format!("/api/v1/relationship-types/{peer}"),
        &f.session,
        Some(json!({ "impactDirection": "target_to_source" })),
    )
    .await;
    assert_eq!((status, v["error"]["details"][0]["field"].as_str()), (400, Some("impactDirection")), "{v}");
    let body = json!({ "key": "twin", "name": "Twin", "forwardLabel": "twins", "reverseLabel": "twins",
        "isDirectional": false, "impactDirection": "source_to_target" });
    let (status, v, _) = call(&f.app, "POST", "/api/v1/relationship-types", &f.session, Some(body)).await;
    assert_eq!((status, v["error"]["details"][0]["field"].as_str()), (400, Some("impactDirection")), "{v}");
    let body = json!({ "key": "twin", "name": "Twin", "forwardLabel": "twins", "reverseLabel": "twins",
        "isDirectional": false, "impactDirection": "both" });
    let (status, v, _) = call(&f.app, "POST", "/api/v1/relationship-types", &f.session, Some(body)).await;
    assert_eq!((status, v["impactDirection"].as_str()), (201, Some("both")), "{v}");
    // Left out: none.
    let body = json!({ "key": "docs", "name": "Docs", "forwardLabel": "documents", "reverseLabel": "documented by" });
    let (status, v, _) = call(&f.app, "POST", "/api/v1/relationship-types", &f.session, Some(body)).await;
    assert_eq!((status, v["impactDirection"].as_str()), (201, Some("none")), "{v}");

    let err = sqlx::query("UPDATE relationship_types SET impact_direction = 'source_to_target' WHERE id = $1")
        .bind(peer)
        .execute(&f.pool)
        .await
        .unwrap_err();
    assert_eq!(err.as_database_error().unwrap().constraint(), Some("relationship_types_impact_nondirectional"));
    db.drop().await;
}

// ---------------------------------------------------------------------------
// HTTP: status codes, export, audit, concurrency, settings
// ---------------------------------------------------------------------------

async fn raw(app: &Router, path: &str, creds: &Creds) -> (u16, String, HeaderMap) {
    let mut req = Request::builder().method("GET").uri(path);
    if let Some(c) = &creds.cookie {
        req = req.header(header::COOKIE, c);
    }
    if let Some(b) = &creds.bearer {
        req = req.header(header::AUTHORIZATION, format!("Bearer {b}"));
    }
    let res = app.clone().oneshot(req.body(HttpBody::empty()).unwrap()).await.unwrap();
    let status = res.status().as_u16();
    let headers = res.headers().clone();
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 22).await.unwrap();
    (status, String::from_utf8(bytes.to_vec()).unwrap(), headers)
}

/// CSV injection, headers, the one `export` audit row without data, and a
/// restricted export equal to the restricted JSON.
#[tokio::test]
async fn export_is_neutralised_audited_and_scoped() {
    let Some(db) = scratch::database("export_is_neutralised_audited_and_scoped").await else { return };
    let f = fixture(&db).await;
    // A title attribute so CI names can hold formulas.
    let (status, v, _) = call(
        &f.app,
        "POST",
        "/api/v1/attribute-definitions",
        &f.session,
        Some(json!({ "classId": f.classes["app"], "key": "name", "label": "Name", "dataType": "text" })),
    )
    .await;
    assert_eq!(status, 201, "{v}");
    let name_attr = v["id"].as_str().unwrap().to_owned();
    let (status, v, _) = call(
        &f.app,
        "PATCH",
        &format!("/api/v1/ci-classes/{}", f.classes["app"]),
        &f.session,
        Some(json!({ "titleAttributeId": name_attr })),
    )
    .await;
    assert_eq!(status, 200, "{v}");
    let mut named = Vec::new();
    for name in ["root", "=HYPERLINK(\"http://x\")", "+cmd", "-2", "@SUM(A1)", "plain, \"quoted\""] {
        let body = json!({ "classId": f.classes["app"], "attributes": { "name": name } });
        let (status, v, _) = call(&f.app, "POST", "/api/v1/configuration-items", &f.session, Some(body)).await;
        assert_eq!(status, 201, "{v}");
        named.push(v["id"].as_str().unwrap().parse::<Uuid>().unwrap());
    }
    let root = named[0];
    for c in &named[1..] {
        f.affects(root, *c).await;
    }
    let secret = f.ci("secret").await;
    f.affects(root, secret).await;

    let path = format!("/api/v1/configuration-items/{root}/impact/export?depth=2");
    let (status, body, headers) = raw(&f.app, &path, &f.session).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(headers[header::CONTENT_TYPE], "text/csv; charset=utf-8");
    assert_eq!(headers[header::CACHE_CONTROL], "no-store");
    let disposition = headers[header::CONTENT_DISPOSITION].to_str().unwrap();
    assert!(
        disposition.starts_with("attachment; filename=\"impact-CI-") && disposition.contains("-downstream-"),
        "{disposition}"
    );
    let lines: Vec<&str> = body.split("\r\n").filter(|l| !l.is_empty()).collect();
    assert!(lines[0].starts_with("\"# Impact analysis of CI-"), "{}", lines[0]);
    assert_eq!(
        lines[1],
        "\"ci_id\",\"ident\",\"name\",\"class\",\"criticality\",\"direction\",\"hops\",\"via_relationship\",\"via_ci_ident\",\"path_idents\",\"active\",\"status\""
    );
    assert_eq!(lines.len(), 2 + 6);
    assert!(body.contains("\"'=HYPERLINK(\"\"http://x\"\")\""), "{body}");
    for cell in ["\"'+cmd\"", "\"'-2\"", "\"'@SUM(A1)\"", "\"plain, \"\"quoted\"\"\""] {
        assert!(body.contains(cell), "{cell} in {body}");
    }
    // Every field is quoted.
    for line in &lines[1..] {
        assert!(line.starts_with('"') && line.ends_with('"'), "{line}");
    }

    let audit: Vec<(String, Uuid, Value)> =
        sqlx::query_as("SELECT entity_type, entity_id, new_value FROM audit_log WHERE action = 'export' ORDER BY id")
            .fetch_all(&f.pool)
            .await
            .unwrap();
    assert_eq!(audit.len(), 1);
    let (entity_type, entity_id, value) = &audit[0];
    assert_eq!((entity_type.as_str(), *entity_id), ("configuration_items", root));
    assert_eq!(value["kind"], "impact");
    assert_eq!(value["rowCount"], 6);
    assert_eq!(value["parameters"]["depth"], 2);
    assert!(!value.to_string().contains("HYPERLINK"), "no row data: {value}");

    // A restricted export is the restricted JSON result as CSV.
    let restricted = viewer(&[f.classes["app"]]);
    let json = run(&f, &restricted, root, &down(2)).await;
    let csv = service::csv_of(&f.pool, &restricted, root, &down(2)).await;
    let csv_ids: Vec<String> =
        csv.split("\r\n").skip(2).filter(|l| !l.is_empty()).map(|l| l[1..37].to_owned()).collect();
    let json_ids: Vec<String> = json.items.iter().map(|i| i.id.to_string()).collect();
    assert_eq!(csv_ids, json_ids);
    assert!(!csv.contains(&secret.to_string()));
    assert!(csv.lines().next().unwrap().contains("only CIs of classes you are allowed to view"));
    db.drop().await;
}

/// The routes: 404 for a hidden or missing root with the same body shape, 400
/// for bad parameters, the settings endpoint, and 401 without a session.
#[tokio::test]
async fn routes_answer_with_the_standard_envelope() {
    let Some(db) = scratch::database("impact_routes_answer_with_the_standard_envelope").await else { return };
    let f = fixture(&db).await;
    let root = f.ci("app").await;
    let a = f.ci("app").await;
    f.affects(root, a).await;

    let (status, v, _) =
        call(&f.app, "GET", &format!("/api/v1/configuration-items/{root}/impact"), &f.session, None).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(v["items"][0]["id"], json!(a));
    assert_eq!(v["parameters"]["depth"], 3);
    assert_eq!(v["parameters"]["maxNodes"], 500);
    assert_eq!(v["visibility"], "all_classes");
    for key in ["root", "summary", "truncated", "truncatedReason", "hasMoreBeyondDepth", "limits", "elapsedMs"] {
        assert!(v.get(key).is_some(), "{key}");
    }
    let (status, v, _) =
        call(&f.app, "GET", &format!("/api/v1/configuration-items/{root}/impact?depth=0"), &f.session, None).await;
    assert_eq!((status, v["error"]["code"].as_str()), (400, Some("VALIDATION_ERROR")));
    let (status, v, _) =
        call(&f.app, "GET", &format!("/api/v1/configuration-items/{root}/impact?depth=11"), &f.session, None).await;
    assert_eq!((status, v["error"]["details"][0]["field"].as_str()), (400, Some("depth")), "{v}");
    let missing = Uuid::new_v4();
    let (status, v, _) =
        call(&f.app, "GET", &format!("/api/v1/configuration-items/{missing}/impact"), &f.session, None).await;
    assert_eq!((status, v["error"]["code"].as_str()), (404, Some("NOT_FOUND")));
    let (status, _, _) =
        call(&f.app, "GET", &format!("/api/v1/configuration-items/{root}/impact"), &Creds::default(), None).await;
    assert_eq!(status, 401);

    let (status, v, _) = call(&f.app, "GET", "/api/v1/settings/impact", &f.session, None).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(
        v,
        json!({ "maxDepth": 10, "maxNodesLimit": 2000, "defaultDepth": 3, "defaultMaxNodes": 500, "timeoutMs": 5000,
                "anyTypePropagates": true })
    );
    sqlx::query("UPDATE relationship_types SET impact_direction = 'none'").execute(&f.pool).await.unwrap();
    let (_, v, _) = call(&f.app, "GET", "/api/v1/settings/impact", &f.session, None).await;
    assert_eq!(v["anyTypePropagates"], false);
    db.drop().await;
}

/// A user runs at most IMPACT_MAX_CONCURRENT_PER_USER analyses at once (429),
/// the process IMPACT_MAX_CONCURRENT (503); places come back when done.
#[test]
fn concurrent_analyses_are_capped_per_user_and_per_process() {
    let state =
        ImpactState::new(ImpactConfig { max_concurrent: 3, max_concurrent_per_user: 2, ..ImpactConfig::default() });
    let (alice, bob) = (viewer(&[]), viewer(&[]));
    let one = state.acquire(&alice).unwrap();
    let two = state.acquire(&alice).unwrap();
    let err = state.acquire(&alice).err().unwrap();
    assert_eq!((err.code, err.retry_after), (ErrorCode::RateLimited, Some(1)));
    let three = state.acquire(&bob).unwrap();
    let err = state.acquire(&bob).err().unwrap();
    assert_eq!(err.code, ErrorCode::ServerBusy);
    // Refused by the process cap, bob's place went back: after one finishes he still has two.
    drop(one);
    let four = state.acquire(&bob).unwrap();
    assert_eq!(state.acquire(&bob).err().unwrap().code, ErrorCode::RateLimited);
    drop((two, three, four));
    let _a = state.acquire(&alice).unwrap();
    let _b = state.acquire(&alice).unwrap();
}

/// Concurrency through the service: a user's third analysis while two run is refused.
#[tokio::test]
async fn a_third_concurrent_analysis_of_one_user_is_refused() {
    let Some(db) = scratch::database("a_third_concurrent_analysis_of_one_user_is_refused").await else { return };
    let f = fixture(&db).await;
    let root = f.ci("app").await;
    let state = Arc::new(ImpactState::default());
    let user = viewer(&[f.classes["app"]]);
    let _running = (state.acquire(&user).unwrap(), state.acquire(&user).unwrap());
    let err = service::analyse(&f.pool, &user, &state, root, &down(1)).await.err().unwrap();
    assert_eq!(err.code, ErrorCode::RateLimited);
    // Another user is not affected.
    assert!(service::analyse(&f.pool, &viewer(&[f.classes["app"]]), &state, root, &down(1)).await.is_ok());
    db.drop().await;
}

/// Upgrade (decision D2): 0029 sets target_to_source on runs_on, depends_on
/// and located_in only while their labels are the starter template's; a
/// repurposed key and every other type stay none. 0030 seeds the criticality
/// list next to an existing list keyed "criticality".
#[tokio::test]
async fn the_upgrade_seeds_impact_only_for_unchanged_starter_types() {
    let Some(db) = scratch::empty("the_upgrade_seeds_impact_only_for_unchanged_starter_types").await else { return };
    let before = sqlx::migrate::Migrator {
        migrations: std::borrow::Cow::Owned(crate::db::MIGRATOR.iter().filter(|m| m.version <= 28).cloned().collect()),
        table_name: std::borrow::Cow::Borrowed("public._sqlx_migrations"),
        ..sqlx::migrate!("../sql/migrations")
    };
    before.run(&db.pool).await.unwrap();
    sqlx::query(
        "INSERT INTO relationship_types (key, name, forward_label, reverse_label, is_directional) VALUES
           ('runs_on', 'Runs on', 'runs on', 'hosts', true),
           ('depends_on', 'Depends on', 'needs', 'is needed by', true),
           ('located_in', 'Located in', 'is located in', 'contains', true),
           ('connected_to', 'Connected to', 'is connected to', 'is connected to', false),
           ('backs_up', 'Backs up', 'backs up', 'is backed up by', true)",
    )
    .execute(&db.pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO lookup_lists (key, name) VALUES ('criticality', 'Our own criticality')")
        .execute(&db.pool)
        .await
        .unwrap();
    let stamped: Vec<(String, chrono::DateTime<chrono::Utc>)> =
        sqlx::query_as("SELECT key, updated_at FROM relationship_types ORDER BY key")
            .fetch_all(&db.pool)
            .await
            .unwrap();

    crate::db::MIGRATOR.run(&db.pool).await.unwrap();
    let rows: Vec<(String, String, chrono::DateTime<chrono::Utc>)> =
        sqlx::query_as("SELECT key, impact_direction, updated_at FROM relationship_types ORDER BY key")
            .fetch_all(&db.pool)
            .await
            .unwrap();
    let got: Vec<(&str, &str)> = rows.iter().map(|(k, d, _)| (k.as_str(), d.as_str())).collect();
    assert_eq!(
        got,
        [
            ("backs_up", "none"),
            ("connected_to", "none"),
            ("depends_on", "none"),
            ("located_in", "target_to_source"),
            ("runs_on", "target_to_source"),
        ]
    );
    // Not an edit: updated_at is kept.
    assert_eq!(rows.iter().map(|(k, _, u)| (k.clone(), *u)).collect::<Vec<_>>(), stamped);

    let lists: Vec<(String, Option<String>)> =
        sqlx::query_as("SELECT key, system_role FROM lookup_lists ORDER BY key").fetch_all(&db.pool).await.unwrap();
    assert_eq!(lists, [("criticality".to_owned(), None), ("criticality_2".to_owned(), Some("criticality".to_owned()))]);
    let values: Vec<String> = sqlx::query_scalar(
        "SELECT v.key FROM lookup_list_values v JOIN lookup_lists l ON l.id = v.list_id
         WHERE l.system_role = 'criticality' ORDER BY v.sort_order",
    )
    .fetch_all(&db.pool)
    .await
    .unwrap();
    assert_eq!(values, ["critical", "high", "medium", "low"]);
    // The system list stays; its role cannot move.
    let err = sqlx::query("DELETE FROM lookup_lists WHERE key = 'criticality_2'").execute(&db.pool).await.unwrap_err();
    assert_eq!(err.as_database_error().unwrap().constraint(), Some("lookup_lists_system_protected"));
    let err = sqlx::query("UPDATE lookup_lists SET system_role = 'criticality' WHERE key = 'criticality'")
        .execute(&db.pool)
        .await
        .unwrap_err();
    assert_eq!(err.as_database_error().unwrap().constraint(), Some("lookup_lists_system_protected"));
    sqlx::query("DELETE FROM lookup_lists WHERE key = 'criticality'").execute(&db.pool).await.unwrap();
    // A CI's criticality must be a value of the system list.
    let other: Uuid = sqlx::query_scalar("INSERT INTO lookup_lists (key, name) VALUES ('tiers', 'Tiers') RETURNING id")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    let tier: Uuid = sqlx::query_scalar(
        "INSERT INTO lookup_list_values (list_id, key, name) VALUES ($1, 'gold', 'Gold') RETURNING id",
    )
    .bind(other)
    .fetch_one(&db.pool)
    .await
    .unwrap();
    let class: Uuid = sqlx::query_scalar(
        "WITH a AS (INSERT INTO areas (key, name) VALUES ('things', 'Things') RETURNING id)
         INSERT INTO ci_classes (key, name, area_id) SELECT 'thing', 'Thing', id FROM a RETURNING id",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    let err = sqlx::query(
        "INSERT INTO configuration_items (class_id, ident, label, criticality_value_id) VALUES ($1, 'X-1', 'x', $2)",
    )
    .bind(class)
    .bind(tier)
    .execute(&db.pool)
    .await
    .unwrap_err();
    assert_eq!(err.as_database_error().unwrap().constraint(), Some("configuration_items_criticality_list"));
    db.drop().await;
}
