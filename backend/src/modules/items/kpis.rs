//! Dashboard KPIs (SHAA-2350): how complete the inventory's records are, and
//! how many CIs and relationships there were over time.
//!
//! Both read only. Completeness counts per class the fields a complete CI holds
//! a value for (required or `isExpected`, or every active field). Count
//! histories are derived from the timestamps of the registry rows, see
//! [`crate::data::counts`].

use std::collections::HashSet;

use chrono::{DateTime, Datelike, DurationRound, TimeDelta, Utc};
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use super::schemas::{
    ActiveQuery, ClassCompleteness, Completeness, CompletenessBasis, CompletenessCounts, CompletenessField,
    CompletenessQuery, CountActive, CountBucket, CountHistory, CountHistoryBucket, ItemCompleteness,
    ItemCountHistoryQuery, ItemFilterQuery, KindQuery, RelationshipCountHistoryQuery,
};
use super::service::{filters, inventory_filters, query_error};
use crate::api::context::RequestContext;
use crate::api::schemas::{Deleted, LookupRef, UuidList};
use crate::auth::permissions::ClassOp;
use crate::data::counts::{self, CountEvents};
use crate::data::items::{self as data, ItemFilters};
use crate::data::relationships as relationship_data;
use crate::http::error::AppError;
use crate::schema::model::{Field, Model};

/// Whether a field counts for completeness on this basis.
fn counted(f: &Field, basis: CompletenessBasis) -> bool {
    match basis {
        CompletenessBasis::Expected => f.counts_for_completeness(),
        CompletenessBasis::All => f.is_active,
    }
}

/// The counted fields of a class (its ancestors' first), in form order per class.
fn counted_fields(model: &Model, class_id: Uuid, basis: CompletenessBasis) -> Vec<&Field> {
    model.lineage(class_id).into_iter().flat_map(|c| model.own_fields(c.id)).filter(|f| counted(f, basis)).collect()
}

/// A read-only snapshot, so the per-class counts add up to the overall ones.
async fn snapshot(pool: &PgPool) -> Result<sqlx::Transaction<'static, sqlx::Postgres>, AppError> {
    let mut tx = pool.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY").execute(&mut *tx).await?;
    Ok(tx)
}

/// Completeness of the CIs the inventory list returns for the same filters.
pub async fn completeness(
    pool: &PgPool,
    ctx: &RequestContext,
    q: &CompletenessQuery,
) -> Result<Completeness, AppError> {
    let mut tx = snapshot(pool).await?;
    let conn: &mut PgConnection = &mut tx;
    let model = Model::load(conn).await?;
    let own_layout = q.own_layout.map(bool::from);
    let f = inventory_filters(conn, ctx, &model, q, q.q.as_deref(), own_layout, q.layout_template.as_deref()).await?;
    let mut overall = CompletenessCounts::default();
    let mut classes = Vec::new();
    for (id, key, name, items) in data::counts_per_class(conn, &f).await? {
        let fields = counted_fields(&model, id, q.basis);
        let counts = if fields.is_empty() {
            CompletenessCounts { items, complete_items: items, expected_values: 0, filled_values: 0 }
        } else {
            let (items, complete_items, filled_values) =
                data::completeness_of_class(conn, &model, id, &fields, &f).await?;
            CompletenessCounts { items, complete_items, expected_values: items * fields.len() as i64, filled_values }
        };
        overall.items += counts.items;
        overall.complete_items += counts.complete_items;
        overall.expected_values += counts.expected_values;
        overall.filled_values += counts.filled_values;
        classes.push(ClassCompleteness {
            class: LookupRef { id, key, name },
            counted_fields: fields.len() as i64,
            counts,
        });
    }
    tx.commit().await?;
    Ok(Completeness { basis: q.basis, overall, classes })
}

/// Which counted fields of one CI hold a value.
pub async fn item_completeness(
    pool: &PgPool,
    ctx: &RequestContext,
    id: Uuid,
    basis: CompletenessBasis,
) -> Result<ItemCompleteness, AppError> {
    let mut conn = pool.acquire().await?;
    let Some(row) = data::summary(&mut conn, id).await? else {
        return Err(AppError::missing("Configuration item", id));
    };
    ctx.require_class_visible(row.class_id, "Configuration item", id)?;
    let model = Model::load(&mut conn).await?;
    let held: HashSet<String> = data::values(&mut conn, &model, &[id]).await?.into_iter().map(|v| v.key).collect();
    let fields: Vec<CompletenessField> = counted_fields(&model, row.class_id, basis)
        .into_iter()
        .map(|f| CompletenessField {
            key: f.key.clone(),
            label: f.label.clone(),
            is_required: f.is_required,
            is_expected: f.is_expected,
            filled: held.contains(&f.key),
        })
        .collect();
    let filled_fields = fields.iter().filter(|f| f.filled).count() as i64;
    Ok(ItemCompleteness {
        id,
        basis,
        complete: filled_fields == fields.len() as i64,
        counted_fields: fields.len() as i64,
        filled_fields,
        fields,
    })
}

