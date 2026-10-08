//! Saved views: storage, access and audit (SHAA-578 §2.3–§2.5, §3).
//!
//! A personal view is its owner's alone: anyone else gets the same `404` as for
//! a view that does not exist, administrators included (GDPR data
//! minimisation, §3.2). A shared view is read by every user who may view one of
//! its classes and changed only with `views.share` (§3.3) by a user who may
//! view every class it names (GH#508). Changes to shared
//! views are audited in the same transaction; personal views and defaults are
//! not (§3.4).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{PgConnection, PgPool};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use super::definition::{SavedViewContext, SavedViewDefinition, SavedViewVisibility, body_field};
use super::resolve::{Catalogue, SavedViewResolution, SavedViewState, Viewer, check_references, merge_hidden, resolve};
use crate::api::context::{ActorType, RequestContext};
use crate::api::route::Check;
use crate::api::schemas::{self, KEY_PATTERN, NOT_BLANK_PATTERN, patch_trimmed};
use crate::auth::permissions::GlobalPermission;
use crate::data::crud::{self, AuditAction, AuditEntry};
use crate::http::error::{AppError, ErrorCode, FieldError, FieldLocation};

/// Most personal views per user, both contexts together (§2.5).
pub const MAX_PERSONAL: i64 = 200;
/// Most shared views per instance, both contexts together (§2.5).
pub const MAX_SHARED: i64 = 500;

const ENTITY: &str = "saved_views";

// ---------------------------------------------------------------------------
// API shapes (§2.3, §2.4)
// ---------------------------------------------------------------------------

/// A user named on a view; the name stays when the account is deleted
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SavedViewUser {
    /// Null once the user was deleted
    pub id: Option<Uuid>,
    pub name: String,
}

/// A saved view as the caller may see it
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SavedView {
    pub id: Uuid,
    pub context: SavedViewContext,
    pub visibility: SavedViewVisibility,
    pub name: String,
    pub description: Option<String>,
    /// As stored, except that class keys the caller may not view are left out (counted in `resolved.issues`)
    pub definition: SavedViewDefinition,
    pub resolved: SavedViewResolution,
    /// Inventory views: the class whose list the view can be the default of; null for the unscoped inventory list
    /// (several classes or none), for a search view, and when the class is not available to the caller
    pub home: Option<String>,
    /// The caller's default for `home`
    pub is_default: bool,
    /// Shared views, for callers with `views.share`: how many users have it as their default
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub default_count: Option<i64>,
    /// Whether the caller may change and delete it: the owner of a personal view; for a shared one, `views.share` and
    /// the right to view every class it names
    pub can_edit: bool,
    /// Send it back with changes and deletes (optimistic concurrency)
    pub version: i32,
    pub created_at: DateTime<Utc>,
    pub created_by: SavedViewUser,
    pub updated_at: DateTime<Utc>,
    pub updated_by: SavedViewUser,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SavedViewLimit {
    pub used: i64,
    pub max: i64,
}

/// Personal: the caller's views; shared: the shared views available to the caller (the instance limit counts every
/// shared view). Both contexts count.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SavedViewLimits {
    pub personal: SavedViewLimit,
    pub shared: SavedViewLimit,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SavedViewList {
    /// The caller's personal views, then the shared views available to them, each by name (case-insensitive)
    pub data: Vec<SavedView>,
    pub limits: SavedViewLimits,
}

#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct ListSavedViewsQuery {
    /// Only the views of this context; left out: both
    #[param(inline)]
    pub context: Option<SavedViewContext>,
}

#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct DeleteSavedViewQuery {
    /// The version you loaded
    #[param(minimum = 1)]
    pub version: i32,
}

fn name_schema() -> utoipa::openapi::schema::Schema {
    utoipa::openapi::schema::ObjectBuilder::new()
        .schema_type(utoipa::openapi::schema::Type::String)
        .min_length(Some(1))
        .max_length(Some(100))
        .pattern(Some(NOT_BLANK_PATTERN))
        .description(Some(
            "Unique per owner and context, ignoring case. Shared views: unique per context across the instance, \
             including shared views not available to you",
        ))
        .into()
}

fn description_schema() -> utoipa::openapi::schema::Schema {
    schemas::nullable_string_schema(500)
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateSavedView {
    pub context: SavedViewContext,
    #[schema(schema_with = name_schema)]
    #[serde(deserialize_with = "schemas::trimmed")]
    pub name: String,
    #[schema(schema_with = description_schema)]
    #[serde(default, deserialize_with = "schemas::trimmed_opt")]
    pub description: Option<String>,
    /// shared needs `views.share`
    pub visibility: SavedViewVisibility,
    pub definition: SavedViewDefinition,
}

impl Check for CreateSavedView {
    fn check(&self) -> Vec<FieldError> {
        self.definition.problems(self.context, "definition")
    }
}

/// Changes to a view. Its context and visibility cannot change: copy it instead.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateSavedView {
    /// The version you loaded; if someone saved in between, `409 VERSION_CONFLICT`
    #[schema(minimum = 1)]
    pub version: i32,
    #[schema(schema_with = name_schema)]
    #[serde(default, deserialize_with = "schemas::trimmed_opt")]
    pub name: Option<String>,
    /// Null removes the description
    #[schema(schema_with = description_schema)]
    #[serde(default, deserialize_with = "patch_trimmed")]
    pub description: Option<Option<String>>,
    /// Replaces the definition. Class keys of a personal view that its owner may no longer view are kept
    #[schema(nullable = false)]
    #[serde(default)]
    pub definition: Option<SavedViewDefinition>,
}

