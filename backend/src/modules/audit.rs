//! Read-only view of audit_log. Rows are written by the services in the same
//! transaction as each change; there is no write endpoint and the table is
//! append-only at the database level.

use std::collections::{HashMap, HashSet};

use axum::http::Method;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{PgPool, Postgres, QueryBuilder};
use utoipa::openapi::schema::{KnownFormat, ObjectBuilder, Schema, SchemaFormat, Type};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::api::context::{ActorType, RequestContext};
use crate::api::route::{In, Json, NoBody, NoPath, Query, Route, route};
use crate::api::schemas::{self, Page, Paged, Sort, UuidList, like_pattern, ts};
use crate::api::validate;
use crate::auth::permissions::{ClassOp, GlobalPermission};
use crate::data::crud::{self, AuditAction, Where};
use crate::http::error::AppError;
use crate::modules::schema_changes;
use crate::paged;
use crate::schema::SchemaChange;
use crate::secrets::Keyring;

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
    /// Bulk import jobs: `import.commit` per commit, `import.report_read`
    ImportJobs,
    /// The bulk import switch
    ImportSettings,
    /// Saved bulk import column mappings
    ImportMappings,
    /// User groups (owners of business services): create, update (members too), delete
    UserGroups,
    /// Shared saved views: create, update, delete (personal views are not audited)
    SavedViews,
    /// Configuration file downloads (`export`; entity id is the nil UUID)
    Config,
    /// A CI's own detail page layout (entity id: the CI's id): create, update, delete
    CiLayoutOverrides,
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
            EntityType::ImportJobs => "import_jobs",
            EntityType::ImportSettings => "import_settings",
            EntityType::ImportMappings => "import_mappings",
            EntityType::UserGroups => "user_groups",
            EntityType::SavedViews => "saved_views",
            EntityType::Config => "config",
            EntityType::CiLayoutOverrides => "ci_layout_overrides",
        }
    }
}

#[derive(Debug, Serialize, ToSchema, sqlx::FromRow)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AuditEntry {
    /// Identifies the entry. A caller whose profile limits the classes they may
    /// view gets a stable, distinct id per entry that says nothing about order
    /// (it changes when the encryption key is rotated); others get the stored
    /// sequence number. Use `occurredAt` and the list order to order entries.
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
    /// True when oldValue and newValue were withheld: a schema change entry
    /// whose record no longer exists, or a CI entry whose CI moved to a class
    /// the caller may not view while the page was read (entries about CIs the
    /// caller may not view are otherwise left out of the list)
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

pub async fn list(
    pool: &PgPool,
    ctx: &RequestContext,
    keyring: &Keyring,
    q: &AuditQuery,
) -> Result<Page<AuditEntry>, AppError> {
    let scope = ctx.class_scope(ClassOp::View);
    let filter = |w: &mut Where<'_>| {
        if let Some(visible) = &scope {
            push_visible(w.and(), visible);
        }
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
    if let Some(visible) = scope {
        let edges = relationship_endpoints(pool, &path_relationships(&rows)).await?;
        let mut cis = referenced_cis(&rows);
        cis.extend(edges.values().flat_map(|&(source, target)| [source, target]));
        let classes = ci_classes(pool, &cis).await?;
        let changes = schema_changes::visible_changes(pool, &referenced_changes(&rows), &visible).await?;
        let fields = field_classes(pool, &reported_fields(&rows)).await?;
        let visible = visible.into_iter().collect();
        redact(&mut rows, &classes, &changes, &fields, &visible);
        hide_path_ids(&mut rows, &classes, &edges, &visible);
        withhold_writer_counts(&mut rows, ctx.principal().map(|p| p.user_id.to_string()).as_deref());
        if rows.iter().any(|e| e.entity_type == "saved_views") {
            let ids: Vec<Uuid> = visible.iter().copied().collect();
            let keys: Vec<String> = sqlx::query_scalar("SELECT key FROM cmdb.ci_classes WHERE id = ANY($1)")
                .bind(ids)
                .fetch_all(pool)
                .await?;
            hide_view_classes(&mut rows, &keys.into_iter().collect());
        }
        // Raw ids would show, by a gap, that a row left out sits between two shown (GH#378).
        for e in &mut rows {
            e.id = keyring.audit_row_id(e.id);
        }
    }
    Ok(Page { data: rows, page: q.page_meta(total) })
}

// CI and relationship entries hold the unredacted item as written, so a caller
// whose view is limited to some classes gets them filtered the way the item
// and relationship endpoints would. An entry about a CI they may not view (or
// an edge with such an endpoint) is left out in SQL by [`push_visible`], so
// neither the page, its total nor any filter (entityId, requestId, ...) tells
// that it exists (GH#264). In the entries they may see, a reference attribute
// into such a CI keeps only its id. A schema change entry holds the writer's
// record, counts of stored data included, so it shows the summary and impact
// GET /schema-changes would show the caller (GH#261); its existence is no
// secret, so it stays listed. A field entry that lists stored values
// (`notMappedValues`, written by migration 0036) keeps its counts but shows the
// values only to a caller who may view the field's type and every subtype,
// whose CIs hold them (GH#392). [`redact`] applies the same CI rules again to
// the page, in case a CI changed class between the two queries.

/// The key of a field entry that lists values stored in CIs of the field's type.
const STORED_VALUES: &str = "notMappedValues";

/// The condition that keeps only entries the caller may see in full, given the
/// classes they may view: the rules of [`redact`], in SQL. A CI that no longer
/// exists counts as hidden. `x IN (uncorrelated subquery)` runs as one hashed
/// subplan per query, not as a lookup per audit row. Ids inside the values are
/// compared as text, in the form the API writes them (`uuid::text`), which
/// spares a cast per row; anything else matches nothing and counts as hidden.
fn push_visible(qb: &mut QueryBuilder<Postgres>, visible: &[Uuid]) {
    let texts: Vec<String> = visible.iter().map(Uuid::to_string).collect();
    qb.push(
        "(entity_type NOT IN ('configuration_items', 'ci_relationships') OR (entity_type = 'configuration_items' \
         AND entity_id IN (SELECT id FROM cmdb.configuration_items WHERE class_id = ANY(",
    )
    .push_bind(visible.to_vec())
    .push("))");
    // A CI can change class, so the class it had in either value must be viewable as well.
    for v in ["old_value", "new_value"] {
        qb.push(format!(" AND ({v} IS NULL OR {v}->'classId' IS NULL OR coalesce({v}->>'classId' = ANY("))
            .push_bind(texts.clone())
            .push("), false))");
    }
    // A business service's membership change: shown only with a member the
    // caller may view; the others are dropped from the row by `redact`.
    qb.push(
        " AND (new_value IS NULL OR jsonb_typeof(new_value->'members') IS DISTINCT FROM 'object' OR EXISTS (\
         SELECT 1 FROM jsonb_array_elements_text(coalesce(new_value->'members'->'added', '[]'::jsonb) \
         || coalesce(new_value->'members'->'removed', '[]'::jsonb)) AS m(id) \
         WHERE m.id IN (SELECT id::text FROM cmdb.configuration_items WHERE class_id = ANY(",
    )
    .push_bind(visible.to_vec())
    .push("))))");
    qb.push(") OR (entity_type = 'ci_relationships'");
    for v in ["old_value", "new_value"] {
        qb.push(format!(" AND ({v} IS NULL OR ("));
        for (i, end) in ["sourceCiId", "targetCiId"].into_iter().enumerate() {
            qb.push(if i == 0 { "" } else { " AND " })
                .push(format!("{v}->>'{end}' IN (SELECT id::text FROM cmdb.configuration_items WHERE class_id = ANY("))
                .push_bind(visible.to_vec())
                .push("))");
        }
        qb.push("))");
    }
    qb.push("))");
    // A CI's own layout: only with the CI (SHAA-1472).
    qb.push(
        " AND (entity_type <> 'ci_layout_overrides' \
         OR entity_id IN (SELECT id FROM cmdb.configuration_items WHERE class_id = ANY(",
    )
    .push_bind(visible.to_vec())
    .push(")))");
    // Import jobs and saved mappings name their class by key (T21).
    qb.push(
        " AND (entity_type NOT IN ('import_jobs', 'import_mappings') \
         OR coalesce(new_value ->> 'classKey', old_value ->> 'classKey') IN (SELECT key FROM cmdb.ci_classes WHERE id = ANY(",
    )
    .push_bind(visible.to_vec())
    .push(")))");
}

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