/// Only the class filter applies to a CI count (see [`item_count_history`]).
impl ItemFilterQuery for ItemCountHistoryQuery {
    fn class_id(&self) -> Option<&UuidList> {
        self.class_id.as_ref()
    }
    fn include_subclasses(&self) -> bool {
        self.include_subclasses.into()
    }
    fn active(&self) -> ActiveQuery {
        ActiveQuery::All
    }
    fn lookup_value_id(&self) -> Option<&UuidList> {
        None
    }
    fn ip_within(&self) -> Option<&str> {
        None
    }
    fn criticality_value_id(&self) -> Option<&UuidList> {
        None
    }
    fn deleted(&self) -> Deleted {
        Deleted::Include
    }
    fn kind(&self) -> Option<KindQuery> {
        None
    }
    fn business_service_id(&self) -> Option<&UuidList> {
        None
    }
}

/// The start of the bucket holding `t`: its UTC day, or the Monday of its ISO week.
fn bucket_start(bucket: CountBucket, t: DateTime<Utc>) -> DateTime<Utc> {
    let day = t.duration_trunc(TimeDelta::days(1)).unwrap_or(t);
    match bucket {
        CountBucket::Day => day,
        CountBucket::Week => day - TimeDelta::days(day.weekday().num_days_from_monday().into()),
    }
}

/// `[from, to)` with `from` rounded down to its bucket, checked against the cap.
fn count_range(
    bucket: CountBucket,
    from: Option<DateTime<Utc>>,
    to: Option<DateTime<Utc>>,
) -> Result<(DateTime<Utc>, DateTime<Utc>), AppError> {
    let (_, cap) = bucket.width_and_cap();
    let to = to.unwrap_or_else(Utc::now);
    let from = from.unwrap_or_else(|| match bucket {
        CountBucket::Day => to - TimeDelta::days(30),
        CountBucket::Week => to - TimeDelta::weeks(12),
    });
    if from >= to {
        return Err(query_error("from", "Must be before `to`", "invalid_range"));
    }
    let from = bucket_start(bucket, from);
    if to - from > cap {
        let message = match bucket {
            CountBucket::Day => format!("The range may span at most {} days with day buckets", cap.num_days()),
            CountBucket::Week => format!("The range may span at most {} weeks with week buckets", cap.num_weeks()),
        };
        return Err(query_error("from", &message, "range_too_large"));
    }
    Ok((from, to))
}

fn unit(bucket: CountBucket) -> &'static str {
    match bucket {
        CountBucket::Day => "day",
        CountBucket::Week => "week",
    }
}

/// Every bucket of `[from, to)` with its running count, filled from the non-empty ones.
fn history(
    bucket: CountBucket,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    at_from: i64,
    events: Vec<CountEvents>,
) -> CountHistory {
    let (width, _) = bucket.width_and_cap();
    let mut events = events.into_iter().filter_map(|e| Some((e.start?, e.added, e.removed))).peekable();
    let mut count = at_from;
    let mut buckets = Vec::new();
    let mut start = from;
    while start < to {
        let mut b = CountHistoryBucket { start, count, added: 0, removed: 0 };
        while let Some((_, added, removed)) = events.next_if(|(s, _, _)| *s < start + width) {
            b.added += added;
            b.removed += removed;
        }
        count += b.added - b.removed;
        b.count = count;
        buckets.push(b);
        start += width;
    }
    CountHistory { from, to, bucket, count_at_from: at_from, buckets }
}

/// CIs per bucket in classes the caller may view. Process records are left out
/// unless their type is named, as in the inventory list.
pub async fn item_count_history(
    pool: &PgPool,
    ctx: &RequestContext,
    q: &ItemCountHistoryQuery,
) -> Result<CountHistory, AppError> {
    let (from, to) = count_range(q.bucket, q.from, q.to)?;
    let mut conn = pool.acquire().await?;
    let model = Model::load(&mut conn).await?;
    let f = ItemFilters { visible_class_ids: ctx.class_scope(ClassOp::View), ..filters(&mut conn, &model, q).await? };
    let validity = q.active == CountActive::True;
    let (at_from, events) =
        counts::count_history(&mut conn, |qb| data::push_count_spans(qb, &f, validity), unit(q.bucket), from, to)
            .await?;
    Ok(history(q.bucket, from, to, at_from, events))
}