impl Check for UpdateSavedView {
    fn check(&self) -> Vec<FieldError> {
        if self.name.is_none() && self.description.is_none() && self.definition.is_none() {
            return vec![body_field("(root)", "Provide at least one field to change", "custom")];
        }
        // The context is the stored view's; the search rules are checked again once it is known.
        self.definition.as_ref().map(|d| d.problems(SavedViewContext::Inventory, "definition")).unwrap_or_default()
    }
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CopySavedView {
    #[schema(schema_with = name_schema)]
    #[serde(deserialize_with = "schemas::trimmed")]
    pub name: String,
    /// shared needs `views.share`
    pub visibility: SavedViewVisibility,
}

impl Check for CopySavedView {}

/// Only inventory views can be a default (D7)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SavedViewDefaultContext {
    Inventory,
}

fn class_key_schema() -> utoipa::openapi::schema::Schema {
    utoipa::openapi::schema::AnyOfBuilder::new()
        .item(
            utoipa::openapi::schema::ObjectBuilder::new()
                .schema_type(utoipa::openapi::schema::Type::String)
                .pattern(Some(KEY_PATTERN)),
        )
        .item(utoipa::openapi::schema::ObjectBuilder::new().schema_type(utoipa::openapi::schema::Type::Null))
        .description(Some("The class whose list the default is for; null: the unscoped inventory list"))
        .into()
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SetSavedViewDefault {
    #[schema(inline)]
    pub context: SavedViewDefaultContext,
    #[schema(schema_with = class_key_schema)]
    pub class_key: Option<String>,
    /// Null clears the default
    #[schema(required = true)]
    pub view_id: Option<Uuid>,
}

impl Check for SetSavedViewDefault {}

/// The caller's default for one inventory list
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SavedViewDefault {
    #[schema(inline)]
    pub context: SavedViewDefaultContext,
    /// Null: the unscoped inventory list
    pub home: Option<String>,
    /// Null: no default (the admin list view applies)
    pub view_id: Option<Uuid>,
}

// ---------------------------------------------------------------------------
// Rows
// ---------------------------------------------------------------------------

#[derive(Debug, sqlx::FromRow)]
struct Row {
    id: Uuid,
    owner_id: Option<Uuid>,
    context: String,
    name: String,
    description: Option<String>,
    definition: sqlx::types::Json<Value>,
    version: i32,
    created_at: DateTime<Utc>,
    created_by_id: Option<Uuid>,
    created_by_name: String,
    updated_at: DateTime<Utc>,
    updated_by_id: Option<Uuid>,
    updated_by_name: String,
    /// The caller's default for the view's home
    is_default: bool,
    default_count: i64,
}

/// `$1`: the caller's user id.
const SELECT: &str = "SELECT v.id, v.owner_id, v.context, v.name, v.description, v.definition, v.version, \
    v.created_at, v.created_by_id, v.created_by_name, v.updated_at, v.updated_by_id, v.updated_by_name, \
    EXISTS (SELECT 1 FROM cmdb.saved_view_defaults d WHERE d.view_id = v.id AND d.user_id = $1) AS is_default, \
    (SELECT count(*) FROM cmdb.saved_view_defaults d WHERE d.view_id = v.id) AS default_count \
    FROM cmdb.saved_views v";

impl Row {
    fn context(&self) -> Result<SavedViewContext, AppError> {
        SavedViewContext::parse(&self.context).ok_or_else(AppError::internal)
    }

    fn definition(&self) -> Result<SavedViewDefinition, AppError> {
        serde_json::from_value(self.definition.0.clone()).map_err(|e| {
            tracing::error!(view = %self.id, error = %e, "stored saved view definition does not parse");
            AppError::internal()
        })
    }

    fn shared(&self) -> bool {
        self.owner_id.is_none()
    }

    /// What the audit log keeps of a shared view (§3.4).
    fn audit_value(&self) -> Value {
        json!({ "name": self.name, "description": self.description, "context": self.context,
                "visibility": "shared", "definition": self.definition.0 })
    }
}

/// The signed-in user. Every saved-view route needs a session (D8).
fn me(ctx: &RequestContext) -> Result<(Uuid, String), AppError> {
    ctx.principal()
        .map(|p| (p.user_id, p.username.clone()))
        .ok_or_else(|| AppError::new(ErrorCode::Unauthenticated, "Sign in first"))
}

fn not_found(id: Uuid) -> AppError {
    AppError::not_found(format!("Saved view {id} does not exist"))
}

fn may_share(ctx: &RequestContext) -> bool {
    ctx.require(GlobalPermission::ViewsShare).is_ok()
}

fn require_share(ctx: &RequestContext) -> Result<(), AppError> {
    if may_share(ctx) {
        return Ok(());
    }
    Err(AppError::new(
        ErrorCode::Forbidden,
        "Creating, changing and deleting shared views needs the views.share permission.",
    ))
}

/// A shared view that names a class the caller may not view: changing it could
/// widen it for those who may, deleting it would remove what they rely on (GH#508).
fn partly_hidden() -> AppError {
    AppError::new(
        ErrorCode::Forbidden,
        "This shared view includes classes you may not view, so you cannot change or delete it. Copy it instead.",
    )
}

fn coded(code: ErrorCode, message: &str, field: &str, detail: &str) -> AppError {
    AppError::new(code, message).with_details(vec![FieldError {
        location: FieldLocation::Body,
        field: field.into(),
        message: message.into(),
        code: detail.into(),
    }])
}

fn duplicate_name(shared: bool) -> AppError {
    let message = if shared {
        "A shared view of this context already has that name."
    } else {
        "You already have a view of this context with that name."
    };
    coded(ErrorCode::Conflict, message, "name", "duplicate_name")
}

fn is_duplicate_name(e: &sqlx::Error) -> bool {
    matches!(e, sqlx::Error::Database(d)
        if matches!(d.constraint(), Some("saved_views_personal_name_uq" | "saved_views_shared_name_uq")))
}

fn too_large() -> AppError {
    AppError::validation(vec![body_field("definition", "The definition is larger than 16 KiB", "too_large")])
}

fn is_too_large(e: &sqlx::Error) -> bool {
    matches!(e, sqlx::Error::Database(d) if d.constraint() == Some("saved_views_definition_check"))
}

/// A database error from writing a view, as the API reports it.
fn write_error(e: sqlx::Error, shared: bool) -> AppError {
    if is_duplicate_name(&e) {
        duplicate_name(shared)
    } else if is_too_large(&e) {
        too_large()
    } else {
        e.into()
    }
}

async fn fetch(conn: &mut PgConnection, me: Uuid, id: Uuid, lock: bool) -> sqlx::Result<Option<Row>> {
    let sql = format!("{SELECT} WHERE v.id = $2{}", if lock { " FOR UPDATE OF v" } else { "" });
    sqlx::query_as(sqlx::AssertSqlSafe(sql)).bind(me).bind(id).fetch_optional(conn).await
}

/// Whether the caller may read the view: their own personal view, or a shared
/// view with a class they may view (§3.2).
fn readable(row: &Row, def: &SavedViewDefinition, me: Uuid, viewer: &Viewer, cat: &Catalogue) -> bool {
    match row.owner_id {
        Some(owner) => owner == me,
        None => viewer.sees_shared(cat, def),
    }
}

struct Seen {
    row: Row,
    stored: SavedViewDefinition,
}

/// The view, if the caller may read it; `404` otherwise.
async fn fetch_readable(
    conn: &mut PgConnection,
    me: Uuid,
    id: Uuid,
    lock: bool,
    viewer: &Viewer,
    cat: &Catalogue,
) -> Result<Seen, AppError> {
    let Some(row) = fetch(conn, me, id, lock).await? else { return Err(not_found(id)) };
    let stored = row.definition()?;
    if !readable(&row, &stored, me, viewer, cat) {
        return Err(not_found(id));
    }
    Ok(Seen { row, stored })
}

fn dto(seen: &Seen, ctx: &RequestContext, viewer: &Viewer, cat: &Catalogue) -> Result<SavedView, AppError> {
    let Seen { row, stored } = seen;
    let context = row.context()?;
    let (definition, _) = viewer.visible_part(cat, stored);
    let resolved = resolve(stored, context, cat, viewer);
    let home = match context {
        SavedViewContext::Inventory => Some(stored.home()).filter(|h| !h.is_empty() && !viewer.hides(cat, h)),
        SavedViewContext::Search => None,
    };
    let home_hidden = context == SavedViewContext::Inventory && !stored.home().is_empty() && home.is_none();
    let shares = may_share(ctx);
    Ok(SavedView {
        id: row.id,
        context,
        visibility: if row.shared() { SavedViewVisibility::Shared } else { SavedViewVisibility::Personal },
        name: row.name.clone(),
        description: row.description.clone(),
        definition,
        resolved,
        home: home.map(str::to_owned),
        is_default: row.is_default && !home_hidden,
        default_count: (row.shared() && shares).then_some(row.default_count),
        can_edit: !row.shared() || (shares && viewer.sees_all(cat, stored)),
        version: row.version,
        created_at: row.created_at,
        created_by: SavedViewUser { id: row.created_by_id, name: row.created_by_name.clone() },
        updated_at: row.updated_at,
        updated_by: SavedViewUser { id: row.updated_by_id, name: row.updated_by_name.clone() },
    })
}

// ---------------------------------------------------------------------------
// Reads
// ---------------------------------------------------------------------------

/// The caller's personal views, and the shared views available to them: a
/// shared view none of whose classes they may view is not counted, so the
/// number does not tell that one exists (GH#507). `visible_keys`: see
/// [`Viewer::visible_keys`]; [`Viewer::sees_shared`] in SQL.
async fn counts(conn: &mut PgConnection, me: Uuid, visible_keys: Option<Vec<String>>) -> sqlx::Result<(i64, i64)> {
    sqlx::query_as(
        "SELECT count(*) FILTER (WHERE owner_id = $1),
           count(*) FILTER (WHERE owner_id IS NULL AND ($2::text[] IS NULL
             OR coalesce(definition -> 'classKeys', '[]'::jsonb) = '[]'::jsonb OR definition -> 'classKeys' ?| $2))
         FROM cmdb.saved_views WHERE owner_id = $1 OR owner_id IS NULL",
    )
    .bind(me)
    .bind(visible_keys)
    .fetch_one(conn)
    .await
}

pub async fn list(pool: &PgPool, ctx: &RequestContext, q: &ListSavedViewsQuery) -> Result<SavedViewList, AppError> {
    let (me, _) = me(ctx)?;
    let mut conn = pool.acquire().await?;
    let cat = Catalogue::load(&mut conn).await?;
    let viewer = Viewer::of(ctx);
    let rows: Vec<Row> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "{SELECT} WHERE (v.owner_id = $1 OR v.owner_id IS NULL) AND ($2::text IS NULL OR v.context = $2)
         ORDER BY v.owner_id IS NULL, lower(v.name), v.name, v.id"
    )))
    .bind(me)
    .bind(q.context.map(SavedViewContext::as_str))
    .fetch_all(&mut *conn)
    .await?;
    let mut data = Vec::with_capacity(rows.len());
    for row in rows {
        let stored = row.definition()?;
        if readable(&row, &stored, me, &viewer, &cat) {
            data.push(dto(&Seen { row, stored }, ctx, &viewer, &cat)?);
        }
    }
    let (personal, shared) = counts(&mut conn, me, viewer.visible_keys(&cat)).await?;
    Ok(SavedViewList {
        data,
        limits: SavedViewLimits {
            personal: SavedViewLimit { used: personal, max: MAX_PERSONAL },
            shared: SavedViewLimit { used: shared, max: MAX_SHARED },
        },
    })
}