/// The CIs a business service's membership change names (`members.added`, `members.removed`).
fn member_ids(value: &Value) -> impl Iterator<Item = Uuid> + '_ {
    let members = value.get("members").filter(|m| m.is_object());
    ["added", "removed"]
        .into_iter()
        .filter_map(move |k| members.and_then(|m| m.get(k)).and_then(Value::as_array))
        .flatten()
        .filter_map(|id| id.as_str()?.parse().ok())
}

/// Drops the members the caller may not view from a membership change;
/// false when none is left (the entry then says nothing to the caller).
fn hide_members(value: &mut Value, can_view: &impl Fn(Uuid) -> bool) -> bool {
    let Some(members) = value.get_mut("members").and_then(Value::as_object_mut) else { return true };
    let mut any = false;
    for k in ["added", "removed"] {
        if let Some(ids) = members.get_mut(k).and_then(Value::as_array_mut) {
            ids.retain(|id| id.as_str().and_then(|s| s.parse().ok()).is_some_and(can_view));
            any |= !ids.is_empty();
        }
    }
    any
}

/// Every CI whose class decides what the caller may see of these entries.
fn referenced_cis(rows: &[AuditEntry]) -> Vec<Uuid> {
    let mut ids = HashSet::new();
    for e in rows {
        match e.entity_type.as_str() {
            "configuration_items" => {
                ids.insert(e.entity_id);
                ids.extend(values(e).flat_map(reference_ids));
                ids.extend(values(e).flat_map(member_ids));
            }
            "ci_relationships" => {
                ids.extend(values(e).flat_map(|v| [uuid_at(v, "sourceCiId"), uuid_at(v, "targetCiId")]).flatten());
            }
            "ci_layout_overrides" => {
                ids.insert(e.entity_id);
            }
            _ => {}
        }
        ids.extend(path_ids(e).into_iter().filter_map(|(_, p)| match p {
            PathId::Ci(id) => Some(id),
            PathId::Relationship(_) => None,
        }));
    }
    ids.into_iter().collect()
}

/// Every recorded schema change the entries are about.
fn referenced_changes(rows: &[AuditEntry]) -> Vec<Uuid> {
    let ids: HashSet<Uuid> = rows.iter().filter(|e| e.entity_type == "schema_changes").map(|e| e.entity_id).collect();
    ids.into_iter().collect()
}

/// Every field whose entry lists stored values ([`STORED_VALUES`]).
fn reported_fields(rows: &[AuditEntry]) -> Vec<Uuid> {
    let ids: HashSet<Uuid> = rows
        .iter()
        .filter(|e| e.entity_type == "ci_attribute_definitions" && values(e).any(|v| v.get(STORED_VALUES).is_some()))
        .map(|e| e.entity_id)
        .collect();
    ids.into_iter().collect()
}

/// Every relationship a `token.use` path names.
fn path_relationships(rows: &[AuditEntry]) -> Vec<Uuid> {
    let ids: HashSet<Uuid> = rows
        .iter()
        .flat_map(path_ids)
        .filter_map(|(_, p)| match p {
            PathId::Relationship(id) => Some(id),
            PathId::Ci(_) => None,
        })
        .collect();
    ids.into_iter().collect()
}

async fn relationship_endpoints(pool: &PgPool, ids: &[Uuid]) -> Result<HashMap<Uuid, (Uuid, Uuid)>, AppError> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let rows: Vec<(Uuid, Uuid, Uuid)> =
        sqlx::query_as("SELECT id, source_ci_id, target_ci_id FROM cmdb.ci_relationships WHERE id = ANY($1)")
            .bind(ids)
            .fetch_all(pool)
            .await?;
    Ok(rows.into_iter().map(|(id, source, target)| (id, (source, target))).collect())
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

/// The type of each field and every type below it, whose CIs hold the field's values.
async fn field_classes(pool: &PgPool, ids: &[Uuid]) -> Result<HashMap<Uuid, Vec<Uuid>>, AppError> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let rows: Vec<(Uuid, Vec<Uuid>)> = sqlx::query_as(
        "WITH RECURSIVE down (field, class_id, depth) AS (
           SELECT id, class_id, 0 FROM cmdb.ci_attribute_definitions WHERE id = ANY($1)
           UNION
           SELECT down.field, c.id, down.depth + 1 FROM cmdb.ci_classes c JOIN down ON c.parent_id = down.class_id
           WHERE down.depth < 64
         )
         SELECT field, array_agg(DISTINCT class_id) FROM down GROUP BY field",
    )
    .bind(ids)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().collect())
}

/// `classes`: the current class of each CI in [`referenced_cis`]; `changes`: each
/// change in [`referenced_changes`] as the caller sees it; `fields`: the types of each
/// field in [`reported_fields`] ([`field_classes`]); `visible`: the classes the caller may view.
fn redact(
    rows: &mut [AuditEntry],
    classes: &HashMap<Uuid, Uuid>,
    changes: &HashMap<Uuid, SchemaChange>,
    fields: &HashMap<Uuid, Vec<Uuid>>,
    visible: &HashSet<Uuid>,
) {
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
            "ci_layout_overrides" => can_view(e.entity_id),
            // Without its record there is no telling what the counts describe.
            "schema_changes" => match changes.get(&e.entity_id) {
                Some(change) => {
                    for v in e.old_value.iter_mut().chain(e.new_value.iter_mut()).filter_map(Value::as_object_mut) {
                        v.insert("summary".into(), Value::String(change.summary.clone()));
                        v.insert("impact".into(), crud::json(&change.impact));
                    }
                    continue;
                }
                None => false,
            },
            // The counts stay; a field that no longer exists reveals nothing.
            "ci_attribute_definitions" => {
                if !fields.get(&e.entity_id).is_some_and(|types| types.iter().all(|c| visible.contains(c))) {
                    for v in e.old_value.iter_mut().chain(e.new_value.iter_mut()).filter_map(Value::as_object_mut) {
                        v.remove(STORED_VALUES);
                    }
                }
                continue;
            }
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
            // Left out in SQL when no member is viewable; this covers a member
            // that changed class between the two queries.
            if let Some(v) = e.old_value.as_mut() {
                hide_members(v, &can_view);
            }
            let members_left = e.new_value.as_mut().is_none_or(|v| hide_members(v, &can_view));
            if !members_left {
                e.old_value = None;
                e.new_value = None;
                e.redacted = true;
            }
        }
    }
}

// A CI export (impact, business service members) and an `import.commit` hold
// counts taken with the writer's view: an export's rows span whatever classes
// the writer could see, and an import's counters include CIs of subclasses and
// relationships into other classes. Neither row says which classes, so a caller
// whose view is limited gets these counts as null unless they wrote the entry
// themselves (GH#440, as `Count::scoped` does for the item endpoints, GH#265).

