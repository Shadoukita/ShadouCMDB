//! Business services against a real PostgreSQL (SHAA-927 §7.1, §7.2).
//!
//! The §7.2 oracles build the same world twice, once with a member the
//! restricted user may not view (`db-01`) and once without it (the control),
//! and compare every answer the restricted user gets. Generated values differ
//! between two databases, so before comparing, the known ids are replaced by
//! the names of what they identify (the control uses a fresh id where the
//! other world uses `db-01`'s), other ids by their order of first appearance,
//! audit row numbers by their order, and timestamps, `requestId` and
//! `elapsedMs` are blanked. Everything else must be byte-identical.

use std::collections::HashMap;

use axum::Router;
use axum::body::Body as HttpBody;
use axum::http::{HeaderMap, Request, header};
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

use crate::api::context::RequestContext;
use crate::config::BusinessServiceConfig;
use crate::db::scratch;
use crate::modules::api_tokens::tests::{Creds, app_with_business_services, call, session_of};

// ---------------------------------------------------------------------------
// Fixture
// ---------------------------------------------------------------------------

/// The business service class's table: `migrate` creates it with the
/// reconcile it runs after the migrations, which a scratch database skips.
async fn reconcile(pool: &PgPool) {
    let ctx = RequestContext::system("test", "test");
    let mut tx = pool.begin().await.unwrap();
    crate::schema::reconcile(&mut tx, &ctx, "test").await.unwrap_or_else(|e| panic!("reconcile: {}", e.message));
    tx.commit().await.unwrap();
}

struct World {
    app: Router,
    admin: Creds,
    pool: PgPool,
    service_class: Uuid,
    member_type: Uuid,
    /// Class ids by key
    classes: HashMap<&'static str, Uuid>,
    /// CI ids by ident
    cis: HashMap<String, Uuid>,
    /// A relationship type that propagates impact target_to_source (runs on)
    runs_on: Uuid,
}

async fn raw(app: &Router, method: &str, path: &str, creds: &Creds, body: Option<Value>) -> (u16, HeaderMap, String) {
    let mut req = Request::builder().method(method).uri(path).header(header::USER_AGENT, "services-test");
    if let Some(c) = &creds.cookie {
        req = req.header(header::COOKIE, c);
    }
    if let Some(c) = &creds.csrf {
        req = req.header("x-csrf-token", c);
    }
    if let Some(b) = &creds.bearer {
        req = req.header(header::AUTHORIZATION, format!("Bearer {b}"));
    }
    let req = match body {
        Some(b) => req.header(header::CONTENT_TYPE, "application/json").body(HttpBody::from(b.to_string())),
        None => req.body(HttpBody::empty()),
    };
    let res = app.clone().oneshot(req.unwrap()).await.unwrap();
    let status = res.status().as_u16();
    let headers = res.headers().clone();
    let bytes = axum::body::to_bytes(res.into_body(), 16 << 20).await.unwrap();
    (status, headers, String::from_utf8(bytes.to_vec()).unwrap())
}

impl World {
    async fn new(db: &scratch::Scratch, limits: BusinessServiceConfig) -> World {
        let pool = db.pool.clone();
        reconcile(&pool).await;
        let app = app_with_business_services(pool.clone(), limits);
        let setup = json!({ "username": "admin", "displayName": "Admin", "password": "correct horse battery", "setupToken": crate::auth::setup_token::TEST_TOKEN });
        let (status, me, headers) = call(&app, "POST", "/api/v1/setup", &Creds::default(), Some(setup)).await;
        assert_eq!(status, 201, "{me}");
        let admin = session_of(&me, &headers);
        let (service_class, member_type): (Uuid, Uuid) = sqlx::query_as(
            "SELECT (SELECT id FROM ci_classes WHERE system_role = 'business_service'),
                    (SELECT id FROM relationship_types WHERE system_role = 'business_service_member')",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        let runs_on: Uuid = sqlx::query_scalar(
            "INSERT INTO relationship_types (key, name, forward_label, reverse_label, impact_direction)
             VALUES ('runs_on', 'Runs on', 'runs on', 'hosts', 'target_to_source') RETURNING id",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        let mut w = World {
            app,
            admin,
            pool,
            service_class,
            member_type,
            classes: HashMap::from([("business_service", service_class)]),
            cis: HashMap::new(),
            runs_on,
        };
        // datastore > database: a grant on datastore says nothing about database (oracle 11).
        w.class("server", None).await;
        w.class("datastore", None).await;
        w.class("database", Some("datastore")).await;
        sqlx::query(
            "INSERT INTO relationship_type_rules (relationship_type_id, source_class_id, target_class_id)
             SELECT $1, a.id, b.id FROM ci_classes a, ci_classes b WHERE a.parent_id IS NULL AND b.parent_id IS NULL",
        )
        .bind(runs_on)
        .execute(&w.pool)
        .await
        .unwrap();
        w
    }

    /// A class with a required text field "name" as its title.
    async fn class(&mut self, key: &'static str, parent: Option<&str>) -> Uuid {
        let mut body = json!({ "key": key, "name": key });
        if let Some(p) = parent {
            body["parentId"] = json!(self.classes[p]);
        }
        let (status, v, _) = call(&self.app, "POST", "/api/v1/ci-classes", &self.admin, Some(body)).await;
        assert_eq!(status, 201, "{v}");
        let id: Uuid = v["id"].as_str().unwrap().parse().unwrap();
        self.classes.insert(key, id);
        if parent.is_none() {
            let field = json!({ "classId": id, "key": "name", "label": "Name", "dataType": "text" });
            let (status, f, _) =
                call(&self.app, "POST", "/api/v1/attribute-definitions", &self.admin, Some(field)).await;
            assert_eq!(status, 201, "{f}");
            let patch = json!({ "titleAttributeId": f["id"] });
            let (status, v, _) =
                call(&self.app, "PATCH", &format!("/api/v1/ci-classes/{id}"), &self.admin, Some(patch)).await;
            assert_eq!(status, 200, "{v}");
        }
        id
    }

    /// A CI with this ident and name.
    async fn ci(&mut self, class: &str, ident: &str, name: &str) -> Uuid {
        self.ci_with(class, ident, json!({ "attributes": { "name": name } })).await
    }

    async fn ci_with(&mut self, class: &str, ident: &str, extra: Value) -> Uuid {
        let mut body = json!({ "classId": self.classes[class], "ident": ident });
        for (k, v) in extra.as_object().unwrap() {
            body[k] = v.clone();
        }
        let (status, v, _) = call(&self.app, "POST", "/api/v1/configuration-items", &self.admin, Some(body)).await;
        assert_eq!(status, 201, "{v}");
        let id: Uuid = v["id"].as_str().unwrap().parse().unwrap();
        self.cis.insert(ident.to_owned(), id);
        id
    }

    fn id(&self, ident: &str) -> Uuid {
        self.cis[ident]
    }

    async fn add(&self, creds: &Creds, service: &str, members: &[&str]) -> (u16, Value) {
        let ids: Vec<Uuid> = members.iter().map(|m| self.cis.get(*m).copied().unwrap_or_else(Uuid::new_v4)).collect();
        let path = format!("/api/v1/business-services/{}/members", self.id(service));
        let (status, v, _) = call(&self.app, "POST", &path, creds, Some(json!({ "memberIds": ids }))).await;
        (status, v)
    }

    async fn remove(&self, creds: &Creds, service: &str, members: &[&str]) -> (u16, Value) {
        let ids: Vec<Uuid> = members.iter().map(|m| self.id(m)).collect();
        let path = format!("/api/v1/business-services/{}/members/remove", self.id(service));
        let (status, v, _) = call(&self.app, "POST", &path, creds, Some(json!({ "memberIds": ids }))).await;
        (status, v)
    }

    async fn get(&self, creds: &Creds, path: &str) -> (u16, Value) {
        let (status, v, _) = call(&self.app, "GET", path, creds, None).await;
        (status, v)
    }

    /// `source -runs_on-> target` (target fails, source is affected).
    async fn runs_on(&self, source: &str, target: &str) {
        sqlx::query(
            "INSERT INTO ci_relationships (relationship_type_id, source_ci_id, target_ci_id) VALUES ($1, $2, $3)",
        )
        .bind(self.runs_on)
        .bind(self.id(source))
        .bind(self.id(target))
        .execute(&self.pool)
        .await
        .unwrap();
    }