// ---------------------------------------------------------------------------
// Result counts (SHAA-2352): the rail shows each view with how many CIs it has
// ---------------------------------------------------------------------------

/// Most views counted in one request.
pub const MAX_COUNTED: usize = 50;
/// Counts stop here: a larger result is reported as `at_least` this many.
pub const COUNT_CAP: i64 = 10_000;
/// Time for all counts of one request together; views not reached by then are `timed_out`.
const COUNT_BUDGET: std::time::Duration = std::time::Duration::from_secs(2);

/// Count requests that may count at once (GH#780): a quarter of `DATABASE_POOL_MAX`, at least one. Impact analysis
/// may take up to half the pool, so the rest of the API keeps at least a quarter while the rail counts.
pub fn count_slots(pool_max: u32) -> usize {
    (pool_max as usize / 4).max(1)
}

#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct SavedViewCountsQuery {
    /// Only the views of this context; left out: both
    #[param(inline)]
    pub context: Option<SavedViewContext>,
    /// View ids, comma-separated (at most 50), counted in this order. Left out: the first 50 views of
    /// `listSavedViews`, in its order. A view the caller may not read is left out, as if it did not exist.
    #[param(value_type = Option<String>, schema_with = view_ids_schema)]
    pub ids: Option<schemas::UuidList>,
}

