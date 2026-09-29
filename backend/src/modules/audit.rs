//! Read-only view of audit_log. Rows are written by the services in the same
//! transaction as each change; there is no write endpoint and the table is
//! append-only at the database level.

use std::collections::{HashMap, HashSet};

use axum::http::Method;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::PgPool;
use utoipa::openapi::schema::{KnownFormat, ObjectBuilder, Schema, SchemaFormat, Type};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::api::context::{ActorType, RequestContext};
use crate::api::route::{In, Json, NoBody, NoPath, Query, Route, route};
use crate::api::schemas::{self, Page, Paged, Sort, UuidList, like_pattern, ts};
use crate::auth::permissions::{ClassOp, GlobalPermission};
use crate::data::crud::{self, AuditAction, Where};
use crate::http::error::AppError;
use crate::paged;

// Entity types that appear in audit_log.
#[derive(Debug, Clone, Copy, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum EntityType {
    ConfigurationItems,
    CiRelationships,
    CiClasses,
    CiAttributeDefinitions,
    RelationshipTypes,
    RelationshipTypeRules,
    Statuses,
    Environments,
    Locations,
    Owners,
    Users,
    PermissionProfiles,
    UiSettings,
    UiAssets,
    /// Authentication events: sign-in, sign-out, session revocation
    Sessions,
    /// Operator purges (`audit.purge`)
    AuditLog,
    Areas,
    SchemaChanges,
    /// API tokens: created, revoked, and every request made with one (`token.use`)
    ApiTokens,
    /// OIDC providers and LDAP/AD directories, with their group mappings
    IdentityProviders,
    LookupLists,
    LookupListValues,
}

impl EntityType {
    fn as_str(self) -> &'static str {
        match self {
            EntityType::ConfigurationItems => "configuration_items",
            EntityType::CiRelationships => "ci_relationships",
            EntityType::CiClasses => "ci_classes",
            EntityType::CiAttributeDefinitions => "ci_attribute_definitions",
            EntityType::RelationshipTypes => "relationship_types",
            EntityType::RelationshipTypeRules => "relationship_type_rules",
            EntityType::Statuses => "statuses",
            EntityType::Environments => "environments",
            EntityType::Locations => "locations",
            EntityType::Owners => "owners",
            EntityType::Users => "users",
            EntityType::PermissionProfiles => "permission_profiles",
            EntityType::UiSettings => "ui_settings",
            EntityType::UiAssets => "ui_assets",
            EntityType::Sessions => "sessions",
            EntityType::AuditLog => "audit_log",
            EntityType::Areas => "areas",
            EntityType::SchemaChanges => "schema_changes",
            EntityType::ApiTokens => "api_tokens",
            EntityType::IdentityProviders => "identity_providers",
            EntityType::LookupLists => "lookup_lists",
            EntityType::LookupListValues => "lookup_list_values",
        }
    }
}

#[derive(Debug, Serialize, ToSchema, sqlx::FromRow)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AuditEntry {
    pub id: i64,
    #[serde(serialize_with = "ts::serialize")]
    pub occurred_at: DateTime<Utc>,
    #[schema(inline)]
    pub actor_type: ActorType,
    #[schema(required = true)]
    pub actor_id: Option<String>,
    #[schema(required = true)]
    pub actor_name: Option<String>,
    #[schema(inline)]
    pub action: AuditAction,
    /// Table of the changed entity, e.g. configuration_items (sessions for authentication events)
    pub entity_type: String,
    pub entity_id: Uuid,
    /// API representation before the change (null for create)
    #[schema(value_type = serde_json::Value, required = true)]
    pub old_value: Option<Value>,
    /// API representation after the change (null for delete); the details of an authentication event
    #[schema(value_type = serde_json::Value, required = true)]
    pub new_value: Option<Value>,
    #[schema(required = true)]
    pub request_id: Option<String>,
    /// True when oldValue and newValue were withheld because the entry is about
    /// a CI (or a relationship with an endpoint) in a class the caller may not view
    #[sqlx(skip)]
    pub redacted: bool,
}