    /// A profile with these class rights (view, edit) and global rights, and a signed-in user holding it.
    async fn user(&self, name: &str, grants: &[(&str, bool)], global: &[&str]) -> (Uuid, Creds) {
        let profile: Uuid = sqlx::query_scalar("INSERT INTO permission_profiles (name) VALUES ($1) RETURNING id")
            .bind(format!("profile {name}"))
            .fetch_one(&self.pool)
            .await
            .unwrap();
        for (class, edit) in grants {
            sqlx::query(
                "INSERT INTO permission_profile_class_permissions (profile_id, class_id, can_view, can_edit)
                 VALUES ($1, $2, true, $3)",
            )
            .bind(profile)
            .bind(self.classes[class])
            .bind(edit)
            .execute(&self.pool)
            .await
            .unwrap();
        }
        for g in global {
            sqlx::query("INSERT INTO permission_profile_global_permissions (profile_id, permission) VALUES ($1, $2)")
                .bind(profile)
                .bind(g)
                .execute(&self.pool)
                .await
                .unwrap();
        }
        let body = json!({ "username": name, "displayName": format!("User {name}"), "password": "a long enough password", "profileIds": [profile] });
        let (status, v, _) = call(&self.app, "POST", "/api/v1/admin/users", &self.admin, Some(body)).await;
        assert_eq!(status, 201, "{v}");
        let login = json!({ "username": name, "password": "a long enough password" });
        let (status, me, headers) = call(&self.app, "POST", "/api/v1/auth/login", &Creds::default(), Some(login)).await;
        assert_eq!(status, 200, "{me}");
        (v["id"].as_str().unwrap().parse().unwrap(), session_of(&me, &headers))
    }
}

fn code(v: &Value) -> &str {
    v["error"]["code"].as_str().unwrap_or_default()
}

fn details(v: &Value) -> Vec<(String, String)> {
    v["error"]["details"]
        .as_array()
        .map(|d| {
            d.iter()
                .map(|e| (e["field"].as_str().unwrap().to_owned(), e["code"].as_str().unwrap().to_owned()))
                .collect()
        })
        .unwrap_or_default()
}

fn idents(list: &Value) -> Vec<String> {
    list["data"].as_array().unwrap().iter().map(|m| m["ci"]["ident"].as_str().unwrap().to_owned()).collect()
}

// ---------------------------------------------------------------------------
// §7.2 Restricted-user oracles (release gate)
// ---------------------------------------------------------------------------

/// Blanks what legitimately differs between two databases (see the module docs).
struct Normaliser {
    names: Vec<(String, String)>,
}

impl Normaliser {
    fn new(w: &World, stand_ins: &HashMap<&str, Uuid>) -> Self {
        let mut names: Vec<(String, String)> =
            w.cis.iter().map(|(ident, id)| (id.to_string(), format!("<ci:{ident}>"))).collect();
        names.extend(w.classes.iter().map(|(k, id)| (id.to_string(), format!("<class:{k}>"))));
        names.extend(stand_ins.iter().map(|(k, id)| (id.to_string(), format!("<ci:{k}>"))));
        names.push((w.member_type.to_string(), "<member-type>".into()));
        names.push((w.runs_on.to_string(), "<runs-on>".into()));
        Normaliser { names }
    }

    fn text(&self, s: &str) -> String {
        let mut s = s.to_owned();
        for (from, to) in &self.names {
            s = s.replace(from.as_str(), to);
        }
        let ts = regex::Regex::new(r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(\.\d+)?Z").unwrap();
        s = ts.replace_all(&s, "<ts>").into_owned();
        let file_ts = regex::Regex::new(r"-\d{8}-\d{4}\.csv").unwrap();
        s = file_ts.replace_all(&s, "-<ts>.csv").into_owned();
        let uuid = regex::Regex::new(r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}").unwrap();
        let mut seen: Vec<String> = Vec::new();
        uuid.replace_all(&s, |c: &regex::Captures| {
            let id = c[0].to_owned();
            let n = seen.iter().position(|x| *x == id).unwrap_or_else(|| {
                seen.push(id);
                seen.len() - 1
            });
            format!("<id#{n}>")
        })
        .into_owned()
    }

    fn json(&self, v: &Value) -> String {
        fn strip(v: &mut Value, audit_ids: &mut Vec<i64>) {
            match v {
                Value::Object(m) => {
                    for k in ["requestId", "elapsedMs"] {
                        if m.contains_key(k) {
                            m.insert(k.into(), Value::Null);
                        }
                    }
                    // Audit rows: `id` is the log's sequence number.
                    if m.contains_key("occurredAt")
                        && let Some(n) = m.get("id").and_then(Value::as_i64)
                    {
                        audit_ids.push(n);
                        m.insert("id".into(), json!(format!("<row#{}>", audit_ids.len() - 1)));
                    }
                    m.values_mut().for_each(|x| strip(x, audit_ids));
                }
                Value::Array(a) => a.iter_mut().for_each(|x| strip(x, audit_ids)),
                _ => {}
            }
        }
        let mut v = v.clone();
        strip(&mut v, &mut Vec::new());
        self.text(&v.to_string())
    }
}

/// One world of §7.2: profile R may view and edit services, view servers and
/// the datastore class (not its subclass database); S includes web-01 and
/// web-02 and, unless `control`, db-01.
struct Oracle {
    w: World,
    /// db-01, or in the control world an id that names nothing
    db01: Uuid,
    r: Creds,
    /// Without view on the service class.
    plain: Creds,
    norm: Normaliser,
}

async fn oracle_world(db: &scratch::Scratch, control: bool) -> Oracle {
    let mut w = World::new(db, BusinessServiceConfig { max_members: 3, max_nesting: 5 }).await;
    w.ci("business_service", "S", "Online shop").await;
    w.ci("business_service", "T", "Outer").await;
    for ident in ["web-01", "web-02", "web-03", "web-04", "host-01", "san-01"] {
        w.ci("server", ident, ident).await;
    }
    let mut stand_ins = HashMap::new();
    if control {
        stand_ins.insert("db-01", Uuid::new_v4());
    } else {
        w.ci("database", "db-01", "db-01").await;
    }
    let admin = w.admin.clone();
    // web-01 and db-01 in one request; db-01 removed and added alone; then web-02.
    let first: &[&str] = if control { &["web-01"] } else { &["web-01", "db-01"] };
    assert_eq!(w.add(&admin, "S", first).await.0, 200);
    if !control {
        assert_eq!(w.remove(&admin, "S", &["db-01"]).await.0, 204);
        assert_eq!(w.add(&admin, "S", &["db-01"]).await.0, 200);
    }
    assert_eq!(w.add(&admin, "S", &["web-02"]).await.0, 200);
    assert_eq!(w.add(&admin, "T", &["S"]).await.0, 200);
    // Impact: host-01 <- web-01 (S), san-01 <- db-01 (S).
    w.runs_on("web-01", "host-01").await;
    if !control {
        w.runs_on("db-01", "san-01").await;
    }
    let (_, r) =
        w.user("r", &[("business_service", true), ("server", false), ("datastore", false)], &["audit.view"]).await;
    let (_, plain) = w.user("plain", &[("server", false)], &[]).await;
    let norm = Normaliser::new(&w, &stand_ins);
    let db01 = stand_ins.get("db-01").copied().unwrap_or_else(|| w.id("db-01"));
    Oracle { w, db01, r, plain, norm }
}

impl Oracle {
    fn db01(&self) -> Uuid {
        self.db01
    }

    /// An administrator's API token that may view and edit every class.
    async fn admin_token(&self) -> Creds {
        let profile: Uuid = sqlx::query_scalar("INSERT INTO permission_profiles (name) VALUES ('token') RETURNING id")
            .fetch_one(&self.w.pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO permission_profile_class_permissions (profile_id, class_id, can_view, can_edit)
             VALUES ($1, NULL, true, true)",
        )
        .bind(profile)
        .execute(&self.w.pool)
        .await
        .unwrap();
        let expires = (chrono::Utc::now() + chrono::Duration::days(30)).to_rfc3339();
        let body = json!({ "name": "script", "profileId": profile, "expiresAt": expires });
        let (status, v, _) = call(&self.w.app, "POST", "/api/v1/admin/api-tokens", &self.w.admin, Some(body)).await;
        assert_eq!(status, 201, "{v}");
        Creds { bearer: v["secret"].as_str().map(str::to_owned), ..Creds::default() }
    }

