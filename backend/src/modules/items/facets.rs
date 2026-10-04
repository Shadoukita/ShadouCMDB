//! Facet counts of the inventory list (SHAA-1686): per class, criticality,
//! lookup list and business service, how many CIs match.
//!
//! Each facet is counted with its own filter left out and every other filter
//! applied (multi-select facets: ticking a second class still shows the other
//! classes' counts). The filter sets are the list's own ([`inventory_filters`]),
//! view scope included, so no count covers a CI the caller could not list.
//! Counts are exact; the work is one statement (see `data::item_facets`).

use std::collections::{HashMap, HashSet};

use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use super::plan::is_visible;
use super::schemas::{Facet, FacetKind, FacetValue, FacetsQuery, ItemFacets};
use super::service::inventory_filters;
use crate::api::context::RequestContext;
use crate::api::schemas::UuidList;
use crate::auth::permissions::ClassOp;
use crate::data::item_facets::{self as data, FacetSource, FacetSql, ValueRow};
use crate::data::items::{ItemFilters, lookup_value_lists};
use crate::http::error::AppError;
use crate::schema::model::Model;

/// A facet being built: what it is, its counts' filter set and its selection.
struct Spec {
    facet: Facet,
    sql: FacetSql,
    selected: Vec<Uuid>,
}

/// The filter set of `q`, added to `sets` unless it is the full one (index 0).
async fn set_without(
    conn: &mut PgConnection,
    ctx: &RequestContext,
    model: &Model,
    sets: &mut Vec<ItemFilters>,
    q: &FacetsQuery,
    removed: bool,
) -> Result<usize, AppError> {
    if !removed {
        return Ok(0);
    }
    let own_layout = q.own_layout.map(bool::from);
    sets.push(inventory_filters(conn, ctx, model, q, q.q.as_deref(), own_layout, q.layout_template.as_deref()).await?);
    Ok(sets.len() - 1)
}

fn facet(key: &str, kind: FacetKind, label: &str, param: &str, list_id: Option<Uuid>) -> Facet {
    Facet {
        key: key.into(),
        kind,
        label: label.into(),
        param: param.into(),
        list_id,
        values: Vec::new(),
        truncated: false,
    }
}