fn view_ids_schema() -> utoipa::openapi::schema::Schema {
    schemas::uuid_list_described("View ids, comma-separated (at most 50)")
}

/// How a view's count came out
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SavedViewCountStatus {
    /// `count` is exact
    Counted,
    /// There are `count` (the cap) or more
    AtLeast,
    /// The view is `unavailable` (see `resolved.state`) and is never applied; no count
    Unavailable,
    /// The time for this request ran out before the view was counted; no count
    TimedOut,
}

/// The number of CIs a view shows the caller
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SavedViewCount {
    pub view_id: Uuid,
    #[schema(inline)]
    pub status: SavedViewCountStatus,
    /// Null when `unavailable` or `timed_out`
    pub count: Option<i64>,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SavedViewCounts {
    pub data: Vec<SavedViewCount>,
    /// The count limit: `at_least` results have this count
    pub cap: i64,
    /// Without `ids`: whether the caller has more than 50 views (of the context), so not all were counted
    pub truncated: bool,
}

/// A resolved view query as list filters (the same parameters the list would get).
struct ViewFilter {
    class_id: Option<schemas::UuidList>,
    include_subclasses: bool,
    active: crate::modules::items::schemas::ActiveQuery,
    lookup_value_id: Option<schemas::UuidList>,
    criticality_value_id: Option<schemas::UuidList>,
    deleted: schemas::Deleted,
    ip_within: Option<String>,
}

fn ids(s: &Option<String>) -> Option<schemas::UuidList> {
    s.as_deref().map(|s| schemas::UuidList(s.split(',').filter_map(|p| p.parse().ok()).collect()))
}

impl ViewFilter {
    fn of(q: &super::resolve::SavedViewQuery) -> Self {
        use super::definition::{SavedViewActive, SavedViewDeleted};
        use crate::modules::items::schemas::ActiveQuery;
        ViewFilter {
            class_id: ids(&q.class_id),
            include_subclasses: q.include_subclasses.as_deref() != Some("false"),
            active: match q.active {
                None | Some(SavedViewActive::True) => ActiveQuery::True,
                Some(SavedViewActive::False) => ActiveQuery::False,
                Some(SavedViewActive::All) => ActiveQuery::All,
            },
            lookup_value_id: ids(&q.lookup_value_id),
            criticality_value_id: ids(&q.criticality_value_id),
            deleted: match q.deleted {
                None | Some(SavedViewDeleted::Exclude) => schemas::Deleted::Exclude,
                Some(SavedViewDeleted::Include) => schemas::Deleted::Include,
                Some(SavedViewDeleted::Only) => schemas::Deleted::Only,
            },
            ip_within: q.ip_within.clone(),
        }
    }
}

impl crate::modules::items::schemas::ItemFilterQuery for ViewFilter {
    fn class_id(&self) -> Option<&schemas::UuidList> {
        self.class_id.as_ref()
    }
    fn include_subclasses(&self) -> bool {
        self.include_subclasses
    }
    fn active(&self) -> crate::modules::items::schemas::ActiveQuery {
        self.active
    }
    fn lookup_value_id(&self) -> Option<&schemas::UuidList> {
        self.lookup_value_id.as_ref()
    }
    fn ip_within(&self) -> Option<&str> {
        self.ip_within.as_deref()
    }
    fn criticality_value_id(&self) -> Option<&schemas::UuidList> {
        self.criticality_value_id.as_ref()
    }
    fn deleted(&self) -> schemas::Deleted {
        self.deleted
    }
    fn kind(&self) -> Option<crate::modules::items::schemas::KindQuery> {
        None
    }
    fn business_service_id(&self) -> Option<&schemas::UuidList> {
        None
    }
}

/// Counts what each view shows the caller: the view resolved as `listSavedViews` resolves it, run as a list (or, for
/// a search view, a search) request with the caller's class rights. Read-only, so not audited. Each count runs in a
/// savepoint under what is left of [`COUNT_BUDGET`], so one slow view cannot hold the page.
///
/// Counting takes one of `slots` ([`count_slots`]) first, waiting for it within the budget; a request that gets none
/// in time only lists its views (one short query) and answers them `timed_out`, so however many batches the rail
/// sends at once, they never hold more pool connections than there are slots.
pub async fn counts_of(
    pool: &PgPool,
    ctx: &RequestContext,
    slots: &tokio::sync::Semaphore,
    q: &SavedViewCountsQuery,
) -> Result<SavedViewCounts, AppError> {
    use crate::modules::impact::engine::{is_query_canceled, remaining_ms};
    use sqlx::Connection;

    let (me, _) = me(ctx)?;
    if q.ids.as_ref().is_some_and(|l| l.0.len() > MAX_COUNTED) {
        return Err(AppError::validation(vec![FieldError {
            location: FieldLocation::Query,
            field: "ids".into(),
            message: format!("At most {MAX_COUNTED} views per request"),
            code: "too_big".into(),
        }]));
    }
    let deadline = tokio::time::Instant::now() + COUNT_BUDGET;
    let slot = tokio::time::timeout_at(deadline, slots.acquire()).await.ok().and_then(Result::ok);
    let mut tx = pool.begin().await?;
    let cat = Catalogue::load(&mut tx).await?;
    let viewer = Viewer::of(ctx);
    let rows: Vec<Row> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "{SELECT} WHERE (v.owner_id = $1 OR v.owner_id IS NULL) AND ($2::text IS NULL OR v.context = $2)
           AND ($3::uuid[] IS NULL OR v.id = ANY($3))
         ORDER BY v.owner_id IS NULL, lower(v.name), v.name, v.id"
    )))
    .bind(me)
    .bind(q.context.map(SavedViewContext::as_str))
    .bind(q.ids.as_ref().map(|l| l.0.clone()))
    .fetch_all(&mut *tx)
    .await?;
    let mut views = Vec::new();
    for row in rows {
        let stored = row.definition()?;
        if readable(&row, &stored, me, &viewer, &cat) {
            let context = row.context()?;
            views.push((row.id, resolve(&stored, context, &cat, &viewer).query));
        }
    }
    if let Some(order) = &q.ids {
        views.sort_by_key(|(id, _)| order.0.iter().position(|o| o == id));
    }
    let truncated = q.ids.is_none() && views.len() > MAX_COUNTED;
    views.truncate(MAX_COUNTED);

    let model = match slot {
        Some(_) => Some(crate::schema::model::Model::load(&mut tx).await?),
        None => None,
    };
    let mut data = Vec::with_capacity(views.len());
    for (view_id, query) in views {
        let Some(query) = query else {
            data.push(SavedViewCount { view_id, status: SavedViewCountStatus::Unavailable, count: None });
            continue;
        };
        let timed_out = SavedViewCount { view_id, status: SavedViewCountStatus::TimedOut, count: None };
        let (Some(model), Some(ms)) = (&model, remaining_ms(deadline)) else {
            data.push(timed_out);
            continue;
        };
        let filter = ViewFilter::of(&query);
        let f = crate::modules::items::service::list_filters(&mut tx, ctx, model, &filter, query.q.as_deref()).await?;
        let mut sp = tx.begin().await?;
        crate::data::impact::set_statement_timeout(&mut sp, ms).await?;
        let counted = crate::data::items::count_capped(&mut sp, &f, COUNT_CAP).await;
        sp.rollback().await?;
        match counted {
            Ok(n) => {
                let status = if n >= COUNT_CAP { SavedViewCountStatus::AtLeast } else { SavedViewCountStatus::Counted };
                data.push(SavedViewCount { view_id, status, count: Some(n) });
            }
            Err(e) if is_query_canceled(&e) => data.push(timed_out),
            // A stored value the list cannot apply (an `ipWithin` that is not a CIDR): the list would
            // refuse it, so the view has no count; the savepoint keeps the other counts going.
            Err(e) if is_data_exception(&e) => {
                data.push(SavedViewCount { view_id, status: SavedViewCountStatus::Unavailable, count: None })
            }
            Err(e) => return Err(e.into()),
        }
    }
    tx.rollback().await?;
    drop(slot);
    Ok(SavedViewCounts { data, cap: COUNT_CAP, truncated })
}