/// Relationships per bucket whose both CIs are in classes the caller may view.
pub async fn relationship_count_history(
    pool: &PgPool,
    ctx: &RequestContext,
    q: &RelationshipCountHistoryQuery,
) -> Result<CountHistory, AppError> {
    let (from, to) = count_range(q.bucket, q.from, q.to)?;
    let visible = ctx.class_scope(ClassOp::View);
    let types = q.relationship_type_id.as_ref().map(|l| l.0.as_slice());
    let mut conn = pool.acquire().await?;
    let (at_from, events) = counts::count_history(
        &mut conn,
        |qb| relationship_data::push_count_spans(qb, types, visible.as_deref()),
        unit(q.bucket),
        from,
        to,
    )
    .await?;
    Ok(history(q.bucket, from, to, at_from, events))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use chrono::{DateTime, Datelike, DurationRound, TimeDelta, Utc};
    use serde_json::{Value, json};
    use uuid::Uuid;

    use super::super::schemas::{CountActive, CountBucket, ItemCountHistoryQuery};
    use crate::api::context::RequestContext;
    use crate::api::schemas::{QueryBool, UuidList};
    use crate::auth::permissions::{ClassRights, Permissions};
    use crate::auth::{Credential, Principal};
    use crate::db::scratch;
    use crate::modules::api_tokens::tests::{Creds, app, call, code, session_of};

    /// A user who may view only these classes.
    fn viewer(classes: &[Uuid]) -> RequestContext {
        let permissions = Permissions {
            classes: classes.iter().map(|id| (*id, ClassRights { view: true, ..Default::default() })).collect(),
            ..Default::default()
        };
        let principal = Principal {
            user_id: Uuid::new_v4(),
            username: "viewer".into(),
            credential: Credential::Token { profile_id: None, creator_id: None, token_id: None, minted_by: None },
            permissions,
        };
        RequestContext::user(Arc::new(principal), "kpi-viewer".into())
    }

    fn id(v: &Value) -> Uuid {
        v["id"].as_str().unwrap().parse().unwrap()
    }

    fn enc(t: DateTime<Utc>) -> String {
        t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true).replace('+', "%2B")
    }

    /// (count, added, removed) per bucket.
    fn buckets(v: &Value) -> Vec<(i64, i64, i64)> {
        v["buckets"]
            .as_array()
            .unwrap()
            .iter()
            .map(|b| (b["count"].as_i64().unwrap(), b["added"].as_i64().unwrap(), b["removed"].as_i64().unwrap()))
            .collect()
    }

    #[tokio::test]
    async fn completeness_and_count_history() {
        let Some(db) = scratch::database("dashboard_kpis").await else { return };
        let pool = &db.pool;
        let app = app(pool.clone());
        let setup = json!({ "username": "owner", "email": "owner@example.test", "displayName": "Owner", "password": "correct horse battery", "setupToken": crate::auth::setup_token::TEST_TOKEN });
        let (status, me, headers) = call(&app, "POST", "/api/v1/setup", &Creds::default(), Some(setup)).await;
        assert_eq!(status, 201, "{me}");
        let admin = session_of(&me, &headers);

        // srv (owner expected, notes optional) and its subclass vm (cores expected).
        let (status, v, _) =
            call(&app, "POST", "/api/v1/ci-classes", &admin, Some(json!({ "key": "srv", "name": "Srv" }))).await;
        assert_eq!(status, 201, "{v}");
        let srv = id(&v);
        let body = json!({ "key": "vm", "name": "VM", "parentId": srv });
        let (status, v, _) = call(&app, "POST", "/api/v1/ci-classes", &admin, Some(body)).await;
        assert_eq!(status, 201, "{v}");
        let vm = id(&v);
        let mut owner_field = Uuid::nil();
        for (class, key, ty, expected) in
            [(srv, "owner", "text", true), (srv, "notes", "text", false), (vm, "cores", "number", true)]
        {
            let body = json!({ "classId": class, "key": key, "label": key, "dataType": ty, "isExpected": expected });
            let (status, v, _) = call(&app, "POST", "/api/v1/attribute-definitions", &admin, Some(body)).await;
            assert_eq!(status, 201, "{v}");
            assert_eq!(v["isExpected"], json!(expected), "{v}");
            if key == "owner" {
                owner_field = id(&v);
            }
        }

        let mut cis = Vec::new();
        for (class, attrs) in [
            (srv, json!({ "owner": "ops", "notes": "x" })),
            (srv, json!({})),
            (vm, json!({ "owner": "ops", "cores": 4 })),
            (vm, json!({ "cores": 2 })),
        ] {
            let body = json!({ "classId": class, "attributes": attrs });
            let (status, v, _) = call(&app, "POST", "/api/v1/configuration-items", &admin, Some(body)).await;
            assert_eq!(status, 201, "{v}");
            cis.push(id(&v));
        }
        let [srv1, srv2, vm1, vm2] = cis[..] else { unreachable!() };

        // G1: srv counts owner (1 field), vm owner and cores (2).
        let path = format!("/api/v1/configuration-items/completeness?classId={srv}");
        let (status, v, _) = call(&app, "GET", &path, &admin, None).await;
        assert_eq!(status, 200, "{v}");
        assert_eq!(v["basis"], "expected");
        assert_eq!(v["overall"], json!({ "items": 4, "completeItems": 2, "expectedValues": 6, "filledValues": 4 }));
        let classes = v["classes"].as_array().unwrap();
        assert_eq!(classes.len(), 2, "{v}");
        let vm_row = classes.iter().find(|c| c["class"]["key"] == "vm").unwrap();
        assert_eq!(vm_row["countedFields"], 2);
        assert_eq!(vm_row["counts"], json!({ "items": 2, "completeItems": 1, "expectedValues": 4, "filledValues": 3 }));

        // basis=all counts notes too.
        let (status, v, _) = call(&app, "GET", &format!("{path}&basis=all"), &admin, None).await;
        assert_eq!(status, 200, "{v}");
        let srv_row = v["classes"].as_array().unwrap().iter().find(|c| c["class"]["key"] == "srv").unwrap().clone();
        assert!(srv_row["countedFields"].as_i64().unwrap() >= 2, "{v}");
        assert!(srv_row["counts"]["completeItems"].as_i64().unwrap() <= 1, "{v}");

        // Per CI: srv2 lacks its owner.
        let (status, v, _) =
            call(&app, "GET", &format!("/api/v1/configuration-items/{srv2}/completeness"), &admin, None).await;
        assert_eq!(status, 200, "{v}");
        assert_eq!(v["complete"], false);
        assert_eq!(
            v["fields"],
            json!([{ "key": "owner", "label": "owner", "isRequired": false, "isExpected": true, "filled": false }])
        );
        let (status, v, _) =
            call(&app, "GET", &format!("/api/v1/configuration-items/{vm1}/completeness"), &admin, None).await;
        assert_eq!(status, 200, "{v}");
        assert_eq!(
            (v["complete"].clone(), v["countedFields"].clone(), v["filledFields"].clone()),
            (json!(true), json!(2), json!(2))
        );
        let (status, v, _) =
            call(&app, "GET", &format!("/api/v1/configuration-items/{}/completeness", Uuid::new_v4()), &admin, None)
                .await;
        assert_eq!((status, code(&v)), (404, "NOT_FOUND"), "{v}");

        // Unmarking a field is an audited change and leaves it out of the count.
        let body = json!({ "isExpected": false });
        let (status, v, _) =
            call(&app, "PATCH", &format!("/api/v1/attribute-definitions/{owner_field}"), &admin, Some(body)).await;
        assert_eq!(status, 200, "{v}");
        let audited: Option<Value> = sqlx::query_scalar(
            "SELECT new_value FROM audit_log WHERE entity_id = $1 AND action = 'update' ORDER BY id DESC LIMIT 1",
        )
        .bind(owner_field)
        .fetch_optional(pool)
        .await
        .unwrap();
        assert_eq!(audited.as_ref().map(|n| n["isExpected"].clone()), Some(json!(false)), "{audited:?}");
        let (status, v, _) = call(&app, "GET", &path, &admin, None).await;
        assert_eq!(status, 200, "{v}");
        assert_eq!(v["overall"], json!({ "items": 4, "completeItems": 4, "expectedValues": 2, "filledValues": 2 }));

        // G2: CIs backdated around a fixed day D.
        let d = Utc::now().duration_trunc(TimeDelta::days(1)).unwrap();
        let h = TimeDelta::hours;
        let days = TimeDelta::days;
        for (ci, created, deleted) in [
            (srv1, d - days(10), None),
            (srv2, d - days(3) + h(1), Some(d - days(1) + h(1))),
            (vm1, d - days(2) + h(5), None),
            (vm2, d - days(20), None),
        ] {
            sqlx::query("UPDATE configuration_items SET created_at = $2, deleted_at = $3 WHERE id = $1")
                .bind(ci)
                .bind(created)
                .bind(deleted)
                .execute(pool)
                .await
                .unwrap();
        }
        let range = format!("from={}&to={}", enc(d - days(4)), enc(d));
        let path = format!("/api/v1/configuration-items/count-history?{range}&classId={srv}&active=all");
        let (status, v, _) = call(&app, "GET", &path, &admin, None).await;
        assert_eq!(status, 200, "{v}");
        assert_eq!(v["countAtFrom"], 2, "{v}");
        assert_eq!(buckets(&v), vec![(2, 0, 0), (3, 1, 0), (4, 1, 0), (3, 0, 1)]);

        // A viewer of srv alone does not count vm CIs.
        let q = ItemCountHistoryQuery {
            from: Some(d - days(4)),
            to: Some(d),
            bucket: CountBucket::Day,
            class_id: Some(UuidList(vec![srv])),
            include_subclasses: QueryBool::True,
            active: CountActive::All,
        };
        let h1 = super::item_count_history(pool, &viewer(&[srv]), &q).await.unwrap();
        let counts: Vec<i64> = h1.buckets.iter().map(|b| b.count).collect();
        assert_eq!((h1.count_at_from, counts), (1, vec![1, 2, 2, 1]));

        // Weeks: from rounds down to Monday; the last count matches the day view.
        let path = format!(
            "/api/v1/configuration-items/count-history?from={}&to={}&bucket=week&classId={srv}&active=all",
            enc(d - days(30)),
            enc(d)
        );
        let (status, v, _) = call(&app, "GET", &path, &admin, None).await;
        assert_eq!(status, 200, "{v}");
        let from: DateTime<Utc> = v["from"].as_str().unwrap().parse().unwrap();
        assert_eq!(from.weekday(), chrono::Weekday::Mon, "{v}");
        assert_eq!(buckets(&v).last().unwrap().0, 3, "{v}");

        // Range checks.
        let path = format!("/api/v1/configuration-items/count-history?from={}&to={}", enc(d), enc(d - days(1)));
        let (status, v, _) = call(&app, "GET", &path, &admin, None).await;
        assert_eq!((status, v["error"]["details"][0]["code"].as_str()), (400, Some("invalid_range")), "{v}");
        let path = format!("/api/v1/configuration-items/count-history?from={}&to={}", enc(d - days(400)), enc(d));
        let (status, v, _) = call(&app, "GET", &path, &admin, None).await;
        assert_eq!((status, v["error"]["details"][0]["code"].as_str()), (400, Some("range_too_large")), "{v}");

        // Relationships: e1 srv1 -> vm1 since D-3, e2 srv1 -> vm2 from D-10 to D-2.
        let rt: Uuid = sqlx::query_scalar(
            "INSERT INTO relationship_types (key, name, forward_label, reverse_label, is_directional)
             VALUES ('uses', 'Uses', 'uses', 'used by', true) RETURNING id",
        )
        .fetch_one(pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO relationship_type_rules (relationship_type_id, source_class_id, target_class_id) VALUES ($1, $2, $2)",
        )
        .bind(rt)
        .bind(srv)
        .execute(pool)
        .await
        .unwrap();
        for (target, created, deleted) in
            [(vm1, d - days(3) + h(2), None), (vm2, d - days(10), Some(d - days(2) + h(1)))]
        {
            sqlx::query(
                "INSERT INTO ci_relationships (relationship_type_id, source_ci_id, target_ci_id, created_at, deleted_at)
                 VALUES ($1, $2, $3, $4, $5)",
            )
            .bind(rt)
            .bind(srv1)
            .bind(target)
            .bind(created)
            .bind(deleted)
            .execute(pool)
            .await
            .unwrap();
        }
        let path = format!("/api/v1/relationships/count-history?{range}&relationshipTypeId={rt}");
        let (status, v, _) = call(&app, "GET", &path, &admin, None).await;
        assert_eq!(status, 200, "{v}");
        assert_eq!(v["countAtFrom"], 1, "{v}");
        assert_eq!(buckets(&v), vec![(1, 0, 0), (2, 1, 0), (1, 0, 1), (1, 0, 0)]);
        let q = super::super::schemas::RelationshipCountHistoryQuery {
            from: Some(d - days(4)),
            to: Some(d),
            bucket: CountBucket::Day,
            relationship_type_id: None,
        };
        let h2 = super::relationship_count_history(pool, &viewer(&[srv]), &q).await.unwrap();
        assert!(h2.count_at_from == 0 && h2.buckets.iter().all(|b| b.count == 0), "{h2:?}");
    }
}