    /// The restricted user's answer to a request, normalised.
    async fn ask(&self, creds: &Creds, method: &str, path: &str, body: Option<Value>) -> String {
        let (status, headers, text) = raw(&self.w.app, method, path, creds, body).await;
        let ct = headers.get(header::CONTENT_TYPE).map(|v| v.to_str().unwrap().to_owned()).unwrap_or_default();
        let shown = if ct.starts_with("application/json") {
            self.norm.json(&serde_json::from_str::<Value>(&text).unwrap())
        } else {
            let disposition =
                headers.get(header::CONTENT_DISPOSITION).map(|v| v.to_str().unwrap().to_owned()).unwrap_or_default();
            self.norm.text(&format!("{ct}\n{disposition}\n{text}"))
        };
        format!("{status} {shown}")
    }
}

/// §7.2: every answer the restricted user gets is the same whether or not S
/// has a member they may not view.
#[tokio::test]
async fn restricted_user_oracles() {
    let Some(db_a) = scratch::database("restricted_user_oracles_hidden").await else { return };
    let Some(db_b) = scratch::database("restricted_user_oracles_control").await else { return };
    let a = oracle_world(&db_a, false).await;
    let b = oracle_world(&db_b, true).await;
    let s = |o: &Oracle| o.w.id("S");

    // Same request on both worlds; `path` gets each world's ids.
    macro_rules! same {
        ($label:expr, $creds:ident, $method:expr, $path:expr, $body:expr) => {{
            let answer_a = a.ask(&a.$creds, $method, &$path(&a), $body(&a)).await;
            let answer_b = b.ask(&b.$creds, $method, &$path(&b), $body(&b)).await;
            assert_eq!(answer_a, answer_b, "oracle {}", $label);
            answer_a
        }};
    }
    let none = |_: &Oracle| None::<Value>;

    // 1. List and detail: memberCount 2, visibility restricted.
    let list = same!("1 list", r, "GET", |_: &Oracle| "/api/v1/business-services?sort=name".to_owned(), none);
    let detail = same!("1 detail", r, "GET", |o: &Oracle| format!("/api/v1/business-services/{}", s(o)), none);
    let detail: Value = serde_json::from_str(detail.split_once(' ').unwrap().1).unwrap();
    assert_eq!((detail["memberCount"].as_i64(), detail["visibility"].as_str()), (Some(2), Some("restricted")));
    assert!(list.contains("\"memberCount\":2") && list.contains("\"visibility\":\"restricted\""), "{list}");
    // The administrator sees all three on the hidden world.
    let (_, full) = a.w.get(&a.w.admin, &format!("/api/v1/business-services/{}", s(&a))).await;
    assert_eq!(full["memberCount"], 3);

    // 2. Members: two rows, total 2; db-01 absent from a ciId probe.
    let members =
        same!("2 members", r, "GET", |o: &Oracle| format!("/api/v1/business-services/{}/members", s(o)), none);
    let members: Value = serde_json::from_str(members.split_once(' ').unwrap().1).unwrap();
    assert_eq!((members["data"].as_array().unwrap().len(), members["page"]["total"].as_i64()), (2, Some(2)));
    let probe = same!(
        "2 ciId",
        r,
        "GET",
        |o: &Oracle| format!("/api/v1/business-services/{}/members?ciId={},{}", s(o), o.db01(), o.w.id("web-01")),
        none
    );
    assert!(probe.contains("\"total\":1"), "{probe}");

    // 3. Adding db-01 is refused like a random id, and so is a batch with it.
    let random = Uuid::new_v4();
    let add_hidden = same!(
        "3 add hidden",
        r,
        "POST",
        |o: &Oracle| format!("/api/v1/business-services/{}/members", s(o)),
        |o: &Oracle| Some(json!({ "memberIds": [o.db01()] }))
    );
    let add_random = a
        .ask(
            &a.r,
            "POST",
            &format!("/api/v1/business-services/{}/members", s(&a)),
            Some(json!({ "memberIds": [random] })),
        )
        .await;
    assert_eq!(add_hidden, add_random, "oracle 3: a hidden id answers like a random one");
    assert!(add_hidden.starts_with("400 ") && add_hidden.contains("\"code\":\"not_found\""), "{add_hidden}");
    let batch = same!(
        "3 batch",
        r,
        "POST",
        |o: &Oracle| format!("/api/v1/business-services/{}/members", s(o)),
        |o: &Oracle| Some(json!({ "memberIds": [o.db01(), o.w.id("web-03")] }))
    );
    assert!(batch.starts_with("400 ") && batch.contains("memberIds[0]") && !batch.contains("memberIds[1]"), "{batch}");
    let (_, after) = a.w.get(&a.w.admin, &format!("/api/v1/business-services/{}", s(&a))).await;
    assert_eq!(after["memberCount"], 3, "nothing was written");

    // 4. Removing db-01 answers like removing a non-member.
    let del_hidden = same!(
        "4 delete hidden",
        r,
        "DELETE",
        |o: &Oracle| format!("/api/v1/business-services/{}/members/{}", s(o), o.db01()),
        none
    );
    let del_non_member =
        a.ask(&a.r, "DELETE", &format!("/api/v1/business-services/{}/members/{}", s(&a), a.w.id("web-03")), None).await;
    assert_eq!(del_hidden.replace("<ci:db-01>", "<x>"), del_non_member.replace("<ci:web-03>", "<x>"), "oracle 4");
    assert!(del_hidden.starts_with("404 "), "{del_hidden}");

    // 6. The CSV export (before 5 changes the members).
    let csv =
        same!("6 export", r, "GET", |o: &Oracle| format!("/api/v1/business-services/{}/members/export", s(o)), none);
    assert!(csv.contains("not allowed to view") && !csv.contains("db-01"), "{csv}");

    // 7. "Part of" for web-01, and for a user who may not view services.
    let part_of = same!(
        "7 part of",
        r,
        "GET",
        |o: &Oracle| format!("/api/v1/configuration-items/{}/business-services", o.w.id("web-01")),
        none
    );
    assert!(part_of.contains("<ci:S>") && part_of.contains("<ci:T>"), "{part_of}");
    let plain = same!(
        "7 no service view",
        plain,
        "GET",
        |o: &Oracle| format!("/api/v1/configuration-items/{}/business-services", o.w.id("web-01")),
        none
    );
    assert!(plain.starts_with("200 ") && plain.contains("\"data\":[]"), "{plain}");

    // 8. Impact: S is reached through web-01, never through db-01 alone.
    let via_hidden = same!(
        "8 impact via db-01",
        r,
        "GET",
        |o: &Oracle| format!("/api/v1/configuration-items/{}/impact?depth=3", o.w.id("san-01")),
        none
    );
    assert!(!via_hidden.contains("<ci:S>"), "{via_hidden}");
    let via_web = same!(
        "8 impact via web-01",
        r,
        "GET",
        |o: &Oracle| format!("/api/v1/configuration-items/{}/impact?depth=3", o.w.id("host-01")),
        none
    );
    assert!(via_web.contains("<ci:S>") && via_web.contains("<ci:T>"), "{via_web}");

    // 9. History: the membership rows show only web-01 / web-02; none names db-01.
    let history =
        same!("9 history", r, "GET", |o: &Oracle| format!("/api/v1/audit-log?entityId={}&sort=occurredAt", s(o)), none);
    assert!(history.contains("\"added\":[\"<ci:web-01>\"]") && !history.contains("db-01"), "{history}");
    let edges = same!(
        "9 relationship rows",
        r,
        "GET",
        |_: &Oracle| "/api/v1/audit-log?entityType=ci_relationships&sort=occurredAt".to_owned(),
        none
    );
    assert!(!edges.contains("db-01"), "{edges}");

    // 10. Audit ids (GH#378), compared raw rather than normalised: the rows R sees
    // from the first add (web-01 with db-01, or alone) are consecutive in the
    // stored sequence only in the control world, and R must not be able to tell.
    let mut consecutive = Vec::new();
    for o in [&a, &b] {
        let (_, log) =
            o.w.get(&o.r, &format!("/api/v1/audit-log?entityId={}&action=update&sort=occurredAt", s(o))).await;
        let request = log["data"][0]["requestId"].as_str().unwrap().to_owned();
        let path = format!("/api/v1/audit-log?requestId={request}&sort=occurredAt");
        let ids =
            |v: &Value| v["data"].as_array().unwrap().iter().map(|e| e["id"].as_i64().unwrap()).collect::<Vec<_>>();
        let (_, shown) = o.w.get(&o.r, &path).await;
        let (_, stored) = o.w.get(&o.w.admin, &path).await;
        let (shown, stored) = (ids(&shown), ids(&stored));
        assert_eq!(shown.len(), 2, "the service update and the web-01 edge");
        assert!(shown.iter().all(|id| !stored.contains(id)), "R gets no stored sequence number: {shown:?} {stored:?}");
        consecutive.push((stored.len(), shown.iter().max().unwrap() - shown.iter().min().unwrap() == 1));
    }
    assert_eq!(consecutive[0].0, consecutive[1].0 + 1, "{consecutive:?}: db-01's edge is a stored row of its own");
    assert_eq!(consecutive[0].1, consecutive[1].1, "oracle 10: R's ids say nothing about the gap");

    // 11. A grant on datastore gives nothing on database: db-01 stays hidden (all of the above).
    let (status, v) = a.w.get(&a.r, &format!("/api/v1/configuration-items/{}", a.db01())).await;
    assert_eq!((status, code(&v)), (404, "NOT_FOUND"));

    // 12. Changing S's class (GH#409), with create on the target class: refused the
    // same way with or without db-01, and validUntil = validFrom fails in the API,
    // so neither the type trigger nor the validity constraint is reached.
    for o in [&a, &b] {
        sqlx::query(
            "UPDATE permission_profile_class_permissions SET can_create = true
             WHERE class_id = $1 AND profile_id = (SELECT id FROM permission_profiles WHERE name = 'profile r')",
        )
        .bind(o.w.classes["server"])
        .execute(&o.w.pool)
        .await
        .unwrap();
    }
    let item = |o: &Oracle| format!("/api/v1/configuration-items/{}", s(o));
    let (from_a, from_b) = (a.w.get(&a.w.admin, &item(&a)).await.1, b.w.get(&b.w.admin, &item(&b)).await.1);
    let valid_from = |o: &Oracle| if std::ptr::eq(o, &a) { &from_a } else { &from_b }["validFrom"].clone();
    let retype = same!("12 retype", r, "PATCH", item, |o: &Oracle| Some(
        json!({ "classId": o.w.classes["server"], "attributes": {} })
    ));
    assert!(retype.starts_with("400 ") && retype.contains("\"business_service_class\""), "{retype}");
    let probe = same!("12 retype with an empty period", r, "PATCH", item, |o: &Oracle| Some(
        json!({ "classId": o.w.classes["server"], "validUntil": valid_from(o), "attributes": {} })
    ));
    assert!(probe.starts_with("400 ") && !probe.contains("configuration_items_"), "{probe}");
    let empty = same!("12 empty period", r, "PATCH", item, |o: &Oracle| Some(json!({ "validUntil": valid_from(o) })));
    assert!(empty.starts_with("400 ") && empty.contains("\"validUntil\"") && !empty.contains("configuration_items_"));
    let (_, after) = a.w.get(&a.w.admin, &item(&a)).await;
    assert_eq!(
        (&after["classId"], &after["validUntil"], &after["version"]),
        (&json!(a.w.service_class), &Value::Null, &from_a["version"])
    );

    // 5. The limit (3) counts visible members: R can add one more on both worlds, then no more.
    let add_one = same!(
        "5 add within limit",
        r,
        "POST",
        |o: &Oracle| format!("/api/v1/business-services/{}/members", s(o)),
        |o: &Oracle| Some(json!({ "memberIds": [o.w.id("web-03")] }))
    );
    assert!(add_one.starts_with("200 "), "{add_one}");
    let add_more = same!(
        "5 add beyond limit",
        r,
        "POST",
        |o: &Oracle| format!("/api/v1/business-services/{}/members", s(o)),
        |o: &Oracle| Some(json!({ "memberIds": [o.w.id("web-04")] }))
    );
    assert!(add_more.starts_with("400 ") && add_more.contains("member_limit"), "{add_more}");
    // The detail and list agree after the change.
    same!("5 detail", r, "GET", |o: &Oracle| format!("/api/v1/business-services/{}", s(o)), none);

    // 9. token.use: an administrator's token removes db-01, then web-01. The member
    // id in the recorded path is hidden on its own, as the service id is (GH#377).
    let mut token_use = Vec::new();
    for o in [&a, &b] {
        let token = o.admin_token().await;
        for member in [o.db01(), o.w.id("web-01")] {
            let path = format!("/api/v1/business-services/{}/members/{member}", s(o));
            raw(&o.w.app, "DELETE", &path, &token, None).await;
        }
        let log = "/api/v1/audit-log?entityType=api_tokens&action=token.use&sort=occurredAt";
        let prefix = &token.bearer.as_deref().unwrap()[..14];
        token_use.push(o.ask(&o.r, "GET", log, None).await.replace(prefix, "<prefix>"));
    }
    assert_eq!(token_use[0], token_use[1], "oracle 9 token.use");
    let paths = "/api/v1/business-services/<ci:S>/members/{hidden}\"";
    assert!(token_use[0].contains(paths) && token_use[0].contains("/members/<ci:web-01>\""), "{}", token_use[0]);
    assert!(!token_use[0].contains("db-01"), "{}", token_use[0]);
}

// ---------------------------------------------------------------------------
// §7.1 Membership
// ---------------------------------------------------------------------------

#[tokio::test]
async fn membership_adds_lists_and_removes() {
    let Some(db) = scratch::database("business_services_membership").await else { return };
    let mut w = World::new(&db, BusinessServiceConfig { max_members: 5_000, max_nesting: 2 }).await;
    let admin = w.admin.clone();
    for s in ["A", "B", "C", "D"] {
        w.ci("business_service", s, &format!("Service {s}")).await;
    }
    for i in 1..=4 {
        w.ci("server", &format!("web-0{i}"), &format!("web-0{i}")).await;
    }
    let a = w.id("A");

    // Add, alreadyMembers, list.
    let (status, v) = w.add(&admin, "A", &["web-01", "web-02"]).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(v["added"].as_array().unwrap().len(), 2);
    assert_eq!(v["added"][0]["ci"]["name"], "web-01");
    assert_eq!(v["added"][0]["isService"], false);
    let (status, v) = w.add(&admin, "A", &["web-02", "web-03"]).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(v["alreadyMembers"], json!([w.id("web-02")]));
    assert_eq!(v["added"].as_array().unwrap().len(), 1);
    let (_, list) = w.get(&admin, &format!("/api/v1/business-services/{a}/members?sort=-name")).await;
    assert_eq!(idents(&list), ["web-03", "web-02", "web-01"]);
    assert_eq!(list["visibility"], "all_classes");
    let (_, list) = w.get(&admin, &format!("/api/v1/business-services/{a}/members?limit=1&offset=1")).await;
    assert_eq!((idents(&list), list["page"]["total"].as_i64()), (vec!["web-02".to_owned()], Some(3)));

    // All or nothing: a mixed batch writes nothing.
    let (status, v) = w.add(&admin, "A", &["web-04", "nonexistent", "A"]).await;
    assert_eq!(status, 400, "{v}");
    assert_eq!(
        details(&v),
        [
            ("memberIds[1]".to_owned(), "not_found".to_owned()),
            ("memberIds[2]".to_owned(), "membership_self".to_owned())
        ]
    );
    let (_, d) = w.get(&admin, &format!("/api/v1/business-services/{a}")).await;
    assert_eq!(d["memberCount"], 3);

    // Nesting: B in A; A in B is a cycle; C in B makes the chain A > B > C (2 levels, the limit); D in C is one too many.
    assert_eq!(w.add(&admin, "A", &["B"]).await.0, 200);
    let (status, v) = w.add(&admin, "B", &["A"]).await;
    assert_eq!((status, details(&v)), (400, vec![("memberIds[0]".to_owned(), "membership_cycle".to_owned())]));
    assert_eq!(w.add(&admin, "B", &["C"]).await.0, 200);
    let (status, v) = w.add(&admin, "C", &["D"]).await;
    assert_eq!((status, details(&v)), (400, vec![("memberIds[0]".to_owned(), "membership_nesting_depth".to_owned())]));
    let (_, d) = w.get(&admin, &format!("/api/v1/business-services/{a}")).await;
    assert_eq!((d["memberCount"].as_i64(), d["serviceMemberCount"].as_i64()), (Some(4), Some(1)));
    let (_, nested) = w.get(&admin, &format!("/api/v1/business-services/{a}/members?kind=service")).await;
    assert_eq!(idents(&nested), ["B"]);
    let (_, plain) = w.get(&admin, &format!("/api/v1/business-services/{a}/members?kind=ci&q=WEB-0")).await;
    assert_eq!(plain["page"]["total"], 3);

    // Limits on the request.
    let many: Vec<Uuid> = (0..501).map(|_| Uuid::new_v4()).collect();
    let (status, v, _) = call(
        &w.app,
        "POST",
        &format!("/api/v1/business-services/{a}/members"),
        &admin,
        Some(json!({ "memberIds": many })),
    )
    .await;
    assert_eq!((status, code(&v)), (400, "VALIDATION_ERROR"), "{v}");
    let dup = w.id("web-04");
    let (status, _, _) = call(
        &w.app,
        "POST",
        &format!("/api/v1/business-services/{a}/members"),
        &admin,
        Some(json!({ "memberIds": [dup, dup] })),
    )
    .await;
    assert_eq!(status, 400);
    let unknown_class = Uuid::new_v4();
    let (status, v) = w.get(&admin, &format!("/api/v1/business-services/{a}/members?classId={unknown_class}")).await;
    assert_eq!((status, details(&v)), (400, vec![("classId".to_owned(), "not_found".to_owned())]));

    // Remove: bulk all or nothing, then single.
    let (status, v) = w.remove(&admin, "A", &["web-01", "web-04"]).await;
    assert_eq!((status, details(&v)), (400, vec![("memberIds[1]".to_owned(), "not_found".to_owned())]));
    assert_eq!(w.remove(&admin, "A", &["web-01", "web-02"]).await.0, 204);
    let path = format!("/api/v1/business-services/{a}/members/{}", w.id("web-03"));
    assert_eq!(call(&w.app, "DELETE", &path, &admin, None).await.0, 204);
    assert_eq!(call(&w.app, "DELETE", &path, &admin, None).await.0, 404);
    let (_, d) = w.get(&admin, &format!("/api/v1/business-services/{a}")).await;
    assert_eq!(d["memberCount"], 1);

    // Not a service, a missing one: 404 alike.
    for id in [w.id("web-01"), Uuid::new_v4()] {
        let (status, v) = w.get(&admin, &format!("/api/v1/business-services/{id}")).await;
        assert_eq!((status, code(&v)), (404, "NOT_FOUND"));
    }

    // The generic relationship endpoints refuse the member type.
    let body = json!({ "relationshipTypeId": w.member_type, "sourceCiId": a, "targetCiId": w.id("web-04") });
    let (status, v, _) = call(&w.app, "POST", "/api/v1/relationships", &admin, Some(body)).await;
    assert_eq!(
        (status, details(&v)),
        (400, vec![("relationshipTypeId".to_owned(), "system_relationship_type".to_owned())])
    );
    let edge: Uuid = sqlx::query_scalar(
        "SELECT id FROM ci_relationships WHERE source_ci_id = $1 AND target_ci_id = $2 AND deleted_at IS NULL",
    )
    .bind(a)
    .bind(w.id("B"))
    .fetch_one(&w.pool)
    .await
    .unwrap();
    let (status, v, _) = call(&w.app, "DELETE", &format!("/api/v1/relationships/{edge}"), &admin, None).await;
    assert_eq!(
        (status, details(&v)),
        (400, vec![("relationshipTypeId".to_owned(), "system_relationship_type".to_owned())])
    );
    let (status, _, _) =
        call(&w.app, "PATCH", &format!("/api/v1/relationships/{edge}"), &admin, Some(json!({ "notes": "x" }))).await;
    assert_eq!(status, 400);
    // Generic reads show member edges as usual.
    let (_, rels) = w.get(&admin, &format!("/api/v1/relationships?ciId={a}")).await;
    assert_eq!(
        (rels["page"]["total"].as_i64(), rels["data"][0]["targetCiId"].as_str()),
        (Some(1), Some(w.id("B").to_string().as_str()))
    );
}

/// Two requests that would jointly close a loop: exactly one succeeds.
#[tokio::test]
async fn concurrent_adds_cannot_build_a_loop() {
    let Some(db) = scratch::database("business_services_concurrent_loop").await else { return };
    let mut w = World::new(&db, BusinessServiceConfig::default()).await;
    w.ci("business_service", "A", "A").await;
    w.ci("business_service", "B", "B").await;
    let admin = w.admin.clone();
    for _ in 0..5 {
        let (x, y) = tokio::join!(w.add(&admin, "A", &["B"]), w.add(&admin, "B", &["A"]));
        let ok = [x.0, y.0].iter().filter(|s| **s == 200).count();
        assert_eq!(ok, 1, "{x:?} {y:?}");
        let loser = if x.0 == 200 { &y } else { &x };
        assert_eq!(details(&loser.1), [("memberIds[0]".to_owned(), "membership_cycle".to_owned())], "{loser:?}");
        sqlx::query("DELETE FROM ci_relationships WHERE relationship_type_id = $1")
            .bind(w.member_type)
            .execute(&w.pool)
            .await
            .unwrap();
    }
}

// ---------------------------------------------------------------------------
// §7.1 Protections (API side; the triggers are tested with B1)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_service_class_and_member_type_are_protected_by_the_api() {
    let Some(db) = scratch::database("business_services_api_protections").await else { return };
    let w = World::new(&db, BusinessServiceConfig::default()).await;
    let admin = &w.admin;
    let class = format!("/api/v1/ci-classes/{}", w.service_class);
    let (_, c) = w.get(admin, &class).await;
    assert_eq!(c["systemRole"], "business_service");
    let (_, other) = w.get(admin, &format!("/api/v1/ci-classes/{}", w.classes["server"])).await;
    assert_eq!(other["systemRole"], Value::Null);