/// A PostgreSQL data exception (SQLSTATE class 22), e.g. a value that does not cast to `inet`.
fn is_data_exception(e: &sqlx::Error) -> bool {
    e.as_database_error().and_then(|d| d.code()).is_some_and(|c| c.starts_with("22"))
}

pub async fn get(pool: &PgPool, ctx: &RequestContext, id: Uuid) -> Result<SavedView, AppError> {
    let (me, _) = me(ctx)?;
    let mut conn = pool.acquire().await?;
    let cat = Catalogue::load(&mut conn).await?;
    let viewer = Viewer::of(ctx);
    let seen = fetch_readable(&mut conn, me, id, false, &viewer, &cat).await?;
    dto(&seen, ctx, &viewer, &cat)
}

// ---------------------------------------------------------------------------
// Writes
// ---------------------------------------------------------------------------

/// Serialises creates per owner (or for the shared views), so the limits hold
/// under concurrency.
async fn lock_and_check_limit(conn: &mut PgConnection, owner: Option<Uuid>) -> Result<(), AppError> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext('shadoucmdb:saved-views:' || coalesce($1::text, 'shared')))")
        .bind(owner)
        .execute(&mut *conn)
        .await?;
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM cmdb.saved_views WHERE owner_id IS NOT DISTINCT FROM $1")
        .bind(owner)
        .fetch_one(&mut *conn)
        .await?;
    let (max, message) = match owner {
        Some(_) => (MAX_PERSONAL, "You have reached the limit of 200 saved views. Delete views you no longer need."),
        None => (
            MAX_SHARED,
            "This instance has reached the limit of 500 shared views. Delete shared views no longer needed.",
        ),
    };
    if count >= max {
        return Err(coded(ErrorCode::Conflict, message, "(root)", "limit_reached"));
    }
    Ok(())
}