/// The keys of an entry's `newValue` that count CI data as its writer saw it.
fn writer_counts(e: &AuditEntry) -> &'static [&'static str] {
    match (e.action, e.entity_type.as_str()) {
        (AuditAction::Export, "configuration_items") => &["rowCount", "truncated", "truncatedReason"],
        (AuditAction::ImportCommit, "import_jobs") => {
            &["created", "updated", "unchanged", "skipped", "failed", "relationshipsAdded"]
        }
        _ => &[],
    }
}

/// `reader`: the caller's user id, as `actorId` records it. Only for a caller whose view is limited.
fn withhold_writer_counts(rows: &mut [AuditEntry], reader: Option<&str>) {
    for e in rows {
        let keys = writer_counts(e);
        if keys.is_empty() || reader.is_some_and(|r| e.actor_id.as_deref() == Some(r)) {
            continue;
        }
        if let Some(v) = e.new_value.as_mut().and_then(Value::as_object_mut) {
            for &k in keys {
                if let Some(n) = v.get_mut(k) {
                    *n = Value::Null;
                }
            }
        }
    }
}

// A `token.use` entry records the raw request path, so a request for one CI or
// relationship carries its id: `/api/v1/configuration-items/{id}` (and the
// paths below it), `/api/v1/business-services/{id}` (a service is a CI, and so
// the paths below it) or `/api/v1/relationships/{id}`, the only routes whose
// path names one (GH#270, SHAA-927 §3.4). `/api/v1/business-services/{id}/members/{ciId}`
// names a second CI, the member (GH#377). Each id is judged like the entries above and
// replaced with `{hidden}` on its own when the caller may not view it; the rest of the
// entry stays. The path is parsed on read, so rows written before this check are covered.

/// Index of the id segment in a path split on `/` (`""`, `api`, `v1`, resource, id).
const PATH_ID_SEGMENT: usize = 4;
/// Index of the member id in `/api/v1/business-services/{id}/members/{ciId}`.
const PATH_MEMBER_SEGMENT: usize = 6;
const HIDDEN_SEGMENT: &str = "{hidden}";

#[derive(Debug, PartialEq)]
enum PathId {
    Ci(Uuid),
    Relationship(Uuid),
}

fn recorded_path(e: &AuditEntry) -> Option<&str> {
    if e.action != AuditAction::TokenUse || e.entity_type != "api_tokens" {
        return None;
    }
    e.new_value.as_ref()?.get("path")?.as_str()
}

/// A segment as the router saw it: percent-decoded.
fn decoded(segment: &str) -> Option<String> {
    percent_encoding::percent_decode_str(segment).decode_utf8().ok().map(|s| s.into_owned())
}

/// A segment the route parses as `{id}`.
fn uuid_segment(segment: &str) -> Option<Uuid> {
    let id = decoded(segment)?;
    validate::is_uuid(&id).then(|| Uuid::parse_str(&id).ok()).flatten()
}

/// Every CI or relationship a `token.use` path names, with the index of its segment,
/// parsed the way the routes parse `{id}` and `{ciId}`.
fn path_ids(e: &AuditEntry) -> Vec<(usize, PathId)> {
    let Some(path) = recorded_path(e) else { return Vec::new() };
    let segments: Vec<&str> = path.split('/').collect();
    let Some(prefix) =
        segments.get(..PATH_ID_SEGMENT).and_then(|p| p.iter().map(|s| decoded(s)).collect::<Option<Vec<_>>>())
    else {
        return Vec::new();
    };
    let Some(id) = segments.get(PATH_ID_SEGMENT).and_then(|s| uuid_segment(s)) else { return Vec::new() };
    match prefix.iter().map(String::as_str).collect::<Vec<_>>()[..] {
        ["", "api", "v1", "configuration-items"] => vec![(PATH_ID_SEGMENT, PathId::Ci(id))],
        ["", "api", "v1", "business-services"] => {
            let mut ids = vec![(PATH_ID_SEGMENT, PathId::Ci(id))];
            let member = segments
                .get(PATH_ID_SEGMENT + 1)
                .and_then(|s| decoded(s))
                .filter(|s| s == "members")
                .and_then(|_| segments.get(PATH_MEMBER_SEGMENT))
                .and_then(|s| uuid_segment(s));
            ids.extend(member.map(|m| (PATH_MEMBER_SEGMENT, PathId::Ci(m))));
            ids
        }
        ["", "api", "v1", "relationships"] => vec![(PATH_ID_SEGMENT, PathId::Relationship(id))],
        _ => Vec::new(),
    }
}

/// `edges`: the endpoints of each relationship in [`path_relationships`]; a relationship
/// is viewable when both are. An id with no row (a purged CI or relationship) is hidden.
fn hide_path_ids(
    rows: &mut [AuditEntry],
    classes: &HashMap<Uuid, Uuid>,
    edges: &HashMap<Uuid, (Uuid, Uuid)>,
    visible: &HashSet<Uuid>,
) {
    let can_view = |ci: Uuid| classes.get(&ci).is_some_and(|c| visible.contains(c));
    for e in rows {
        let hidden: Vec<usize> = path_ids(e)
            .into_iter()
            .filter(|(_, p)| match *p {
                PathId::Ci(id) => !can_view(id),
                PathId::Relationship(id) => !edges.get(&id).is_some_and(|&(s, t)| can_view(s) && can_view(t)),
            })
            .map(|(i, _)| i)
            .collect();
        if hidden.is_empty() {
            continue;
        }
        let Some(path) = recorded_path(e) else { continue };
        let mut segments: Vec<&str> = path.split('/').collect();
        for i in hidden {
            segments[i] = HIDDEN_SEGMENT;
        }
        let hidden = segments.join("/");
        if let Some(v) = e.new_value.as_mut().and_then(Value::as_object_mut) {
            v.insert("path".into(), Value::String(hidden));
        }
    }
}