    let in_use = |v: &Value| (code(v).to_owned(), details(v));
    let expect = ("IN_USE".to_owned(), vec![("id".to_owned(), "system_class".to_owned())]);
    let (status, v, _) = call(&w.app, "DELETE", &class, admin, None).await;
    assert_eq!((status, in_use(&v)), (409, expect.clone()), "archive");
    for patch in
        [json!({ "isActive": false }), json!({ "isAbstract": true }), json!({ "parentId": w.classes["server"] })]
    {
        let (status, v, _) = call(&w.app, "PATCH", &class, admin, Some(patch.clone())).await;
        assert_eq!((status, in_use(&v)), (409, expect.clone()), "{patch}");
    }
    let (status, v, _) =
        call(&w.app, "POST", &format!("{class}/purge"), admin, Some(json!({ "confirm": "business_service" }))).await;
    assert_eq!((status, in_use(&v)), (409, expect.clone()), "purge");
    // Rename and recolour are allowed.
    let (status, v, _) =
        call(&w.app, "PATCH", &class, admin, Some(json!({ "name": "IT service", "color": "#123456" }))).await;
    assert_eq!(status, 200, "{v}");
    let (status, v, _) = call(
        &w.app,
        "POST",
        "/api/v1/ci-classes",
        admin,
        Some(json!({ "key": "sub_service", "name": "Sub", "parentId": w.service_class })),
    )
    .await;
    assert_eq!((status, details(&v)), (400, vec![("parentId".to_owned(), "system_class".to_owned())]));