fn entity_ids_schema() -> Schema {
    schemas::uuid_list_described("History of these entities")
}

fn sort_schema() -> Schema {
    schemas::sort_schema(&["occurredAt"], "-occurredAt")
}

fn timestamp_schema(description: &str) -> Schema {
    ObjectBuilder::new()
        .schema_type(Type::String)
        .format(Some(SchemaFormat::KnownFormat(KnownFormat::DateTime)))
        .description(Some(description))
        .into()
}
fn from_schema() -> Schema {
    timestamp_schema("occurredAt >= from (ISO 8601)")
}
fn to_schema() -> Schema {
    timestamp_schema("occurredAt < to (ISO 8601)")
}
fn actor_name_schema() -> Schema {
    ObjectBuilder::new()
        .schema_type(Type::String)
        .min_length(Some(1))
        .max_length(Some(200))
        .description(Some("Case-insensitive substring"))
        .into()
}

#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct AuditQuery {
    /// Page size (1-200)
    #[param(required = false, default = 50, minimum = 1, maximum = 200)]
    limit: i64,
    /// Rows to skip
    #[param(required = false, default = 0, minimum = 0, maximum = 1_000_000)]
    offset: i64,
    #[param(required = false, schema_with = sort_schema)]
    sort: Sort,
    #[param(inline)]
    entity_type: Option<EntityType>,
    #[param(schema_with = entity_ids_schema)]
    entity_id: Option<UuidList>,
    #[param(inline)]
    action: Option<AuditAction>,
    /// Changes made by this user (their id)
    #[param(max_length = 128)]
    actor_id: Option<String>,
    #[param(schema_with = actor_name_schema)]
    #[serde(default, deserialize_with = "schemas::trimmed_opt")]
    actor_name: Option<String>,
    #[param(max_length = 128)]
    request_id: Option<String>,
    #[param(schema_with = from_schema)]
    from: Option<String>,
    #[param(schema_with = to_schema)]
    to: Option<String>,
}
paged!(AuditQuery);

const COLUMNS: &str = "id, occurred_at, actor_type, actor_id, actor_name, action, entity_type, entity_id, old_value, new_value, request_id";

pub async fn list(pool: &PgPool, ctx: &RequestContext, q: &AuditQuery) -> Result<Page<AuditEntry>, AppError> {
    let filter = |w: &mut Where<'_>| {
        if let Some(t) = q.entity_type {
            w.and().push("entity_type = ").push_bind(t.as_str());
        }
        if let Some(ids) = &q.entity_id {
            w.and().push("entity_id = ANY(").push_bind(ids.0.clone()).push(")");
        }
        if let Some(a) = q.action {
            w.and().push("action = ").push_bind(a.as_str());
        }
        if let Some(id) = &q.actor_id {
            w.and().push("actor_id = ").push_bind(id.clone());
        }
        if let Some(name) = &q.actor_name {
            w.and().push("actor_name ILIKE ").push_bind(like_pattern(name));
        }
        if let Some(id) = &q.request_id {
            w.and().push("request_id = ").push_bind(id.clone());
        }
        if let Some(from) = &q.from {
            w.and().push("occurred_at >= ").push_bind(from.clone()).push("::timestamptz");
        }
        if let Some(to) = &q.to {
            w.and().push("occurred_at < ").push_bind(to.clone()).push("::timestamptz");
        }
    };
    let dir = q.sort.dir();
    let order = format!("occurred_at {dir}, id {dir}");
    let (mut rows, total) = crud::select_page::<AuditEntry>(
        &mut *pool.acquire().await?,
        "audit_log",
        COLUMNS,
        &filter,
        &order,
        q.limit,
        q.offset,
    )
    .await?;
    if let Some(visible) = ctx.class_scope(ClassOp::View) {
        let classes = ci_classes(pool, &referenced_cis(&rows)).await?;
        redact(&mut rows, &classes, &visible.into_iter().collect());
    }
    Ok(Page { data: rows, page: q.page_meta(total) })
}