pub async fn facets(pool: &PgPool, ctx: &RequestContext, q: &FacetsQuery) -> Result<ItemFacets, AppError> {
    let mut conn = pool.acquire().await?;
    let model = Model::load(&mut conn).await?;
    let visible = ctx.class_scope(ClassOp::View);
    let own_layout = q.own_layout.map(bool::from);
    let all = inventory_filters(&mut conn, ctx, &model, q, q.q.as_deref(), own_layout, q.layout_template.as_deref());
    let mut sets = vec![all.await?];
    let ids = |l: &Option<UuidList>| l.as_ref().map(|l| l.0.clone()).unwrap_or_default();
    let mut specs: Vec<Spec> = Vec::new();

    let without = FacetsQuery { class_id: None, ..q.clone() };
    let base = set_without(&mut conn, ctx, &model, &mut sets, &without, q.class_id.is_some()).await?;
    specs.push(Spec {
        facet: facet("class", FacetKind::Class, "Class", "classId", None),
        sql: FacetSql { base, source: FacetSource::Class },
        selected: ids(&q.class_id),
    });

    let without = FacetsQuery { criticality_value_id: None, ..q.clone() };
    let base = set_without(&mut conn, ctx, &model, &mut sets, &without, q.criticality_value_id.is_some()).await?;
    specs.push(Spec {
        facet: facet("criticality", FacetKind::Criticality, "Criticality", "criticalityValueId", None),
        sql: FacetSql { base, source: FacetSource::Criticality },
        selected: ids(&q.criticality_value_id),
    });

    // Lookup lists stored in at least one field; values of other lists (or
    // unknown ids) in lookupValueId stay applied to every list's counts.
    let requested = ids(&q.lookup_value_id);
    let list_of: HashMap<Uuid, Uuid> = lookup_value_lists(&mut conn, &requested).await?.into_iter().collect();
    for list in data::lookup_lists(&mut conn).await? {
        let columns = model.lookup_columns(list.id);
        if columns.is_empty() {
            continue;
        }
        let (selected, rest): (Vec<Uuid>, Vec<Uuid>) =
            requested.iter().partition(|v| list_of.get(*v) == Some(&list.id));
        let without = FacetsQuery { lookup_value_id: (!rest.is_empty()).then_some(UuidList(rest)), ..q.clone() };
        let base = set_without(&mut conn, ctx, &model, &mut sets, &without, !selected.is_empty()).await?;
        specs.push(Spec {
            facet: facet(
                &format!("lookup.{}", list.key),
                FacetKind::Lookup,
                &list.name,
                "lookupValueId",
                Some(list.id),
            ),
            sql: FacetSql { base, source: FacetSource::Lookup(columns) },
            selected,
        });
    }

    if let Some(roles) = crate::data::business_services::roles(&mut conn).await? {
        let without = FacetsQuery { business_service_id: None, ..q.clone() };
        let base = set_without(&mut conn, ctx, &model, &mut sets, &without, q.business_service_id.is_some()).await?;
        specs.push(Spec {
            facet: facet("businessService", FacetKind::BusinessService, "Business service", "businessServiceId", None),
            sql: FacetSql {
                base,
                source: FacetSource::Service { member_type: roles.member_type, visible: visible.clone() },
            },
            selected: ids(&q.business_service_id),
        });
    }

    let sql: Vec<FacetSql> = specs.iter().map(|s| s.sql.clone()).collect();
    let (total, counts) = data::counts(&mut conn, &sets, &sql).await?;

    // Labels of every counted or selected value, per kind.
    let mut wanted: HashMap<FacetKind, HashSet<Uuid>> = HashMap::new();
    for (i, s) in specs.iter().enumerate() {
        let w = wanted.entry(s.facet.kind).or_default();
        w.extend(s.selected.iter().copied());
        w.extend(counts.iter().filter(|c| c.0 as usize == i).map(|c| c.1));
    }
    let take = |kind| wanted.get(&kind).map(|w| w.iter().copied().collect::<Vec<_>>()).unwrap_or_default();
    let mut labels: HashMap<(FacetKind, Uuid), ValueRow> = HashMap::new();
    let classes = data::class_labels(&mut conn, &take(FacetKind::Class)).await?;
    let mut values = take(FacetKind::Criticality);
    values.extend(take(FacetKind::Lookup));
    let values = data::value_labels(&mut conn, &values).await?;
    let services = data::service_labels(&mut conn, &take(FacetKind::BusinessService)).await?;
    for (kind, rows) in
        [(FacetKind::Class, classes), (FacetKind::Lookup, values), (FacetKind::BusinessService, services)]
    {
        labels.extend(rows.into_iter().map(|r| ((kind, r.id), r)));
    }
    let criticality_list = data::criticality_list(&mut conn).await?;

    let limit = q.value_limit.clamp(1, 200) as usize;
    let mut facets = Vec::with_capacity(specs.len());
    for (i, spec) in specs.into_iter().enumerate() {
        let Spec { mut facet, selected, .. } = spec;
        let label_kind = if facet.kind == FacetKind::Criticality { FacetKind::Lookup } else { facet.kind };
        // A selected value is shown at 0 only when the caller may see it and it belongs here.
        let belongs = |r: &ValueRow| match facet.kind {
            FacetKind::Class | FacetKind::BusinessService => is_visible(visible.as_deref(), r.scope.unwrap_or(r.id)),
            FacetKind::Criticality => r.scope.is_some() && r.scope == criticality_list,
            FacetKind::Lookup => r.scope.is_some() && r.scope == facet.list_id,
        };
        let mut counted: HashMap<Uuid, i64> = counts.iter().filter(|c| c.0 as usize == i).map(|c| (c.1, c.2)).collect();
        for id in &selected {
            if !counted.contains_key(id) && labels.get(&(label_kind, *id)).is_some_and(belongs) {
                counted.insert(*id, 0);
            }
        }
        let mut rows: Vec<(FacetValue, i32)> = counted
            .into_iter()
            .filter_map(|(id, count)| {
                let r = labels.get(&(label_kind, id))?;
                let v = FacetValue {
                    id,
                    key: r.key.clone(),
                    label: r.name.clone(),
                    count,
                    selected: selected.contains(&id),
                };
                Some((v, r.sort_order))
            })
            .collect();
        rows.sort_by(|(a, ao), (b, bo)| {
            b.count.cmp(&a.count).then(ao.cmp(bo)).then_with(|| a.label.to_lowercase().cmp(&b.label.to_lowercase()))
        });
        let mut kept = Vec::with_capacity(rows.len().min(limit));
        for (v, _) in rows {
            if kept.len() < limit || v.selected {
                kept.push(v);
            } else {
                facet.truncated = true;
            }
        }
        facet.values = kept;
        facets.push(facet);
    }
    Ok(ItemFacets { total, facets })
}

/// Facet performance gate (SHAA-1686). Not part of the normal test run: it
/// seeds the impact analysis data set (100 000 CIs in 20 classes, 30 % hidden
/// from the test profile) and two lookup lists set on every CI.
///
/// ```sh
/// SHADOUCMDB_TEST_DATABASE_URL=postgres://… cargo test --release inventory_facets_performance -- --ignored --nocapture
/// ```
#[cfg(test)]
mod perf {
    use std::time::Instant;

    use serde_json::json;
    use uuid::Uuid;

    use super::facets;
    use crate::api::context::RequestContext;
    use crate::api::schemas::{Deleted, QueryBool, UuidList};
    use crate::db::scratch;
    use crate::modules::api_tokens::tests::{Creds, app_with_business_services, call, session_of};
    use crate::modules::impact::perf::{self as ia, CLASSES, HIDDEN_CLASSES, exec, p95};
    use crate::modules::items::schemas::{ActiveQuery, FacetsQuery};
    use crate::schema::model::Model;