struct NewView<'a> {
    owner: Option<Uuid>,
    context: SavedViewContext,
    name: &'a str,
    description: Option<&'a str>,
    definition: &'a SavedViewDefinition,
}

/// Inserts the view (after [`lock_and_check_limit`]) and audits a shared one.
/// `extra` is merged into the audit value (`copiedFrom`).
async fn insert(
    conn: &mut PgConnection,
    ctx: &RequestContext,
    user: (Option<Uuid>, &str),
    v: NewView<'_>,
    extra: Option<(&str, Value)>,
) -> Result<Uuid, AppError> {
    let inserted: Result<Uuid, sqlx::Error> = sqlx::query_scalar(
        "INSERT INTO cmdb.saved_views
           (owner_id, context, name, description, definition, created_by_id, created_by_name, updated_by_id,
            updated_by_name)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $6, $7) RETURNING id",
    )
    .bind(v.owner)
    .bind(v.context.as_str())
    .bind(v.name.trim())
    .bind(v.description.map(str::trim).filter(|d| !d.is_empty()))
    .bind(sqlx::types::Json(v.definition))
    .bind(user.0)
    .bind(user.1)
    .fetch_one(&mut *conn)
    .await;
    let id = inserted.map_err(|e| write_error(e, v.owner.is_none()))?;
    if v.owner.is_none() {
        let row = fetch(conn, user.0.unwrap_or_default(), id, false).await?.ok_or_else(AppError::internal)?;
        let mut value = row.audit_value();
        if let (Some((k, x)), Some(obj)) = (extra, value.as_object_mut()) {
            obj.insert(k.into(), x);
        }
        let entry = AuditEntry {
            action: AuditAction::Create,
            entity_type: ENTITY,
            entity_id: id,
            old_value: None,
            new_value: Some(value),
        };
        crud::write_audit(conn, ctx, vec![entry]).await?;
    }
    Ok(id)
}

/// A definition ready to store, or every problem with it.
fn checked(
    def: &SavedViewDefinition,
    context: SavedViewContext,
    cat: &Catalogue,
    viewer: &Viewer,
) -> Result<(), AppError> {
    let mut e = def.problems(context, "definition");
    e.extend(check_references(def, context, cat, viewer, "definition"));
    if e.is_empty() { Ok(()) } else { Err(AppError::validation(e)) }
}

async fn respond(
    conn: &mut PgConnection,
    ctx: &RequestContext,
    me: Uuid,
    id: Uuid,
    viewer: &Viewer,
    cat: &Catalogue,
) -> Result<SavedView, AppError> {
    let row = fetch(conn, me, id, false).await?.ok_or_else(AppError::internal)?;
    let stored = row.definition()?;
    dto(&Seen { row, stored }, ctx, viewer, cat)
}

pub async fn create(pool: &PgPool, ctx: &RequestContext, input: &CreateSavedView) -> Result<SavedView, AppError> {
    let (me, username) = me(ctx)?;
    let shared = input.visibility == SavedViewVisibility::Shared;
    if shared {
        require_share(ctx)?;
    }
    let mut tx = pool.begin().await?;
    let cat = Catalogue::load(&mut tx).await?;
    let viewer = Viewer::of(ctx);
    checked(&input.definition, input.context, &cat, &viewer)?;
    let owner = (!shared).then_some(me);
    lock_and_check_limit(&mut tx, owner).await?;
    let view = NewView {
        owner,
        context: input.context,
        name: &input.name,
        description: input.description.as_deref(),
        definition: &input.definition,
    };
    let id = insert(&mut tx, ctx, (Some(me), &username), view, None).await?;
    let out = respond(&mut tx, ctx, me, id, &viewer, &cat).await?;
    tx.commit().await?;
    Ok(out)
}

fn version_conflict(sent: i32, row: &Row) -> AppError {
    let current = row.version;
    AppError::new(
        ErrorCode::VersionConflict,
        format!(
            "The saved view was changed by {} (you sent version {sent}, current is {current}). Reload and retry.",
            row.updated_by_name
        ),
    )
    .with_details(vec![body_field("version", &format!("Current version is {current}"), "stale")])
}

/// The view, locked, if the caller may change it: `404` when they may not read
/// it, `403` for a shared view without `views.share` or with a class they may
/// not view, `409 VERSION_CONFLICT` when stale.
async fn for_change(
    conn: &mut PgConnection,
    ctx: &RequestContext,
    me: Uuid,
    id: Uuid,
    version: i32,
    viewer: &Viewer,
    cat: &Catalogue,
) -> Result<Seen, AppError> {
    let seen = fetch_readable(conn, me, id, true, viewer, cat).await?;
    if seen.row.shared() {
        require_share(ctx)?;
        if !viewer.sees_all(cat, &seen.stored) {
            return Err(partly_hidden());
        }
    }
    if seen.row.version != version {
        return Err(version_conflict(version, &seen.row));
    }
    Ok(seen)
}