    // No CI leaves or enters the class, not even an empty service (GH#409).
    let mut w = w;
    let service = w.ci("business_service", "S", "Empty service").await;
    let server = w.ci("server", "web-01", "web-01").await;
    let admin = &w.admin;
    for (id, class) in [(service, w.classes["server"]), (server, w.service_class)] {
        let path = format!("/api/v1/configuration-items/{id}");
        let (status, v, _) = call(&w.app, "PATCH", &path, admin, Some(json!({ "classId": class }))).await;
        assert_eq!((status, details(&v)), (400, vec![("classId".to_owned(), "business_service_class".to_owned())]));
    }

    let rt = format!("/api/v1/relationship-types/{}", w.member_type);
    let (_, t) = w.get(admin, &rt).await;
    assert_eq!(t["systemRole"], "business_service_member");
    for (patch, field) in
        [(json!({ "impactDirection": "none" }), "impactDirection"), (json!({ "isActive": false }), "isActive")]
    {
        let (status, v, _) = call(&w.app, "PATCH", &rt, admin, Some(patch)).await;
        assert_eq!((status, details(&v)), (400, vec![(field.to_owned(), "system_relationship_type".to_owned())]));
    }
    // Unchanged values and labels are fine.
    let patch = json!({ "forwardLabel": "contains", "impactDirection": "target_to_source", "isActive": true });
    let (status, v, _) = call(&w.app, "PATCH", &rt, admin, Some(patch)).await;
    assert_eq!(status, 200, "{v}");
    let (status, v, _) = call(&w.app, "DELETE", &rt, admin, None).await;
    assert_eq!(
        (status, code(&v), details(&v)),
        (409, "IN_USE", vec![("id".to_owned(), "system_relationship_type".to_owned())])
    );
}

// ---------------------------------------------------------------------------
// §7.1 Soft delete
// ---------------------------------------------------------------------------