/// Shared saved view entries name classes by key in `definition.classKeys`
/// (SHAA-578 §3.4): keys of classes the caller may not view, or that no longer
/// exist, are left out and counted in `hiddenClassKeyCount`, so the log names
/// no class the caller could not see through the saved-view API either.
fn hide_view_classes(rows: &mut [AuditEntry], visible_keys: &HashSet<String>) {
    for e in rows.iter_mut().filter(|e| e.entity_type == "saved_views") {
        for v in e.old_value.iter_mut().chain(e.new_value.iter_mut()) {
            let Some(keys) = v.get_mut("definition").and_then(|d| d.get_mut("classKeys")).and_then(Value::as_array_mut)
            else {
                continue;
            };
            let before = keys.len();
            keys.retain(|k| k.as_str().is_some_and(|k| visible_keys.contains(k)));
            let hidden = before - keys.len();
            if hidden > 0
                && let Some(o) = v.as_object_mut()
            {
                o.insert("hiddenClassKeyCount".into(), hidden.into());
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
                "Every change made through the API records the signed-in user as the actor (`actorType` user, `actorId` their id, `actorName` their username). Authentication events are recorded too, with `entityType` sessions: `login.success`, `login.failure`, `login.locked`, `logout` and `session.revoke`; `oldValue` is null and `newValue` holds the details (user, `ipAddress`, `userAgent`, reason). A failed sign-in has no actor id and records the attempted username as typed (first 64 characters), with nothing saying whether it exists. API tokens (`entityType` api_tokens) record `create` and `update` (revocation), and a `token.use` row for every request made with a known token, accepted or refused: `newValue` holds the token's name and prefix, owner, `outcome` (accepted, revoked, expired, owner_disabled, provider_disabled, mfa_required, no_scope, session_only, forbidden), method, path (first 512 characters, then `…`, with `pathLength`), `operationId`, `ipAddress` and `userAgent`; a token that can no longer authenticate (revoked, expired, owner_disabled, provider_disabled, mfa_required, no_scope) is recorded at most once a minute per outcome, the next row counting the uses left out in `unrecordedRefusals`; when no later request comes, a summary row (`actorType` system, no method or path) carries `unrecordedRefusals`, `windowStart` and `windowEnd` after the window ends or at graceful shutdown. Changes made with a token have `actorType` api_client and the owner as actor; the `token.use` row shares their `requestId`. Two-factor authentication events have `entityType` users and the user's id: `mfa.enrol`, `mfa.disable` (`reason` self_service or admin_reset), `mfa.failure` (a wrong or replayed code; `stage` login, disable or recovery_codes), `mfa.recovery_code_used` (with `recoveryCodesRemaining`) and `mfa.recovery_codes` (new codes replaced the old); sign-ins record `method` password, totp, recovery_code, setup, oidc or ldap in `login.success`. Identity providers (`entityType` identity_providers) record `create`, `update` and `delete` without their secrets; an account an identity provider creates or updates at sign-in is a `create` or `update` row on `users` with `actorType` system and `actorName` `identity provider \"<name>\"`, and a sign-in the provider vouched for but ShadouCMDB refused is a `login.failure`. A data model preview refused for a missing right (e.g. a field type change by a user who may not view every type storing the field) is a `schema_change.refused` row on the area, type or field previewed, `newValue` holding the operation, the body sent, `code`, `field` and `message`. A data export is an `export` row on what was exported (`entityType` configuration_items), `newValue` holding `kind`, `format`, `rowCount` and `visibility`, never the rows: the impact analysis CSV (`kind` impact, on the analysed CI, with the `parameters`, `truncated` and `truncatedReason`), a business service's member CSV (`kind` business_service_members, on the service), and the configuration file (`kind` config, `entityType` config with the nil id, `newValue` holding `format`, `formatVersion`, `sections`, `profilesIncluded` and `mappingCount`, never the content). Adding or removing business service members records one relationship `create` or `delete` per member plus one `update` on the service whose `oldValue` and `newValue` are both `{\"members\": {\"added\": [ids], \"removed\": [ids]}}`; replacing its owners records one `update` on the service whose `oldValue` and `newValue` are `{\"owners\": {\"technical\": [...], \"business\": [...]}}`, each owner as `kind`, `id` and `name`. An operator's `shadoucmdb prune-audit` leaves an `audit.purge` row (`entityType` audit_log, `actorType` system, `actorName` the database user) whose `newValue` holds the scope, window, cutoff and the number of rows deleted per action; those rows are never pruned. A caller whose profile limits the classes they may view does not get entries about a CI of another class, or of a CI that no longer exists, nor relationship entries with an endpoint in one, judged by the CIs' current classes and, for a CI, every class it had in either value: those entries are left out of the page and of `page.total` whatever the filters, so neither tells that they exist. For the same reason such a caller does not get the stored sequence number as `id`, which would show a gap where an entry was left out, but a keyed permutation of it: stable and distinct per entry, unrelated in order or distance to any other (a rotation of the encryption key changes it). In the CI entries they may see, a reference attribute into a CI they may not view keeps only its id (`attributeReferences` shows it hidden, as the item endpoints do), and a business service's membership change lists only the members they may view: one naming none of those is left out like the entries above. Counts an entry took with its writer's view are null to them unless they wrote it: `rowCount`, `truncated` and `truncatedReason` of a CI export (impact, business service members), and `created`, `updated`, `unchanged`, `skipped`, `failed` and `relationshipsAdded` of an `import.commit`, which may span classes they may not view. In `token.use` rows, the id in a `path` that names a CI (`/configuration-items/{id}`, `/configuration-items/{id}/graph`, `/configuration-items/{id}/impact`, `/configuration-items/{id}/impact/export`, `/configuration-items/{id}/business-services`, `/business-services/{id}` and every path below it, whose member in `/business-services/{id}/members/{ciId}` is judged on its own) or relationship (`/relationships/{id}`) they may not view (a relationship: both endpoints) is replaced with `{hidden}`, e.g. `/api/v1/configuration-items/{hidden}/graph`; the rest of the row stays. Schema change entries (`entityType` schema_changes) show `summary` and `impact` as getSchemaChange shows them to the caller: counts of stored data only with the view right on every type they describe. A field entry that lists values stored in CIs (`notMappedValues`, from the upgrade that archived the Application field criticality) keeps its counts but lists the values only to a caller who may view the field's type and every type below it. Shared saved views (`entityType` saved_views) record `create`, `update` and `delete` with the view's `name`, `description`, `context`, `visibility` and `definition` (a shared copy's `create` names its source view in `copiedFrom`; views a configuration import writes have `actorType` import); personal views and default views are not recorded. In those entries a caller whose profile limits the classes they may view does not get the keys of other classes in `definition.classKeys`: they are left out and counted in `hiddenClassKeyCount`. Passwords (local or directory), session tokens, CSRF tokens, API token secrets, TOTP secrets, authenticator or recovery codes, OIDC client secrets, authorization codes and ID tokens, and LDAP bind passwords are never recorded.",
            )
            .requires(GlobalPermission::AuditView)
            .handle(|api, In(NoPath, Query(q), NoBody): In<NoPath, Query<AuditQuery>, NoBody>| async move {
                Ok(Json(list(&api.pool, &api.ctx, &api.auth.keyring, &q).await?))
            }),
    ]
}

#[cfg(test)]
pub(crate) mod tests {
    use serde_json::json;

    use super::*;

    // Version 4 layout, so the ids also pass the route's uuid check.
    fn id(n: u128) -> Uuid {
        Uuid::from_u128(0x0000_0000_0000_4000_8000_0000_0000_0000 | n)
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
        redact(&mut rows, &classes, &HashMap::new(), &HashMap::new(), &HashSet::from([id(10)]));
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
    fn a_membership_change_lists_only_viewable_members() {
        let members = |added: &[Uuid], removed: &[Uuid]| json!({ "members": { "added": added, "removed": removed } });
        let rows = run(vec![
            entry("configuration_items", id(3), None, Some(members(&[id(1), id(2)], &[]))),
            entry("configuration_items", id(3), None, Some(members(&[], &[id(2), id(9)]))),
            entry("configuration_items", id(3), None, Some(members(&[id(2)], &[id(1)]))),
        ]);
        assert_eq!(rows[0].new_value, Some(members(&[id(1)], &[])));
        assert!(!rows[0].redacted);
        // Nothing viewable left (SQL leaves such a row out; this is the backstop).
        assert!(rows[1].redacted && rows[1].new_value.is_none());
        assert_eq!(rows[2].new_value, Some(members(&[], &[id(1)])));
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

    fn token_use(path: &str) -> AuditEntry {
        AuditEntry {
            action: AuditAction::TokenUse,
            ..entry("api_tokens", id(70), None, Some(json!({ "method": "GET", "path": path, "outcome": "accepted" })))
        }
    }

    #[test]
    fn token_use_paths_name_cis_and_relationships_only_where_the_route_does() {
        let ci = id(2).to_string();
        let parsed = |path: &str| match &path_ids(&token_use(path))[..] {
            [] => None,
            [(PATH_ID_SEGMENT, _)] => path_ids(&token_use(path)).pop().map(|(_, p)| p),
            more => panic!("{path}: {more:?}"),
        };
        assert_eq!(parsed(&format!("/api/v1/configuration-items/{ci}")), Some(PathId::Ci(id(2))));
        assert_eq!(parsed(&format!("/api/v1/configuration-items/{ci}/graph")), Some(PathId::Ci(id(2))));
        assert_eq!(parsed(&format!("/api/v1/configuration-items/{ci}/impact")), Some(PathId::Ci(id(2))));
        assert_eq!(parsed(&format!("/api/v1/configuration-items/{ci}/impact/export")), Some(PathId::Ci(id(2))));
        assert_eq!(parsed(&format!("/api/v1/relationships/{ci}")), Some(PathId::Relationship(id(2))));
        // A business service is a CI (SHAA-927 §3.4).
        for below in ["", "/members", "/members/export", "/members/remove", "/members/xxx", "/owners"] {
            assert_eq!(parsed(&format!("/api/v1/business-services/{ci}{below}")), Some(PathId::Ci(id(2))), "{below}");
        }
        assert_eq!(parsed(&format!("/api/v1/configuration-items/{ci}/business-services")), Some(PathId::Ci(id(2))));
        // removeBusinessServiceMember names the member as well, decoded like the service (GH#377).
        let member = id(3).to_string();
        let encoded_member = format!("%{:02x}{}", member.as_bytes()[0], &member[1..]);
        for path in [
            format!("/api/v1/business-services/{ci}/members/{member}"),
            format!("/api/v1/business-services/{ci}/%6Dembers/{encoded_member}"),
        ] {
            assert_eq!(
                path_ids(&token_use(&path)),
                [(PATH_ID_SEGMENT, PathId::Ci(id(2))), (PATH_MEMBER_SEGMENT, PathId::Ci(id(3)))],
                "{path}"
            );
        }
        // Only below a service: another resource's sixth segment is not a CI.
        assert_eq!(parsed(&format!("/api/v1/configuration-items/{ci}/members/{member}")), Some(PathId::Ci(id(2))));
        // The router decodes the segment before parsing it, so the check does too.
        let encoded = format!("/api/v1/configuration%2Ditems/%{:02x}{}", ci.as_bytes()[0], &ci[1..]);
        assert_eq!(parsed(&encoded), Some(PathId::Ci(id(2))));
        for other in [
            format!("/api/v1/admin/users/{ci}"),
            format!("/api/v1/ci-classes/{ci}/attributes"),
            "/api/v1/configuration-items/xxx".into(),
            "/api/v1/configuration-items".into(),
        ] {
            assert_eq!(parsed(&other), None, "{other}");
        }
        let mut create = token_use(&format!("/api/v1/configuration-items/{ci}"));
        create.action = AuditAction::Create;
        assert!(path_ids(&create).is_empty());
    }

    #[test]
    fn token_use_paths_hide_ids_the_caller_may_not_view() {
        let classes = HashMap::from([(id(1), id(10)), (id(2), id(20)), (id(3), id(10))]);
        let edges = HashMap::from([(id(50), (id(1), id(3))), (id(51), (id(1), id(2)))]);
        let mut rows = vec![
            token_use(&format!("/api/v1/configuration-items/{}/graph", id(1))),
            token_use(&format!("/api/v1/configuration-items/{}/graph", id(2))),
            token_use(&format!("/api/v1/configuration-items/{}", id(9))),
            token_use(&format!("/api/v1/relationships/{}", id(50))),
            token_use(&format!("/api/v1/relationships/{}", id(51))),
            token_use(&format!("/api/v1/relationships/{}", id(52))),
            token_use(&format!("/api/v1/business-services/{}/members/{}", id(2), id(1))),
            token_use(&format!("/api/v1/business-services/{}/owners", id(3))),
            // Each CI in a member path is judged on its own (GH#377).
            token_use(&format!("/api/v1/business-services/{}/members/{}", id(1), id(2))),
            token_use(&format!("/api/v1/business-services/{}/members/{}", id(2), id(9))),
            token_use(&format!("/api/v1/business-services/{}/members/{}", id(1), id(3))),
        ];
        hide_path_ids(&mut rows, &classes, &edges, &HashSet::from([id(10)]));
        let paths: Vec<&str> = rows.iter().map(|r| r.new_value.as_ref().unwrap()["path"].as_str().unwrap()).collect();
        assert_eq!(
            paths,
            [
                format!("/api/v1/configuration-items/{}/graph", id(1)).as_str(),
                "/api/v1/configuration-items/{hidden}/graph",
                "/api/v1/configuration-items/{hidden}",
                format!("/api/v1/relationships/{}", id(50)).as_str(),
                "/api/v1/relationships/{hidden}",
                "/api/v1/relationships/{hidden}",
                format!("/api/v1/business-services/{{hidden}}/members/{}", id(1)).as_str(),
                format!("/api/v1/business-services/{}/owners", id(3)).as_str(),
                format!("/api/v1/business-services/{}/members/{{hidden}}", id(1)).as_str(),
                "/api/v1/business-services/{hidden}/members/{hidden}",
                format!("/api/v1/business-services/{}/members/{}", id(1), id(3)).as_str(),
            ]
        );
        assert!(rows.iter().all(|r| !r.redacted && r.new_value.as_ref().unwrap()["outcome"] == "accepted"));
    }

    #[test]
    fn a_schema_change_shows_the_record_as_the_caller_sees_it_or_nothing() {
        let change = |summary: &str| SchemaChange {
            id: id(60),
            occurred_at: Utc::now(),
            actor_type: "user".into(),
            actor_id: None,
            actor_name: None,
            request_id: None,
            summary: summary.into(),
            statements: vec!["ALTER TABLE t DROP COLUMN code".into()],
            impact: sqlx::types::Json(vec![]),
        };
        let written = crud::json(&SchemaChange {
            impact: sqlx::types::Json(vec![crate::schema::Impact {
                statement: Some(0),
                kind: "drop_column".into(),
                rows: Some(12),
                message: "12 stored values of code are deleted".into(),
            }]),
            ..change("Purge field code (12 values)")
        });
        let mut rows = vec![
            entry("schema_changes", id(60), None, Some(written.clone())),
            entry("schema_changes", id(61), None, Some(written)),
        ];
        let changes = HashMap::from([(id(60), change("Purge field code"))]);
        redact(&mut rows, &HashMap::new(), &changes, &HashMap::new(), &HashSet::new());
        let v = rows[0].new_value.as_ref().unwrap();
        assert!(!rows[0].redacted);
        assert_eq!((&v["summary"], &v["impact"]), (&json!("Purge field code"), &json!([])));
        assert_eq!(v["statements"][0], "ALTER TABLE t DROP COLUMN code");
        // No record to judge the counts by: withheld.
        assert!(rows[1].redacted);
        assert_eq!(rows[1].new_value, None);
    }

    #[test]
    fn a_field_entry_lists_stored_values_only_to_who_may_view_every_type_holding_them() {
        let report = || json!({ "isActive": false, "notMapped": 2, "notMappedValues": ["urgent", "ask Bob"] });
        let fields = HashMap::from([(id(80), vec![id(10)]), (id(81), vec![id(10), id(20)])]);
        let mut rows = vec![
            entry("ci_attribute_definitions", id(80), None, Some(report())),
            entry("ci_attribute_definitions", id(81), None, Some(report())),
            // No longer there: no telling which type held the values.
            entry("ci_attribute_definitions", id(82), None, Some(report())),
        ];
        redact(&mut rows, &HashMap::new(), &HashMap::new(), &fields, &HashSet::from([id(10)]));
        assert_eq!(rows[0].new_value, Some(report()));
        for r in &rows[1..] {
            assert!(!r.redacted);
            assert_eq!(r.new_value, Some(json!({ "isActive": false, "notMapped": 2 })));
        }
        assert_eq!(reported_fields(&rows[..1]), [id(80)]);
        assert!(reported_fields(&rows[1..]).is_empty());
    }

    #[test]
    fn other_entity_types_are_left_alone() {
        let rows = run(vec![entry("ci_classes", id(20), None, Some(json!({ "id": id(20), "name": "Server" })))]);
        assert!(!rows[0].redacted);
        assert!(rows[0].new_value.is_some());
    }

    /// Every entry of one entity type, up to 50.
    pub(crate) fn of_type(t: EntityType) -> AuditQuery {
        query(|q| (q.entity_type, q.limit) = (Some(t), 50))
    }

    fn query(f: impl FnOnce(&mut AuditQuery)) -> AuditQuery {
        let mut q = AuditQuery {
            limit: 1,
            offset: 0,
            sort: Sort { field: "occurredAt".into(), desc: true },
            entity_type: None,
            entity_id: None,
            action: None,
            actor_id: None,
            actor_name: None,
            request_id: None,
            from: None,
            to: None,
        };
        f(&mut q);
        q
    }

    pub(crate) fn viewer(classes: &[Uuid]) -> RequestContext {
        use crate::auth::permissions::{ClassRights, Permissions};
        let view = ClassRights { view: true, ..Default::default() };
        let permissions = Permissions {
            global: [GlobalPermission::AuditView, GlobalPermission::DatamodelManage].into(),
            classes: classes.iter().map(|c| (*c, view)).collect(),
            ..Default::default()
        };
        let principal = crate::auth::Principal {
            user_id: Uuid::new_v4(),
            username: "auditor".into(),
            credential: crate::auth::Credential::Token,
            permissions,
        };
        RequestContext::user(std::sync::Arc::new(principal), "test".into())
    }

    /// GH#264: entries about CIs the caller may not view are left out in SQL, so
    /// neither the rows nor `total` under any filter reveal them.
    #[tokio::test]
    async fn entries_about_hidden_cis_are_neither_listed_nor_counted() {
        let Some(db) = crate::db::scratch::database("audit_hidden_cis_are_not_counted").await else { return };
        let pool = &db.pool;
        crate::seed::install_template(pool, "it_infrastructure").await.unwrap();
        crate::seed::seed_demo_data(pool).await.unwrap();
        let ci = |label: &'static str| async move {
            sqlx::query_as::<_, (Uuid, Uuid)>("SELECT id, class_id FROM configuration_items WHERE label = $1")
                .bind(label)
                .fetch_one(pool)
                .await
                .unwrap()
        };
        let (srv, server) = ci("fra1-esx-01").await;
        let (sw, network_device) = ci("fra1-tor-a01").await;
        let (vm, vm_class) = ci("crm-app-01").await;
        let insert = |entity_type: &'static str, entity: Uuid, old: Option<Value>, new: Option<Value>| async move {
            sqlx::query(
                "INSERT INTO audit_log (actor_type, actor_name, action, entity_type, entity_id, old_value, new_value, request_id)
                 VALUES ('user', 'admin', 'delete', $1, $2, $3, $4, 'purge-1')",
            )
            .bind(entity_type)
            .bind(entity)
            .bind(old)
            .bind(new)
            .execute(pool)
            .await
            .unwrap();
        };
        // A purge of a type the caller may not view: two CIs that no longer exist, one
        // edge from one of them, and the type's own (unrestricted) entry.
        let (gone1, gone2, gone_class) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        insert("configuration_items", gone1, Some(json!({ "classId": gone_class })), None).await;
        insert("configuration_items", gone2, Some(json!({ "classId": gone_class })), None).await;
        insert("ci_relationships", Uuid::new_v4(), Some(edge(gone1, srv)), None).await;
        insert("ci_classes", gone_class, Some(json!({ "id": gone_class, "name": "Secrets" })), None).await;
        // Edges: both endpoints visible; one hidden; a malformed endpoint.
        let (both, half, bad) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        insert("ci_relationships", both, Some(edge(srv, sw)), None).await;
        insert("ci_relationships", half, Some(edge(vm, srv)), None).await;
        insert("ci_relationships", bad, Some(json!({ "sourceCiId": srv, "targetCiId": "nope" })), None).await;
        // A visible CI that was in a hidden class before.
        insert(
            "configuration_items",
            sw,
            Some(json!({ "classId": vm_class })),
            Some(json!({ "classId": network_device })),
        )
        .await;

        let restricted = viewer(&[server, network_device]);
        let admin = RequestContext::system("test", "test");
        let total = |ctx: RequestContext, q: AuditQuery| async move {
            list(pool, &ctx, &Keyring::for_tests(), &q).await.unwrap()
        };
        let purge = |t: EntityType| query(move |q| (q.request_id, q.entity_type) = (Some("purge-1".into()), Some(t)));

        // The purge's counts: the full ones for a caller who may view every class, none otherwise.
        assert_eq!(total(admin.clone(), purge(EntityType::ConfigurationItems)).await.page.total, 3);
        assert_eq!(total(restricted.clone(), purge(EntityType::ConfigurationItems)).await.page.total, 0);
        assert_eq!(total(admin.clone(), purge(EntityType::CiRelationships)).await.page.total, 4);
        let edges = total(restricted.clone(), query(|q| (q.request_id, q.limit) = (Some("purge-1".into()), 50))).await;
        let ids: HashSet<Uuid> = edges.data.iter().map(|e| e.entity_id).collect();
        assert_eq!(ids, HashSet::from([both, gone_class]), "{:?}", edges.data);
        assert_eq!(edges.page.total, 2);
        assert!(edges.data.iter().all(|e| !e.redacted && e.old_value.is_some()));

        // A hidden CI has no history for the caller; a visible one keeps it.
        let history = |ci: Uuid| query(move |q| (q.entity_id, q.limit) = (Some(UuidList(vec![ci])), 50));
        let hidden = total(restricted.clone(), history(vm)).await;
        assert_eq!((hidden.page.total, hidden.data.len()), (0, 0));
        assert_eq!(total(admin.clone(), history(vm)).await.page.total, 1);
        let shown = total(restricted.clone(), history(srv)).await;
        assert_eq!(shown.page.total, 1);
        assert!(!shown.data[0].redacted && shown.data[0].new_value.is_some());

        // The total is exactly what the caller can page through.
        let everything = total(
            restricted.clone(),
            query(|q| (q.entity_type, q.limit) = (Some(EntityType::ConfigurationItems), 200)),
        )
        .await;
        assert_eq!(everything.page.total, everything.data.len() as i64);
        assert!(everything.data.iter().all(|e| !e.redacted), "nothing left to withhold");
        let cis: HashSet<Uuid> = everything.data.iter().map(|e| e.entity_id).collect();
        let viewable: Vec<Uuid> = sqlx::query_scalar("SELECT id FROM configuration_items WHERE class_id = ANY($1)")
            .bind(vec![server, network_device])
            .fetch_all(pool)
            .await
            .unwrap();
        assert_eq!(cis, viewable.into_iter().collect());
        assert!(cis.contains(&srv) && !cis.contains(&vm));
        assert!(!everything.data.iter().any(|e| e.entity_id == sw && e.old_value.is_some()), "was in a hidden class");

        // A caller with no class at all sees none of it.
        assert_eq!(total(viewer(&[]), purge(EntityType::CiRelationships)).await.page.total, 0);
        db.drop().await;
    }

    /// GH#378: the rows of one request get consecutive ids, so the raw ids of
    /// the rows a restricted reader sees would show, by a gap, that a hidden row
    /// was written in between. They get a keyed permutation instead, which tells
    /// a request with a hidden row from one without no more than the rows do.
    #[tokio::test]
    async fn restricted_readers_cannot_find_hidden_rows_in_the_ids() {
        let Some(db) = crate::db::scratch::database("audit_row_ids_hide_gaps").await else { return };
        let pool = &db.pool;
        crate::seed::install_template(pool, "it_infrastructure").await.unwrap();
        crate::seed::seed_demo_data(pool).await.unwrap();
        let ci = |label: &'static str| async move {
            sqlx::query_as::<_, (Uuid, Uuid)>("SELECT id, class_id FROM configuration_items WHERE label = $1")
                .bind(label)
                .fetch_one(pool)
                .await
                .unwrap()
        };
        let (srv, server) = ci("fra1-esx-01").await;
        let (vm, vm_class) = ci("crm-app-01").await;
        let insert = |request: &'static str, entity: Uuid, class: Uuid| async move {
            sqlx::query(
                "INSERT INTO audit_log (actor_type, actor_name, action, entity_type, entity_id, old_value, new_value, request_id)
                 VALUES ('user', 'admin', 'update', 'configuration_items', $1, $2, $2, $3)",
            )
            .bind(entity)
            .bind(json!({ "classId": class }))
            .bind(request)
            .execute(pool)
            .await
            .unwrap();
        };
        // The same visible rows, with a hidden one written between them and without.
        insert("hidden-world", srv, server).await;
        insert("hidden-world", vm, vm_class).await;
        insert("hidden-world", srv, server).await;
        insert("control-world", srv, server).await;
        insert("control-world", srv, server).await;

        let keyring = Keyring::for_tests();
        let ids = |ctx: RequestContext, request: &'static str| {
            let keyring = keyring.clone();
            async move {
                let q = query(|q| {
                    (q.request_id, q.limit, q.sort) =
                        (Some(request.into()), 50, Sort { field: "occurredAt".into(), desc: false })
                });
                list(pool, &ctx, &keyring, &q).await.unwrap().data.into_iter().map(|e| e.id).collect::<Vec<i64>>()
            }
        };
        let admin = RequestContext::system("test", "test");
        let raw_hidden = ids(admin.clone(), "hidden-world").await;
        let raw_control = ids(admin, "control-world").await;
        // An unrestricted reader keeps the stored sequence numbers, gap and all.
        assert_eq!(raw_hidden.len(), 3);
        assert_eq!((raw_hidden[2] - raw_hidden[0], raw_control[1] - raw_control[0]), (2, 1));

        let reader = viewer(&[server]);
        let seen_hidden = ids(reader.clone(), "hidden-world").await;
        let seen_control = ids(reader.clone(), "control-world").await;
        assert_eq!(seen_hidden, [raw_hidden[0], raw_hidden[2]].map(|id| keyring.audit_row_id(id)));
        assert_eq!(seen_control, raw_control.iter().map(|&id| keyring.audit_row_id(id)).collect::<Vec<_>>());
        for seen in [&seen_hidden, &seen_control] {
            assert!(seen.iter().all(|id| !raw_hidden.contains(id) && !raw_control.contains(id)), "{seen:?}");
            assert!((seen[1] - seen[0]).abs() > 2, "{seen:?}");
        }
        // Stable from one read to the next, so the UI can key rows by it.
        assert_eq!(ids(reader, "hidden-world").await, seen_hidden);
        db.drop().await;
    }

    /// T21 (SHAA-799): import job and saved-mapping entries name their class by
    /// key; a reader who may not view that class neither sees nor counts them.
    #[tokio::test]
    async fn import_entries_of_hidden_classes_are_neither_listed_nor_counted() {
        let Some(db) = crate::db::scratch::database("import_entries_of_hidden_classes").await else { return };
        let pool = &db.pool;
        crate::seed::install_template(pool, "it_infrastructure").await.unwrap();
        let class = |key: &'static str| async move {
            sqlx::query_scalar::<_, Uuid>("SELECT id FROM ci_classes WHERE key = $1")
                .bind(key)
                .fetch_one(pool)
                .await
                .unwrap()
        };
        let server = class("server").await;
        let insert = |action: &'static str, entity_type: &'static str, old: Option<Value>, new: Value| async move {
            sqlx::query(
                "INSERT INTO audit_log (actor_type, actor_name, action, entity_type, entity_id, old_value, new_value, request_id)
                 VALUES ('import', 'alice', $1, $2, gen_random_uuid(), $3, $4, 'import-t21')",
            )
            .bind(action)
            .bind(entity_type)
            .bind(old)
            .bind(new)
            .execute(pool)
            .await
            .unwrap();
        };
        insert("import.commit", "import_jobs", None, json!({ "classKey": "server" })).await;
        insert("import.commit", "import_jobs", None, json!({ "classKey": "virtual_machine" })).await;
        insert("import.report_read", "import_jobs", None, json!({ "classKey": "virtual_machine" })).await;
        insert("create", "import_mappings", None, json!({ "classKey": "server", "name": "Vendor" })).await;
        insert("delete", "import_mappings", Some(json!({ "classKey": "virtual_machine" })), json!({})).await;
        insert("update", "import_settings", Some(json!({ "enabled": false })), json!({ "enabled": true })).await;

        let q = query(|q| (q.request_id, q.limit) = (Some("import-t21".into()), 50));
        let admin = list(pool, &RequestContext::system("test", "test"), &Keyring::for_tests(), &q).await.unwrap();
        assert_eq!(admin.page.total, 6);
        let restricted = list(pool, &viewer(&[server]), &Keyring::for_tests(), &q).await.unwrap();
        assert_eq!(restricted.page.total, 3, "{:?}", restricted.data);
        assert_eq!(restricted.data.len(), 3);
        for e in &restricted.data {
            let key = e.new_value.iter().chain(e.old_value.iter()).find_map(|v| v.get("classKey"));
            assert!(key.is_none() || key == Some(&json!("server")), "{e:?}");
        }
        db.drop().await;
    }

    /// GH#440: export and `import.commit` counts were taken with the writer's
    /// view; a restricted reader gets them as null unless they wrote the entry.
    #[tokio::test]
    async fn writer_counts_are_withheld_from_restricted_readers_who_did_not_write_them() {
        let Some(db) = crate::db::scratch::database("audit_writer_counts_withheld").await else { return };
        let pool = &db.pool;
        crate::seed::install_template(pool, "it_infrastructure").await.unwrap();
        crate::seed::seed_demo_data(pool).await.unwrap();
        let (srv, server): (Uuid, Uuid) =
            sqlx::query_as("SELECT id, class_id FROM configuration_items WHERE label = 'fra1-esx-01'")
                .fetch_one(pool)
                .await
                .unwrap();
        let reader = viewer(&[server]);
        let reader_id = reader.principal().unwrap().user_id.to_string();
        let admin_id = Uuid::new_v4().to_string();
        let insert = |actor: String, action: &'static str, entity_type: &'static str, entity: Uuid, new: Value| async move {
            sqlx::query(
                "INSERT INTO audit_log (actor_type, actor_id, actor_name, action, entity_type, entity_id, new_value, request_id)
                 VALUES ('user', $1, 'someone', $2, $3, $4, $5, 'gh440')",
            )
            .bind(actor)
            .bind(action)
            .bind(entity_type)
            .bind(entity)
            .bind(new)
            .execute(pool)
            .await
            .unwrap();
        };
        let members =
            json!({ "kind": "business_service_members", "format": "csv", "rowCount": 12, "visibility": "all_classes" });
        let impact = json!({
            "kind": "impact", "format": "csv", "parameters": {}, "rowCount": 40,
            "truncated": true, "truncatedReason": "node_budget", "visibility": "all_classes",
        });
        let commit = json!({
            "classKey": "server", "rows": 10, "created": 3, "updated": 2, "unchanged": 1,
            "skipped": 1, "failed": 3, "relationshipsAdded": 5, "outcome": "succeeded",
        });
        for actor in [admin_id.clone(), reader_id.clone()] {
            insert(actor.clone(), "export", "configuration_items", srv, members.clone()).await;
            insert(actor.clone(), "export", "configuration_items", srv, impact.clone()).await;
            insert(actor, "import.commit", "import_jobs", Uuid::new_v4(), commit.clone()).await;
        }
        let q = query(|q| (q.request_id, q.limit) = (Some("gh440".into()), 50));
        let read = |ctx: RequestContext| {
            let q = &q;
            async move { list(pool, &ctx, &Keyring::for_tests(), q).await.unwrap().data }
        };
        let counts = [
            "rowCount",
            "truncated",
            "truncatedReason",
            "created",
            "updated",
            "unchanged",
            "skipped",
            "failed",
            "relationshipsAdded",
        ];
        let restricted = read(reader).await;
        assert_eq!(restricted.len(), 6);
        for e in &restricted {
            let v = e.new_value.as_ref().unwrap();
            let own = e.actor_id.as_deref() == Some(reader_id.as_str());
            for k in counts.iter().filter(|k| v.get(**k).is_some()) {
                assert_eq!(v[*k].is_null(), !own, "{k} in {e:?}");
            }
            assert!(v.get("kind").is_some() || v["rows"] == 10, "the rest of the entry stays: {e:?}");
        }
        // An unrestricted reader sees every count as written.
        for e in read(RequestContext::system("test", "test")).await {
            let v = e.new_value.unwrap();
            assert!(counts.iter().all(|k| v.get(*k).is_none_or(|n| !n.is_null())), "{v}");
        }
        db.drop().await;
    }

    /// GH#270: a reader with audit.view but no view on a class must not learn,
    /// from `token.use` paths, the ids of its CIs or of relationships into it.
    #[tokio::test]
    async fn token_use_paths_hide_cis_the_reader_may_not_view() {
        use axum::http::header;

        use crate::db::scratch;
        use crate::modules::api_tokens::tests::{Creds, app, call};

        let Some(db) = scratch::database("token_use_paths_hide_cis_the_reader_may_not_view").await else { return };
        let app = app(db.pool.clone());
        let pool = &db.pool;

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

        let mut class_ids = Vec::new();
        for (key, name) in [("public", "Public"), ("secrets", "Secrets")] {
            let (status, v, _) =
                call(&app, "POST", "/api/v1/ci-classes", &session, Some(json!({ "key": key, "name": name }))).await;
            assert_eq!(status, 201, "{v}");
            class_ids.push(v["id"].as_str().unwrap().parse::<Uuid>().unwrap());
        }
        let (public, secrets) = (class_ids[0], class_ids[1]);
        let mut cis = Vec::new();
        for class in [public, secrets] {
            let (status, v, _) =
                call(&app, "POST", "/api/v1/configuration-items", &session, Some(json!({ "classId": class }))).await;
            assert_eq!(status, 201, "{v}");
            cis.push(v["id"].as_str().unwrap().to_owned());
        }
        let (shown, hidden) = (cis[0].clone(), cis[1].clone());
        let rel_type: Uuid = sqlx::query_scalar(
            "INSERT INTO relationship_types (key, name, forward_label, reverse_label)
             VALUES ('uses', 'Uses', 'uses', 'used by') RETURNING id",
        )
        .fetch_one(pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO relationship_type_rules (relationship_type_id, source_class_id, target_class_id) VALUES ($1, $2, $3)",
        )
        .bind(rel_type)
        .bind(public)
        .bind(secrets)
        .execute(pool)
        .await
        .unwrap();
        let edge = json!({ "relationshipTypeId": rel_type, "sourceCiId": shown, "targetCiId": hidden });
        let (status, v, _) = call(&app, "POST", "/api/v1/relationships", &session, Some(edge)).await;
        assert_eq!(status, 201, "{v}");
        let edge = v["id"].as_str().unwrap().to_owned();

        // Two scopes for the owner's tokens: T views every class, the auditor's only Public.
        let profile = |name: &'static str, class: Option<Uuid>| async move {
            let p: Uuid = sqlx::query_scalar("INSERT INTO permission_profiles (name) VALUES ($1) RETURNING id")
                .bind(name)
                .fetch_one(pool)
                .await
                .unwrap();
            sqlx::query(
                "INSERT INTO permission_profile_global_permissions (profile_id, permission) VALUES ($1, 'audit.view')",
            )
            .bind(p)
            .execute(pool)
            .await
            .unwrap();
            sqlx::query(
                "INSERT INTO permission_profile_class_permissions (profile_id, class_id, can_view) VALUES ($1, $2, true)",
            )
            .bind(p)
            .bind(class)
            .execute(pool)
            .await
            .unwrap();
            p
        };
        let expires = (chrono::Utc::now() + chrono::Duration::days(30)).to_rfc3339();
        let mut tokens = Vec::new();
        for (name, class) in [("Everything", None), ("Auditors", Some(public))] {
            let body = json!({ "name": name, "profileId": profile(name, class).await, "expiresAt": expires });
            let (status, v, _) = call(&app, "POST", "/api/v1/admin/api-tokens", &session, Some(body)).await;
            assert_eq!(status, 201, "{v}");
            tokens.push(Creds { bearer: v["secret"].as_str().map(str::to_owned), ..Creds::default() });
        }
        let (full, auditor) = (&tokens[0], &tokens[1]);

        // Percent-encoding the id reaches the same CI, so it must not slip past the check.
        let encoded = format!("%{:02X}{}", hidden.as_bytes()[0], &hidden[1..]);
        let requests = [
            format!("/api/v1/configuration-items/{shown}"),
            format!("/api/v1/configuration-items/{hidden}"),
            format!("/api/v1/configuration-items/{hidden}/graph"),
            format!("/api/v1/configuration-items/{hidden}/impact"),
            format!("/api/v1/configuration-items/{hidden}/impact/export"),
            format!("/api/v1/configuration-items/{shown}/impact"),
            format!("/api/v1/configuration-items/{encoded}"),
            format!("/api/v1/relationships/{edge}"),
        ];
        for path in &requests {
            let (status, v, _) = call(&app, "GET", path, full, None).await;
            assert_eq!(status, 200, "{path}: {v}");
        }

        let log = "/api/v1/audit-log?entityType=api_tokens&action=token.use&sort=occurredAt&limit=200";
        let paths = |v: &Value| -> Vec<String> {
            v["data"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|e| e["newValue"]["tokenName"] == "Everything")
                .map(|e| e["newValue"]["path"].as_str().unwrap().to_owned())
                .collect()
        };
        let (status, v, _) = call(&app, "GET", log, auditor, None).await;
        assert_eq!(status, 200, "{v}");
        assert_eq!(
            paths(&v),
            [
                format!("/api/v1/configuration-items/{shown}"),
                "/api/v1/configuration-items/{hidden}".into(),
                "/api/v1/configuration-items/{hidden}/graph".into(),
                "/api/v1/configuration-items/{hidden}/impact".into(),
                "/api/v1/configuration-items/{hidden}/impact/export".into(),
                format!("/api/v1/configuration-items/{shown}/impact"),
                "/api/v1/configuration-items/{hidden}".into(),
                "/api/v1/relationships/{hidden}".into(),
            ]
        );
        let text = v.to_string();
        assert!(!text.contains(&hidden) && !text.contains(&hidden[1..]) && !text.contains(&edge), "{text}");

        // A reader who may view every class gets the paths as recorded.
        let (status, v, _) = call(&app, "GET", log, full, None).await;
        assert_eq!(status, 200, "{v}");
        // Its own read of the audit log comes last.
        assert_eq!(paths(&v)[..requests.len()], requests[..]);

        db.drop().await;
    }
}