pub async fn update(
    pool: &PgPool,
    ctx: &RequestContext,
    id: Uuid,
    input: &UpdateSavedView,
) -> Result<SavedView, AppError> {
    let (me, username) = me(ctx)?;
    let mut tx = pool.begin().await?;
    let cat = Catalogue::load(&mut tx).await?;
    let viewer = Viewer::of(ctx);
    let before = for_change(&mut tx, ctx, me, id, input.version, &viewer, &cat).await?;
    let context = before.row.context()?;
    let definition = match &input.definition {
        None => before.stored.clone(),
        Some(d) => {
            checked(d, context, &cat, &viewer)?;
            let merged = merge_hidden(&viewer, &cat, &before.stored, d.clone());
            if merged.json_bytes() > super::definition::MAX_DEFINITION_BYTES {
                return Err(too_large());
            }
            merged
        }
    };
    let description = match &input.description {
        None => before.row.description.clone(),
        Some(d) => d.clone().filter(|d| !d.is_empty()),
    };
    sqlx::query(
        "UPDATE cmdb.saved_views SET name = $2, description = $3, definition = $4, version = version + 1,
           updated_by_id = $5, updated_by_name = $6
         WHERE id = $1",
    )
    .bind(id)
    .bind(input.name.as_deref().unwrap_or(&before.row.name))
    .bind(description)
    .bind(sqlx::types::Json(&definition))
    .bind(me)
    .bind(&username)
    .execute(&mut *tx)
    .await
    .map_err(|e| write_error(e, before.row.shared()))?;
    // A default belongs to one list: when the view's home moves, defaults for the old home no longer match it.
    sqlx::query("DELETE FROM cmdb.saved_view_defaults WHERE view_id = $1 AND home <> $2")
        .bind(id)
        .bind(definition.home())
        .execute(&mut *tx)
        .await?;
    let after = fetch(&mut tx, me, id, false).await?.ok_or_else(AppError::internal)?;
    if after.shared() {
        let entry = AuditEntry {
            action: AuditAction::Update,
            entity_type: ENTITY,
            entity_id: id,
            old_value: Some(before.row.audit_value()),
            new_value: Some(after.audit_value()),
        };
        crud::write_audit(&mut tx, ctx, vec![entry]).await?;
    }
    let stored = after.definition()?;
    let out = dto(&Seen { row: after, stored }, ctx, &viewer, &cat)?;
    tx.commit().await?;
    Ok(out)
}

/// Deletes a view; defaults that point at it go with it (cascade).
pub async fn delete(pool: &PgPool, ctx: &RequestContext, id: Uuid, version: i32) -> Result<(), AppError> {
    let (me, _) = me(ctx)?;
    let mut tx = pool.begin().await?;
    let cat = Catalogue::load(&mut tx).await?;
    let viewer = Viewer::of(ctx);
    let before = for_change(&mut tx, ctx, me, id, version, &viewer, &cat).await?;
    sqlx::query("DELETE FROM cmdb.saved_views WHERE id = $1").bind(id).execute(&mut *tx).await?;
    if before.row.shared() {
        let entry = AuditEntry {
            action: AuditAction::Delete,
            entity_type: ENTITY,
            entity_id: id,
            old_value: Some(before.row.audit_value()),
            new_value: None,
        };
        crud::write_audit(&mut tx, ctx, vec![entry]).await?;
    }
    tx.commit().await?;
    Ok(())
}

/// "Save as", "Copy to my views" and "Share a copy": a new view of the same
/// context with the source's description and the part of its definition the
/// caller may see. The source stays as it is.
pub async fn copy(pool: &PgPool, ctx: &RequestContext, id: Uuid, input: &CopySavedView) -> Result<SavedView, AppError> {
    let (me, username) = me(ctx)?;
    let mut tx = pool.begin().await?;
    let cat = Catalogue::load(&mut tx).await?;
    let viewer = Viewer::of(ctx);
    let source = fetch_readable(&mut tx, me, id, false, &viewer, &cat).await?;
    let shared = input.visibility == SavedViewVisibility::Shared;
    if shared {
        require_share(ctx)?;
    }
    let (definition, _) = viewer.visible_part(&cat, &source.stored);
    let owner = (!shared).then_some(me);
    lock_and_check_limit(&mut tx, owner).await?;
    let view = NewView {
        owner,
        context: source.row.context()?,
        name: &input.name,
        description: source.row.description.as_deref(),
        definition: &definition,
    };
    let new_id = insert(&mut tx, ctx, (Some(me), &username), view, Some(("copiedFrom", json!(id)))).await?;
    let out = respond(&mut tx, ctx, me, new_id, &viewer, &cat).await?;
    tx.commit().await?;
    Ok(out)
}

/// Sets or clears the caller's default for one inventory list. Not audited:
/// a default is a personal preference (§3.4).
pub async fn set_default(
    pool: &PgPool,
    ctx: &RequestContext,
    input: &SetSavedViewDefault,
) -> Result<SavedViewDefault, AppError> {
    let (me, _) = me(ctx)?;
    let home = input.class_key.clone().unwrap_or_default();
    let mut tx = pool.begin().await?;
    match input.view_id {
        None => {
            sqlx::query(
                "DELETE FROM cmdb.saved_view_defaults WHERE user_id = $1 AND context = 'inventory' AND home = $2",
            )
            .bind(me)
            .bind(&home)
            .execute(&mut *tx)
            .await?;
        }
        Some(view_id) => {
            let cat = Catalogue::load(&mut tx).await?;
            let viewer = Viewer::of(ctx);
            let seen = fetch_readable(&mut tx, me, view_id, false, &viewer, &cat).await?;
            if seen.row.context()? != SavedViewContext::Inventory {
                return Err(AppError::validation(vec![body_field(
                    "viewId",
                    "A search view cannot be a default",
                    "not_inventory",
                )]));
            }
            if seen.stored.home() != home || (!home.is_empty() && viewer.hides(&cat, &home)) {
                return Err(AppError::validation(vec![body_field(
                    "classKey",
                    "The view does not belong to this list: its home is its class when it has exactly one, else the \
                     unscoped inventory",
                    "home_mismatch",
                )]));
            }
            if resolve(&seen.stored, SavedViewContext::Inventory, &cat, &viewer).state == SavedViewState::Unavailable {
                return Err(AppError::validation(vec![body_field(
                    "viewId",
                    "This view refers to a filter that no longer exists and cannot be a default",
                    "unavailable",
                )]));
            }
            sqlx::query(
                "INSERT INTO cmdb.saved_view_defaults (user_id, context, home, view_id) VALUES ($1, 'inventory', $2, $3)
                 ON CONFLICT (user_id, context, home) DO UPDATE SET view_id = EXCLUDED.view_id",
            )
            .bind(me)
            .bind(&home)
            .bind(view_id)
            .execute(&mut *tx)
            .await?;
        }
    }
    tx.commit().await?;
    Ok(SavedViewDefault { context: input.context, home: input.class_key.clone(), view_id: input.view_id })
}