#[tokio::test]
async fn soft_deletes_hide_memberships_and_restores_bring_them_back() {
    let Some(db) = scratch::database("business_services_soft_delete").await else { return };
    let mut w = World::new(&db, BusinessServiceConfig::default()).await;
    let admin = w.admin.clone();
    let s = w.ci("business_service", "S", "Shop").await;
    let web = w.ci("server", "web-01", "web-01").await;
    w.ci("server", "web-02", "web-02").await;
    assert_eq!(w.add(&admin, "S", &["web-01", "web-02"]).await.0, 200);
    let me: Uuid =
        sqlx::query_scalar("SELECT id FROM users WHERE username = 'admin'").fetch_one(&w.pool).await.unwrap();
    let (_, d) = w.get(&admin, &format!("/api/v1/business-services/{s}")).await;
    let owners = json!({ "version": d["version"], "technical": [{ "kind": "user", "id": me }], "business": [] });
    let (status, v, _) =
        call(&w.app, "PUT", &format!("/api/v1/business-services/{s}/owners"), &admin, Some(owners)).await;
    assert_eq!(status, 200, "{v}");

    // A deleted member leaves the list; its restore (edges with it) brings it back.
    assert_eq!(call(&w.app, "DELETE", &format!("/api/v1/configuration-items/{web}"), &admin, None).await.0, 204);
    let (_, d) = w.get(&admin, &format!("/api/v1/business-services/{s}")).await;
    assert_eq!(d["memberCount"], 1);
    sqlx::query("UPDATE configuration_items SET deleted_at = NULL WHERE id = $1")
        .bind(web)
        .execute(&w.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE ci_relationships SET deleted_at = NULL WHERE target_ci_id = $1")
        .bind(web)
        .execute(&w.pool)
        .await
        .unwrap();
    let (_, d) = w.get(&admin, &format!("/api/v1/business-services/{s}")).await;
    assert_eq!(d["memberCount"], 2);

    // A deleted service: 404, its edges go, its owners stay for a restore.
    assert_eq!(call(&w.app, "DELETE", &format!("/api/v1/configuration-items/{s}"), &admin, None).await.0, 204);
    assert_eq!(w.get(&admin, &format!("/api/v1/business-services/{s}")).await.0, 404);
    let (_, list) = w.get(&admin, "/api/v1/business-services").await;
    assert_eq!(list["page"]["total"], 0);
    let live: i64 =
        sqlx::query_scalar("SELECT count(*) FROM ci_relationships WHERE source_ci_id = $1 AND deleted_at IS NULL")
            .bind(s)
            .fetch_one(&w.pool)
            .await
            .unwrap();
    assert_eq!(live, 0);
    sqlx::query("UPDATE configuration_items SET deleted_at = NULL WHERE id = $1")
        .bind(s)
        .execute(&w.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE ci_relationships SET deleted_at = NULL WHERE source_ci_id = $1")
        .bind(s)
        .execute(&w.pool)
        .await
        .unwrap();
    let (status, d) = w.get(&admin, &format!("/api/v1/business-services/{s}")).await;
    assert_eq!(status, 200);
    assert_eq!(
        (d["memberCount"].as_i64(), d["owners"]["technical"][0]["id"].as_str()),
        (Some(2), Some(me.to_string().as_str()))
    );
}

// ---------------------------------------------------------------------------
// §7.1 Owners
// ---------------------------------------------------------------------------

async fn group(w: &World, name: &str, users: &[Uuid]) -> Uuid {
    let id: Uuid = sqlx::query_scalar("INSERT INTO user_groups (name) VALUES ($1) RETURNING id")
        .bind(name)
        .fetch_one(&w.pool)
        .await
        .unwrap();
    for u in users {
        sqlx::query("INSERT INTO user_group_members (group_id, user_id) VALUES ($1, $2)")
            .bind(id)
            .bind(u)
            .execute(&w.pool)
            .await
            .unwrap();
    }
    id
}

#[tokio::test]
async fn owners_are_replaced_in_order_and_shown_by_name_only() {
    let Some(db) = scratch::database("business_services_owners").await else { return };
    let mut w = World::new(&db, BusinessServiceConfig::default()).await;
    let admin = w.admin.clone();
    let s = w.ci("business_service", "S", "Shop").await;
    let (alice, _) = w.user("alice", &[], &[]).await;
    let (bob, _) = w.user("bob", &[], &[]).await;
    sqlx::query("UPDATE users SET is_active = false, email = 'bob@example.com' WHERE id = $1")
        .bind(bob)
        .execute(&w.pool)
        .await
        .unwrap();
    let dba = group(&w, "DBA team", &[alice]).await;
    let path = format!("/api/v1/business-services/{s}/owners");
    let (_, d) = w.get(&admin, &format!("/api/v1/business-services/{s}")).await;
    let v1 = d["version"].as_i64().unwrap();

    let body = json!({
        "version": v1,
        "technical": [{ "kind": "group", "id": dba }, { "kind": "user", "id": bob }],
        "business": [{ "kind": "user", "id": alice }],
    });
    let (status, v, _) = call(&w.app, "PUT", &path, &admin, Some(body.clone())).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(
        v,
        json!({
            "technical": [
                { "kind": "group", "id": dba, "displayName": "DBA team", "active": true },
                { "kind": "user", "id": bob, "displayName": "User bob", "active": false },
            ],
            "business": [{ "kind": "user", "id": alice, "displayName": "User alice", "active": true }],
        })
    );
    let (_, d) = w.get(&admin, &format!("/api/v1/business-services/{s}")).await;
    assert_eq!((d["version"].as_i64(), &d["owners"]), (Some(v1 + 1), &v));
    assert!(!d.to_string().contains("bob@example.com") && !d.to_string().contains("\"username\""));

    // Stale version, duplicates, unknown principals.
    let (status, v, _) = call(&w.app, "PUT", &path, &admin, Some(body)).await;
    assert_eq!((status, code(&v)), (409, "VERSION_CONFLICT"));
    let bad = json!({
        "version": v1 + 1,
        "technical": [{ "kind": "user", "id": alice }, { "kind": "user", "id": alice }],
        "business": [{ "kind": "group", "id": Uuid::new_v4() }, { "kind": "group", "id": alice }],
    });
    let (status, v, _) = call(&w.app, "PUT", &path, &admin, Some(bad)).await;
    assert_eq!(
        (status, details(&v)),
        (
            400,
            vec![
                ("technical[1]".to_owned(), "duplicate".to_owned()),
                ("business[0]".to_owned(), "not_found".to_owned()),
                ("business[1]".to_owned(), "not_found".to_owned()),
            ]
        )
    );
    let eleven: Vec<Value> = (0..11).map(|_| json!({ "kind": "user", "id": alice })).collect();
    let (status, _, _) =
        call(&w.app, "PUT", &path, &admin, Some(json!({ "version": v1 + 1, "technical": eleven, "business": [] })))
            .await;
    assert_eq!(status, 400);

    // One audit row on the service, kind / id / name per owner.
    let (_, log) = w.get(&admin, &format!("/api/v1/audit-log?entityId={s}&action=update")).await;
    let row = &log["data"][0];
    assert_eq!(row["entityType"], "configuration_items");
    assert_eq!(row["oldValue"], json!({ "owners": { "technical": [], "business": [] } }));
    assert_eq!(
        row["newValue"]["owners"]["technical"],
        json!([{ "kind": "group", "id": dba, "name": "DBA team" }, { "kind": "user", "id": bob, "name": "User bob" }])
    );

    // A caller with view only may read but not assign.
    let (_, viewer) = w.user("viewer", &[("business_service", false)], &[]).await;
    let (status, _, _) =
        call(&w.app, "PUT", &path, &viewer, Some(json!({ "version": v1 + 1, "technical": [], "business": [] }))).await;
    assert_eq!(status, 403);
    assert_eq!(w.get(&viewer, &format!("/api/v1/business-services/{s}")).await.0, 200);
}

// ---------------------------------------------------------------------------
// §7.1 List
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_list_filters_sorts_and_pages() {
    let Some(db) = scratch::database("business_services_list").await else { return };
    let mut w = World::new(&db, BusinessServiceConfig::default()).await;
    let admin = w.admin.clone();
    let crit: Vec<(Uuid, String)> = sqlx::query_as(
        "SELECT v.id, v.key FROM lookup_list_values v JOIN lookup_lists l ON l.id = v.list_id
         WHERE l.system_role = 'criticality' ORDER BY v.sort_order, v.key",
    )
    .fetch_all(&w.pool)
    .await
    .unwrap();
    let (most, least) = (crit[0].0, crit[crit.len() - 1].0);
    w.ci_with("business_service", "S1", json!({ "attributes": { "name": "Alpha" }, "criticalityValueId": least }))
        .await;
    w.ci_with("business_service", "S2", json!({ "attributes": { "name": "beta" }, "criticalityValueId": most })).await;
    w.ci("business_service", "S3", "Gamma").await;
    w.ci_with(
        "business_service",
        "S4",
        json!({ "attributes": { "name": "Old" }, "validFrom": "2020-01-01T00:00:00Z", "validUntil": "2021-01-01T00:00:00Z" }),
    )
    .await;
    for i in 1..=3 {
        w.ci("server", &format!("m{i}"), &format!("m{i}")).await;
    }
    w.add(&admin, "S3", &["m1", "m2", "m3"]).await;
    w.add(&admin, "S1", &["m1"]).await;

    let (me, mine) = w.user("me", &[("business_service", true)], &[]).await;
    let (other, _) = w.user("other", &[], &[]).await;
    sqlx::query("UPDATE users SET is_active = false WHERE id = $1").bind(other).execute(&w.pool).await.unwrap();
    let team = group(&w, "Team", &[me]).await;
    let put = |s: &str, technical: Value, business: Value| {
        let path = format!("/api/v1/business-services/{}/owners", w.id(s));
        let app = w.app.clone();
        let admin = admin.clone();
        async move {
            let (_, d, _) = call(&app, "GET", &path.replace("/owners", ""), &admin, None).await;
            let body = json!({ "version": d["version"], "technical": technical, "business": business });
            let (status, v, _) = call(&app, "PUT", &path, &admin, Some(body)).await;
            assert_eq!(status, 200, "{v}");
        }
    };
    put("S1", json!([{ "kind": "user", "id": me }]), json!([])).await;
    put("S2", json!([]), json!([{ "kind": "group", "id": team }])).await;
    put("S3", json!([{ "kind": "user", "id": other }]), json!([])).await;

    let names = |v: &Value| -> Vec<String> {
        v["data"].as_array().unwrap().iter().map(|s| s["name"].as_str().unwrap().to_owned()).collect()
    };
    let list = |q: &str| {
        let app = w.app.clone();
        let creds = admin.clone();
        let q = q.to_owned();
        async move {
            let (status, v, _) = call(&app, "GET", &format!("/api/v1/business-services{q}"), &creds, None).await;
            assert_eq!(status, 200, "{q}: {v}");
            v
        }
    };
    // Default: most critical first, not set last, then by name.
    assert_eq!(names(&list("").await), ["beta", "Alpha", "Gamma", "Old"]);
    assert_eq!(names(&list("?sort=name").await), ["Alpha", "beta", "Gamma", "Old"]);
    assert_eq!(names(&list("?sort=-memberCount").await), ["Gamma", "Alpha", "beta", "Old"]);
    assert_eq!(names(&list("?sort=memberCount").await)[..2], ["beta", "Old"]);
    assert_eq!(list("?sort=-updatedAt").await["data"].as_array().unwrap().len(), 4);
    assert_eq!(names(&list("?q=alp").await), ["Alpha"]);
    assert_eq!(names(&list("?q=S2").await), ["beta"]);
    assert_eq!(names(&list(&format!("?criticalityValueId={most}")).await), ["beta"]);
    assert_eq!(names(&list(&format!("?criticalityValueId={most},none&sort=name")).await), ["beta", "Gamma", "Old"]);
    assert_eq!(names(&list(&format!("?ownerId={me}")).await), ["Alpha"]);
    assert_eq!(names(&list(&format!("?ownerId={me}&ownerRole=business")).await), Vec::<String>::new());
    assert_eq!(names(&list(&format!("?ownerId={team}&ownerRole=business")).await), ["beta"]);
    assert_eq!(names(&list("?ownerState=none&sort=name").await), ["Old"]);
    assert_eq!(names(&list("?ownerState=disabled").await), ["Gamma"]);
    assert_eq!(names(&list("?includeInactive=false&sort=name").await), ["Alpha", "beta", "Gamma"]);
    let page = list("?sort=name&limit=2&offset=1").await;
    assert_eq!((names(&page), page["page"]["total"].as_i64()), (vec!["beta".to_owned(), "Gamma".to_owned()], Some(4)));
    let alpha = &list("?q=alpha").await["data"][0];
    assert_eq!(
        (alpha["memberCount"].as_i64(), alpha["owners"]["technical"][0]["displayName"].as_str()),
        (Some(1), Some("User me"))
    );
    // "My services": directly (S1) and through the group (S2).
    let (_, v, _) = call(&w.app, "GET", "/api/v1/business-services?mine=true&sort=name", &mine, None).await;
    assert_eq!(names(&v), ["Alpha", "beta"]);
    assert_eq!(v["visibility"], "restricted");
    // Bad parameters.
    for q in ["?sort=ident", "?ownerState=all", "?limit=201", "?criticalityValueId=x", "?q="] {
        let (status, _, _) = call(&w.app, "GET", &format!("/api/v1/business-services{q}"), &admin, None).await;
        assert_eq!(status, 400, "{q}");
    }
    let many: Vec<String> = (0..51).map(|_| Uuid::new_v4().to_string()).collect();
    let (status, _, _) =
        call(&w.app, "GET", &format!("/api/v1/business-services?ownerId={}", many.join(",")), &admin, None).await;
    assert_eq!(status, 400);
    // Without view on the class: 403 on the list and the detail.
    let (_, none) = w.user("none", &[("server", false)], &[]).await;
    assert_eq!(call(&w.app, "GET", "/api/v1/business-services", &none, None).await.0, 403);
    assert_eq!(call(&w.app, "GET", &format!("/api/v1/business-services/{}", w.id("S1")), &none, None).await.0, 403);
}

// ---------------------------------------------------------------------------
// §7.1 Impact and "part of"
// ---------------------------------------------------------------------------

#[tokio::test]
async fn impact_follows_membership_and_part_of_lists_nesting() {
    let Some(db) = scratch::database("business_services_impact").await else { return };
    let mut w = World::new(&db, BusinessServiceConfig::default()).await;
    let admin = w.admin.clone();
    for (s, name) in [("S", "Shop"), ("O", "Outer"), ("X", "Top")] {
        w.ci("business_service", s, name).await;
    }
    let db01 = w.ci("server", "db-01", "db-01").await;
    w.add(&admin, "S", &["db-01"]).await;
    w.add(&admin, "O", &["S"]).await;
    w.add(&admin, "X", &["O"]).await;

    let (_, down) = w.get(&admin, &format!("/api/v1/configuration-items/{db01}/impact?depth=3")).await;
    let hops: Vec<(String, i64, String)> = down["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| {
            (
                i["ident"].as_str().unwrap().into(),
                i["hops"].as_i64().unwrap(),
                i["via"]["relationshipType"]["forwardLabel"].as_str().unwrap().into(),
            )
        })
        .collect();
    assert_eq!(
        hops,
        [("S".into(), 1, "includes".into()), ("O".into(), 2, "includes".into()), ("X".into(), 3, "includes".into())]
    );
    let (_, up) = w.get(&admin, &format!("/api/v1/configuration-items/{}/impact?direction=upstream", w.id("S"))).await;
    assert_eq!(up["items"][0]["ident"], "db-01");
    let (_, other) =
        w.get(&admin, &format!("/api/v1/configuration-items/{db01}/impact?relationshipTypeId={}", w.runs_on)).await;
    assert_eq!(other["items"], json!([]));

    let (_, part) = w.get(&admin, &format!("/api/v1/configuration-items/{db01}/business-services")).await;
    let rows: Vec<(String, bool, Vec<Uuid>)> = part["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            (
                e["service"]["ident"].as_str().unwrap().into(),
                e["direct"].as_bool().unwrap(),
                e["viaServiceIds"].as_array().unwrap().iter().map(|v| v.as_str().unwrap().parse().unwrap()).collect(),
            )
        })
        .collect();
    assert_eq!(
        rows,
        [
            ("S".into(), true, vec![]),
            ("O".into(), false, vec![w.id("S")]),
            ("X".into(), false, vec![w.id("S"), w.id("O")]),
        ]
    );
    assert_eq!((part["truncated"].as_bool(), part["visibility"].as_str()), (Some(false), Some("all_classes")));
    assert_eq!(
        w.get(&admin, &format!("/api/v1/configuration-items/{}/business-services", Uuid::new_v4())).await.0,
        404
    );

    // More than 200 services: truncated at 200.
    let web = w.ci("server", "web", "web").await;
    let ids: Vec<Uuid> = sqlx::query_scalar("SELECT id FROM configuration_items WHERE class_id = $1")
        .bind(w.service_class)
        .fetch_all(&w.pool)
        .await
        .unwrap();
    let mut services = ids;
    for i in services.len()..201 {
        services.push(w.ci("business_service", &format!("many-{i:03}"), &format!("Many {i:03}")).await);
    }
    sqlx::query(
        "INSERT INTO ci_relationships (relationship_type_id, source_ci_id, target_ci_id) SELECT $1, s, $2 FROM unnest($3::uuid[]) s",
    )
    .bind(w.member_type)
    .bind(web)
    .bind(&services)
    .execute(&w.pool)
    .await
    .unwrap();
    let (_, part) = w.get(&admin, &format!("/api/v1/configuration-items/{web}/business-services")).await;
    assert_eq!((part["data"].as_array().unwrap().len(), part["truncated"].as_bool()), (200, Some(true)));
}