// CI and relationship entries hold the unredacted item as written, so a caller
// whose view is limited to some classes gets them filtered the way the item
// and relationship endpoints would: an entry about a CI they may not view (or
// an edge with such an endpoint) keeps its metadata but loses both values, and
// a reference attribute into such a CI keeps only its id. Rows are redacted,
// not dropped, so the page and its total stay consistent.

fn uuid_at(value: &Value, key: &str) -> Option<Uuid> {
    value.get(key)?.as_str()?.parse().ok()
}

fn values(e: &AuditEntry) -> impl Iterator<Item = &Value> {
    e.old_value.iter().chain(e.new_value.iter())
}

fn reference_ids(value: &Value) -> impl Iterator<Item = Uuid> + '_ {
    value
        .get("attributeReferences")
        .and_then(Value::as_object)
        .into_iter()
        .flat_map(|refs| refs.values().filter_map(|r| uuid_at(r, "id")))
}

/// Every CI whose class decides what the caller may see of these entries.
fn referenced_cis(rows: &[AuditEntry]) -> Vec<Uuid> {
    let mut ids = HashSet::new();
    for e in rows {
        match e.entity_type.as_str() {
            "configuration_items" => {
                ids.insert(e.entity_id);
                ids.extend(values(e).flat_map(reference_ids));
            }
            "ci_relationships" => {
                ids.extend(values(e).flat_map(|v| [uuid_at(v, "sourceCiId"), uuid_at(v, "targetCiId")]).flatten());
            }
            _ => {}
        }
    }
    ids.into_iter().collect()
}

async fn ci_classes(pool: &PgPool, ids: &[Uuid]) -> Result<HashMap<Uuid, Uuid>, AppError> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let rows: Vec<(Uuid, Uuid)> =
        sqlx::query_as("SELECT id, class_id FROM cmdb.configuration_items WHERE id = ANY($1)")
            .bind(ids)
            .fetch_all(pool)
            .await?;
    Ok(rows.into_iter().collect())
}

/// `classes`: the current class of each CI in [`referenced_cis`]; `visible`: the classes the caller may view.
fn redact(rows: &mut [AuditEntry], classes: &HashMap<Uuid, Uuid>, visible: &HashSet<Uuid>) {
    let can_view = |ci: Uuid| classes.get(&ci).is_some_and(|c| visible.contains(c));
    for e in rows {
        let shown = match e.entity_type.as_str() {
            // A CI can change class, so the class it had in either value must be viewable as well.
            "configuration_items" => {
                can_view(e.entity_id)
                    && values(e).all(|v| {
                        v.get("classId").is_none() || uuid_at(v, "classId").is_some_and(|c| visible.contains(&c))
                    })
            }
            "ci_relationships" => values(e).all(|v| {
                [uuid_at(v, "sourceCiId"), uuid_at(v, "targetCiId")].into_iter().all(|id| id.is_some_and(can_view))
            }),
            _ => continue,
        };
        if !shown {
            e.old_value = None;
            e.new_value = None;
            e.redacted = true;
        } else if e.entity_type == "configuration_items" {
            for v in e.old_value.iter_mut().chain(e.new_value.iter_mut()) {
                hide_references(v, &can_view);
            }
        }
    }
}

/// The same placeholder the item endpoints return for a reference the caller may not follow.
fn hide_references(value: &mut Value, can_view: &impl Fn(Uuid) -> bool) {
    let Some(refs) = value.get_mut("attributeReferences").and_then(Value::as_object_mut) else { return };
    for r in refs.values_mut() {
        let Some(id) = uuid_at(r, "id") else {
            *r = Value::Null;
            continue;
        };
        if !can_view(id) {
            *r = serde_json::json!({ "id": id, "name": null, "deleted": false, "hidden": true });
        }
    }
}