    fn query() -> FacetsQuery {
        FacetsQuery {
            q: None,
            class_id: None,
            include_subclasses: QueryBool::True,
            active: ActiveQuery::True,
            lookup_value_id: None,
            ip_within: None,
            criticality_value_id: None,
            deleted: Deleted::Exclude,
            own_layout: None,
            layout_template: None,
            kind: None,
            business_service_id: None,
            value_limit: 50,
        }
    }

    #[tokio::test]
    #[ignore = "seeds 100 000 CIs; run with --ignored"]
    async fn inventory_facets_performance() {
        let Some(db) = scratch::database("inventory_facets_performance").await else { return };
        let pool = &db.pool;
        let ctx = RequestContext::system("perf", "facets-perf");
        let mut tx = pool.begin().await.unwrap();
        crate::schema::reconcile(&mut tx, &ctx, "perf").await.unwrap_or_else(|e| panic!("reconcile: {}", e.message));
        tx.commit().await.unwrap();
        let base = ia::seed(pool).await;

        // status (5 values) and env (4 values) on every class, set on every CI.
        let app = app_with_business_services(pool.clone(), Default::default());
        let login = json!({ "username": "owner", "password": "correct horse battery" });
        let (status, me, headers) = call(&app, "POST", "/api/v1/auth/login", &Creds::default(), Some(login)).await;
        assert_eq!(status, 200, "{me}");
        let owner = session_of(&me, &headers);
        let mut values: Vec<(String, Vec<Uuid>)> = Vec::new();
        for (list, n) in [("perf_status", 5), ("perf_env", 4)] {
            let list_id: Uuid = sqlx::query_scalar("INSERT INTO lookup_lists (key, name) VALUES ($1, $1) RETURNING id")
                .bind(list)
                .fetch_one(pool)
                .await
                .unwrap();
            let ids: Vec<Uuid> = sqlx::query_scalar(
                "INSERT INTO lookup_list_values (list_id, key, name, sort_order)
                 SELECT $1, 'v' || i, 'Value ' || i, i FROM generate_series(1, $2) i RETURNING id",
            )
            .bind(list_id)
            .bind(n)
            .fetch_all(pool)
            .await
            .unwrap();
            for class in &base.classes {
                let field = json!({ "classId": class, "key": list, "label": list, "dataType": "lookup", "lookupListId": list_id });
                let (status, f, _) = call(&app, "POST", "/api/v1/attribute-definitions", &owner, Some(field)).await;
                assert_eq!(status, 201, "{f}");
            }
            values.push((list.to_owned(), ids));
        }
        let model = Model::load(&mut pool.acquire().await.unwrap()).await.unwrap();
        for class in &base.classes {
            let table = model.table(*class).unwrap();
            let mut sets = Vec::new();
            for (list, ids) in &values {
                let column = model.own_fields(*class).find(|f| f.key == *list).unwrap().column();
                let picks: Vec<String> = ids.iter().map(|id| format!("'{id}'::uuid")).collect();
                sets.push(format!(
                    "{column} = (ARRAY[{}])[1 + (('x' || substr(md5(id::text || '{list}'), 1, 8))::bit(32)::bigint % {})]",
                    picks.join(", "),
                    ids.len()
                ));
            }
            exec(pool, &format!("UPDATE {} SET {}", table.sql(), sets.join(", "))).await;
        }
        exec(pool, "ANALYZE").await;

        let visible: Vec<Uuid> = base.classes[..CLASSES - HIDDEN_CLASSES].to_vec();
        let restricted = ia::viewer(&visible);
        let (status_values, env_values) = (&values[0].1, &values[1].1);
        let cases: Vec<(&str, FacetsQuery)> = vec![
            ("no filter", query()),
            (
                "class + status + env selected",
                FacetsQuery {
                    class_id: Some(UuidList(base.classes[..3].to_vec())),
                    lookup_value_id: Some(UuidList(vec![status_values[0], status_values[1], env_values[0]])),
                    ..query()
                },
            ),
            ("search q=perf 01", FacetsQuery { q: Some("perf 01".into()), ..query() }),
        ];
        let mut report = Vec::new();
        let mut failures = Vec::new();
        for (who, ctx) in [("admin", &ctx), ("restricted", &restricted)] {
            for (name, q) in &cases {
                let mut ms = Vec::new();
                let mut total = 0;
                for i in 0..25 {
                    let t = Instant::now();
                    let r = facets(pool, ctx, q).await.unwrap();
                    if i >= 5 {
                        ms.push(t.elapsed().as_secs_f64() * 1000.0);
                    }
                    total = r.total;
                }
                let (p50, p95, max) = p95(ms);
                report.push(format!(
                    "{who}, {name}: total {total}, p50 {p50:.1} ms, p95 {p95:.1} ms, max {max:.1} ms (threshold p95 < 1500 ms)"
                ));
                if p95 >= 1500.0 {
                    failures.push(format!("{who}, {name}: p95 {p95:.1} ms"));
                }
            }
        }
        println!("{}", report.join("\n"));
        assert!(failures.is_empty(), "{failures:?}");
    }
}