// ---------------------------------------------------------------------------
// §7.1 Audit and CSV
// ---------------------------------------------------------------------------

#[tokio::test]
async fn membership_changes_and_exports_are_audited() {
    let Some(db) = scratch::database("business_services_audit_csv").await else { return };
    let mut w = World::new(&db, BusinessServiceConfig::default()).await;
    let admin = w.admin.clone();
    let s = w.ci("business_service", "SHOP-1", "Shop").await;
    let evil = w.ci("server", "web-01", "=HYPERLINK(\"http://x\")").await;
    let web2 = w.ci("server", "web-02", "web \"two\"").await;
    let (status, added) = w.add(&admin, "SHOP-1", &["web-01", "web-02"]).await;
    assert_eq!(status, 200);
    assert_eq!(w.remove(&admin, "SHOP-1", &["web-02"]).await.0, 204);

    let (_, log) = w.get(&admin, &format!("/api/v1/audit-log?entityId={s}&action=update&sort=occurredAt")).await;
    let rows: Vec<&Value> = log["data"].as_array().unwrap().iter().collect();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["newValue"], json!({ "members": { "added": [evil, web2], "removed": [] } }));
    assert_eq!(rows[1]["newValue"], json!({ "members": { "added": [], "removed": [web2] } }));
    assert_eq!(rows[0]["oldValue"], rows[0]["newValue"]);
    let edge_rows: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_log WHERE entity_type = 'ci_relationships' AND (new_value->>'sourceCiId' = $1 OR old_value->>'sourceCiId' = $1)",
    )
    .bind(s.to_string())
    .fetch_one(&w.pool)
    .await
    .unwrap();
    assert_eq!(edge_rows, 3, "two creates and one delete");

    // CSV: headers, quoting, formula neutralising, same rows as the JSON list.
    let (status, headers, csv) =
        raw(&w.app, "GET", &format!("/api/v1/business-services/{s}/members/export"), &admin, None).await;
    assert_eq!(status, 200);
    assert!(headers[header::CONTENT_TYPE].to_str().unwrap().starts_with("text/csv"));
    assert_eq!(headers[header::CACHE_CONTROL], "no-store");
    let disposition = headers[header::CONTENT_DISPOSITION].to_str().unwrap();
    assert!(
        disposition.starts_with("attachment; filename=\"service-members-SHOP-1-") && disposition.ends_with(".csv\""),
        "{disposition}"
    );
    let lines: Vec<&str> = csv.split("\r\n").collect();
    assert!(lines[0].starts_with("\"# Members of business service SHOP-1 (Shop): sort=name"), "{}", lines[0]);
    assert_eq!(
        lines[1],
        "\"ci_id\",\"ident\",\"name\",\"class\",\"criticality\",\"is_service\",\"active\",\"added_at\""
    );
    let membership = &added["added"][0];
    assert_eq!(
        lines[2],
        format!(
            "\"{evil}\",\"web-01\",\"'=HYPERLINK(\"\"http://x\"\")\",\"server\",\"\",\"false\",\"true\",\"{}\"",
            membership["addedAt"].as_str().unwrap()
        )
    );
    assert_eq!(lines.len(), 4, "comment, header, one row, trailing empty");
    let (_, list) = w.get(&admin, &format!("/api/v1/business-services/{s}/members")).await;
    assert_eq!(list["page"]["total"], 1);

    // The export row: counts only.
    let (_, log) = w.get(&admin, &format!("/api/v1/audit-log?entityId={s}&action=export")).await;
    assert_eq!(
        log["data"][0]["newValue"],
        json!({ "kind": "business_service_members", "format": "csv", "rowCount": 1, "visibility": "all_classes" })
    );
}