pub fn routes() -> Vec<Route> {
    vec![
        route(Method::GET, "/api/v1/audit-log", "listAuditLog")
            .tag("Audit log")
            .summary("Change history (read-only, paginated, newest first by default)")
            .description(
                "Every change made through the API records the signed-in user as the actor (`actorType` user, `actorId` their id, `actorName` their username). Authentication events are recorded too, with `entityType` sessions: `login.success`, `login.failure`, `login.locked`, `logout` and `session.revoke`; `oldValue` is null and `newValue` holds the details (user, `ipAddress`, `userAgent`, reason). A failed sign-in has no actor id and records the attempted username as typed (first 64 characters), with nothing saying whether it exists. API tokens (`entityType` api_tokens) record `create` and `update` (revocation), and a `token.use` row for every request made with a known token, accepted or refused: `newValue` holds the token's name and prefix, owner, `outcome` (accepted, revoked, expired, owner_disabled, mfa_required, no_scope, session_only, forbidden), method, path (first 512 characters, then `…`, with `pathLength`), `operationId`, `ipAddress` and `userAgent`; a token that can no longer authenticate (revoked, expired, owner_disabled, mfa_required, no_scope) is recorded at most once a minute per outcome, the next row counting the uses left out in `unrecordedRefusals`. Changes made with a token have `actorType` api_client and the owner as actor; the `token.use` row shares their `requestId`. Two-factor authentication events have `entityType` users and the user's id: `mfa.enrol`, `mfa.disable` (`reason` self_service or admin_reset), `mfa.failure` (a wrong or replayed code; `stage` login, disable or recovery_codes), `mfa.recovery_code_used` (with `recoveryCodesRemaining`) and `mfa.recovery_codes` (new codes replaced the old); sign-ins record `method` password, totp, recovery_code, setup, oidc or ldap in `login.success`. Identity providers (`entityType` identity_providers) record `create`, `update` and `delete` without their secrets; an account an identity provider creates or updates at sign-in is a `create` or `update` row on `users` with `actorType` system and `actorName` `identity provider \"<name>\"`, and a sign-in the provider vouched for but ShadouCMDB refused is a `login.failure`. A data model preview refused for a missing right (e.g. a field type change by a user who may not view every type storing the field) is a `schema_change.refused` row on the area, type or field previewed, `newValue` holding the operation, the body sent, `code`, `field` and `message`. An operator's `shadoucmdb prune-audit` leaves an `audit.purge` row (`entityType` audit_log, `actorType` system, `actorName` the database user) whose `newValue` holds the scope, window, cutoff and the number of rows deleted per action; those rows are never pruned. A caller whose profile limits the classes they may view gets `oldValue` and `newValue` withheld (both null, `redacted` true) on entries about a CI of another class and on relationship entries with an endpoint in one, judged by the CIs' current classes and, for a CI, every class it had in either value; in the CI entries they may see, a reference attribute into a CI they may not view keeps only its id (`attributeReferences` shows it hidden, as the item endpoints do). Passwords (local or directory), session tokens, CSRF tokens, API token secrets, TOTP secrets, authenticator or recovery codes, OIDC client secrets, authorization codes and ID tokens, and LDAP bind passwords are never recorded.",
            )
            .requires(GlobalPermission::AuditView)
            .handle(|api, In(NoPath, Query(q), NoBody): In<NoPath, Query<AuditQuery>, NoBody>| async move {
                Ok(Json(list(&api.pool, &api.ctx, &q).await?))
            }),
    ]
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn id(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    fn entry(entity_type: &str, entity_id: Uuid, old_value: Option<Value>, new_value: Option<Value>) -> AuditEntry {
        AuditEntry {
            id: 1,
            occurred_at: Utc::now(),
            actor_type: ActorType::User,
            actor_id: None,
            actor_name: None,
            action: AuditAction::Update,
            entity_type: entity_type.into(),
            entity_id,
            old_value,
            new_value,
            request_id: None,
            redacted: false,
        }
    }

    fn ci(class: Uuid, reference: Uuid) -> Value {
        json!({
            "classId": class,
            "attributes": { "hostname": "db01.corp.example", "runs_on": reference },
            "attributeReferences": { "runs_on": { "id": reference, "name": "secret-host", "deleted": false, "hidden": false } },
        })
    }

    fn edge(source: Uuid, target: Uuid) -> Value {
        json!({ "sourceCiId": source, "targetCiId": target, "source": { "name": "a" }, "target": { "name": "b" } })
    }

    // Classes 10 (viewable) and 20 (not); CIs 1 and 3 are in class 10, CI 2 in class 20.
    fn run(mut rows: Vec<AuditEntry>) -> Vec<AuditEntry> {
        let classes = HashMap::from([(id(1), id(10)), (id(2), id(20)), (id(3), id(10))]);
        let mut ids = referenced_cis(&rows);
        ids.sort();
        assert!(ids.iter().all(|i| classes.contains_key(i) || *i == id(9)), "{ids:?}");
        redact(&mut rows, &classes, &HashSet::from([id(10)]));
        rows
    }

    #[test]
    fn a_ci_of_a_class_the_caller_cannot_view_loses_its_values() {
        let rows = run(vec![entry("configuration_items", id(2), Some(ci(id(20), id(1))), Some(ci(id(20), id(1))))]);
        assert!(rows[0].redacted);
        assert_eq!((&rows[0].old_value, &rows[0].new_value), (&None, &None));
    }

    #[test]
    fn a_viewable_ci_keeps_its_values_but_hides_references_into_other_classes() {
        let rows = run(vec![entry("configuration_items", id(1), None, Some(ci(id(10), id(2))))]);
        assert!(!rows[0].redacted);
        let v = rows[0].new_value.as_ref().unwrap();
        assert_eq!(v["attributes"]["hostname"], "db01.corp.example");
        assert_eq!(
            v["attributeReferences"]["runs_on"],
            json!({ "id": id(2), "name": null, "deleted": false, "hidden": true })
        );

        let rows = run(vec![entry("configuration_items", id(1), None, Some(ci(id(10), id(3))))]);
        assert_eq!(rows[0].new_value.as_ref().unwrap()["attributeReferences"]["runs_on"]["name"], "secret-host");
    }

    #[test]
    fn a_reference_to_an_unknown_ci_is_hidden() {
        let rows = run(vec![entry("configuration_items", id(1), None, Some(ci(id(10), id(9))))]);
        assert_eq!(rows[0].new_value.as_ref().unwrap()["attributeReferences"]["runs_on"]["hidden"], true);
    }

    #[test]
    fn a_ci_that_was_in_another_class_is_withheld() {
        let rows = run(vec![entry("configuration_items", id(1), Some(ci(id(20), id(3))), Some(ci(id(10), id(3))))]);
        assert!(rows[0].redacted);
    }

    #[test]
    fn relationships_need_both_endpoints_viewable() {
        let rows = run(vec![
            entry("ci_relationships", id(50), None, Some(edge(id(1), id(3)))),
            entry("ci_relationships", id(51), None, Some(edge(id(1), id(2)))),
            entry("ci_relationships", id(52), Some(edge(id(2), id(3))), None),
            entry("ci_relationships", id(53), Some(edge(id(1), id(3))), Some(edge(id(1), id(9)))),
        ]);
        assert_eq!(rows.iter().map(|r| r.redacted).collect::<Vec<_>>(), [false, true, true, true]);
        assert!(rows[0].new_value.is_some());
    }

    #[test]
    fn other_entity_types_are_left_alone() {
        let rows = run(vec![entry("ci_classes", id(20), None, Some(json!({ "id": id(20), "name": "Server" })))]);
        assert!(!rows[0].redacted);
        assert!(rows[0].new_value.is_some());
    }
}