// ---------------------------------------------------------------------------
// Config export and import (§4): shared views only
// ---------------------------------------------------------------------------

/// A shared view as the configuration file carries it.
#[derive(Debug)]
pub(crate) struct ConfigView {
    pub id: Uuid,
    pub context: SavedViewContext,
    pub name: String,
    pub description: Option<String>,
    pub definition: SavedViewDefinition,
}

/// Every shared view, by context and name. A stored definition that no longer
/// parses is left out (and logged), as `GET` would refuse it too.
pub(crate) async fn shared_for_config(conn: &mut PgConnection) -> sqlx::Result<Vec<ConfigView>> {
    let rows: Vec<Row> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "{SELECT} WHERE v.owner_id IS NULL ORDER BY v.context, lower(v.name), v.name, v.id"
    )))
    .bind(Uuid::nil())
    .fetch_all(conn)
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(|r| {
            let definition = r.definition().ok()?;
            let context = r.context().ok()?;
            Some(ConfigView { id: r.id, context, name: r.name, description: r.description, definition })
        })
        .collect())
}

/// The part of a shared view the caller may see, for the export; `None` when
/// none of its classes is visible to them (§3.2).
pub(crate) fn exportable(
    ctx: &RequestContext,
    cat: &Catalogue,
    def: &SavedViewDefinition,
) -> Option<SavedViewDefinition> {
    let viewer = Viewer::of(ctx);
    viewer.sees_shared(cat, def).then(|| viewer.visible_part(cat, def).0)
}

/// Creates the shared view (`existing` is `None`) or replaces the description
/// and definition of `existing`, which keeps its name. The existing view's
/// class keys the caller may not view are kept. Audited like the API, with
/// `actor_type = import` (§3.4).
pub(crate) async fn config_write(
    conn: &mut PgConnection,
    ctx: &RequestContext,
    existing: Option<Uuid>,
    context: SavedViewContext,
    name: &str,
    description: Option<&str>,
    definition: &SavedViewDefinition,
) -> Result<Uuid, AppError> {
    let mut actx = ctx.clone();
    actx.actor.actor_type = ActorType::Import;
    let user_id = ctx.principal().map(|p| p.user_id);
    let user_name = ctx.principal().map(|p| p.username.clone()).or_else(|| ctx.actor.name.clone()).unwrap_or_default();
    let user_name = user_name.as_str();
    match existing {
        None => {
            lock_and_check_limit(conn, None).await?;
            let view = NewView { owner: None, context, name, description, definition };
            insert(conn, &actx, (user_id, user_name), view, None).await
        }
        Some(id) => {
            let me = user_id.unwrap_or_default();
            let before = fetch(conn, me, id, true).await?.ok_or_else(|| not_found(id))?;
            let cat = Catalogue::load(conn).await?;
            let viewer = Viewer::of(ctx);
            let stored = before.definition()?;
            // A shared view the importer cannot see is not theirs to rewrite (GH#476), nor one
            // they see only part of (GH#508); the import skips both with a warning.
            if !viewer.sees_shared(&cat, &stored) {
                return Err(not_found(id));
            }
            if !viewer.sees_all(&cat, &stored) {
                return Err(partly_hidden());
            }
            let merged = merge_hidden(&viewer, &cat, &stored, definition.clone());
            if merged.json_bytes() > super::definition::MAX_DEFINITION_BYTES {
                return Err(too_large());
            }
            sqlx::query(
                "UPDATE cmdb.saved_views SET description = $2, definition = $3, version = version + 1,
                   updated_by_id = $4, updated_by_name = $5
                 WHERE id = $1",
            )
            .bind(id)
            .bind(description.map(str::trim).filter(|d| !d.is_empty()))
            .bind(sqlx::types::Json(&merged))
            .bind(user_id)
            .bind(user_name)
            .execute(&mut *conn)
            .await
            .map_err(|e| write_error(e, true))?;
            sqlx::query("DELETE FROM cmdb.saved_view_defaults WHERE view_id = $1 AND home <> $2")
                .bind(id)
                .bind(merged.home())
                .execute(&mut *conn)
                .await?;
            let after = fetch(conn, me, id, false).await?.ok_or_else(AppError::internal)?;
            let entry = AuditEntry {
                action: AuditAction::Update,
                entity_type: ENTITY,
                entity_id: id,
                old_value: Some(before.audit_value()),
                new_value: Some(after.audit_value()),
            };
            crud::write_audit(conn, &actx, vec![entry]).await?;
            Ok(id)
        }
    }
}