/// The member export neutralises what the shared csv_safe rule does and keeps
/// a line break inside its quoted cell (GH#388). The labels are written
/// directly, as older rows or imports can hold them.
#[tokio::test]
async fn member_export_uses_the_shared_csv_safe_rule() {
    let Some(db) = scratch::database("business_services_export_csv_safe").await else { return };
    let mut w = World::new(&db, BusinessServiceConfig::default()).await;
    let admin = w.admin.clone();
    let s = w.ci("business_service", "SHOP-1", "Shop").await;
    let labels = [" =1+1", "\u{3000}=1+1", "＝1+1", "＋1", "－1", "＠SUM(A1)", "\t=1", "\r=1", "Rack A\nSlot 4"];
    let mut idents = Vec::new();
    for (i, label) in labels.iter().enumerate() {
        let ident = format!("web-{i:02}");
        let id = w.ci("server", &ident, "placeholder").await;
        sqlx::query("UPDATE cmdb.configuration_items SET label = $1 WHERE id = $2")
            .bind(label)
            .bind(id)
            .execute(&w.pool)
            .await
            .unwrap();
        idents.push(ident);
    }
    let members: Vec<&str> = idents.iter().map(String::as_str).collect();
    assert_eq!(w.add(&admin, "SHOP-1", &members).await.0, 200);

    let (status, _, csv) =
        raw(&w.app, "GET", &format!("/api/v1/business-services/{s}/members/export"), &admin, None).await;
    assert_eq!(status, 200, "{csv}");
    for label in &labels[..8] {
        assert!(csv.contains(&format!(",\"'{label}\",")), "{label:?} in {csv:?}");
    }
    assert!(csv.contains(",\"Rack A\nSlot 4\","), "{csv:?}");
    assert!(!csv.contains("Rack A Slot 4"), "{csv:?}");
}

// ---------------------------------------------------------------------------
// Principals and settings
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_owner_picker_is_a_bounded_lookup() {
    let Some(db) = scratch::database("business_services_principals").await else { return };
    let w = World::new(&db, BusinessServiceConfig::default()).await;
    for i in 0..25 {
        sqlx::query(
            "INSERT INTO users (username, display_name, password_hash, is_active) VALUES ($1, $2, '$argon2id$x', $3)",
        )
        .bind(format!("smith{i:02}"))
        .bind(format!("Pat Smith {i:02}"))
        .bind(i != 3)
        .execute(&w.pool)
        .await
        .unwrap();
    }
    group(&w, "Smith family", &[]).await;
    let (_, editor) = w.user("editor", &[("business_service", true)], &[]).await;
    let (_, manager) = w.user("manager", &[], &["users.manage"]).await;
    let (_, viewer) = w.user("viewer", &[("business_service", false)], &[]).await;

    let (status, v, _) = call(&w.app, "GET", "/api/v1/principals?q=smith", &editor, None).await;
    assert_eq!(status, 200, "{v}");
    let data = v["data"].as_array().unwrap();
    assert_eq!(data.len(), 20);
    assert!(data.iter().all(|p| p["active"] == true));
    assert!(data.iter().filter(|p| p["kind"] == "user").all(|p| p["username"].is_string() && p.get("email").is_none()));
    let (_, v, _) =
        call(&w.app, "GET", "/api/v1/principals?q=smith03&includeInactive=true&kind=user", &manager, None).await;
    assert_eq!((v["data"][0]["username"].as_str(), v["data"][0]["active"].as_bool()), (Some("smith03"), Some(false)));
    let (_, v, _) = call(&w.app, "GET", "/api/v1/principals?q=smith&kind=group", &editor, None).await;
    assert_eq!(
        v["data"],
        json!([{ "kind": "group", "id": v["data"][0]["id"], "displayName": "Smith family", "active": true }])
    );
    assert_eq!(call(&w.app, "GET", "/api/v1/principals?q=smith", &viewer, None).await.0, 403);
    // The permission is checked before the query.
    assert_eq!(call(&w.app, "GET", "/api/v1/principals", &viewer, None).await.0, 403);
    for q in ["s", "%20s%20", &"x".repeat(101)] {
        let (status, _, _) = call(&w.app, "GET", &format!("/api/v1/principals?q={q}"), &editor, None).await;
        assert_eq!(status, 400, "{q:?}");
    }
    assert_eq!(call(&w.app, "GET", "/api/v1/principals", &editor, None).await.0, 400);

    let (status, v, _) = call(&w.app, "GET", "/api/v1/settings/business-services", &viewer, None).await;
    assert_eq!(status, 200);
    assert_eq!(
        v,
        json!({
            "classId": w.service_class,
            "memberRelationshipTypeId": w.member_type,
            "canView": true,
            "canEdit": false,
            "limits": { "maxMembers": 5000, "maxBatch": 500, "maxNesting": 5, "maxOwnersPerRole": 10 },
        })
    );
}
