//! Read-only view of audit_log. Rows are written by the services in the same
//! transaction as each change; there is no write endpoint and the table is
//! append-only at the database level.

use axum::http::Method;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::PgPool;
use utoipa::openapi::schema::{KnownFormat, ObjectBuilder, Schema, SchemaFormat, Type};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::api::context::ActorType;
use crate::api::route::{In, Json, NoBody, NoPath, Query, Route, route};
use crate::api::schemas::{self, Page, Paged, Sort, UuidList, like_pattern, ts};
use crate::auth::permissions::GlobalPermission;
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
    /// Table of the changed entity, e.g. configuration_items
    pub entity_type: String,
    pub entity_id: Uuid,
    /// API representation before the change (null for create)
    #[schema(value_type = serde_json::Value, required = true)]
    pub old_value: Option<Value>,
    /// API representation after the change (null for delete)
    #[schema(value_type = serde_json::Value, required = true)]
    pub new_value: Option<Value>,
    #[schema(required = true)]
    pub request_id: Option<String>,
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

pub async fn list(pool: &PgPool, q: &AuditQuery) -> Result<Page<AuditEntry>, AppError> {
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
    let (rows, total) =
        crud::select_page::<AuditEntry>(pool, "audit_log", COLUMNS, &filter, &order, q.limit, q.offset).await?;
    Ok(Page { data: rows, page: q.page_meta(total) })
}

pub fn routes() -> Vec<Route> {
    vec![
        route(Method::GET, "/api/v1/audit-log", "listAuditLog")
            .tag("Audit log")
            .summary("Change history (read-only, paginated, newest first by default)")
            .description("Requires `audit.view`. Every change made through the API records the signed-in user as the actor (`actorType` user, `actorId` their id, `actorName` their username).")
            .requires(GlobalPermission::AuditView)
            .handle(|api, In(NoPath, Query(q), NoBody): In<NoPath, Query<AuditQuery>, NoBody>| async move {
                Ok(Json(list(&api.pool, &q).await?))
            }),
    ]
}
