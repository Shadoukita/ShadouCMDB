//! Saved mappings (D9, §3.2): a spreadsheet layout kept for the next file.
//!
//! A saved mapping is shared with everyone who has `cis.import` and can view
//! its class; anyone else gets the same `404` as for a mapping that does not
//! exist, and hidden ones are left out of lists. Names are unique per class,
//! so a taken name reveals nothing about classes the caller cannot see (T18).
//! Only the creator and administrators may change or delete one. Every change
//! is audited with the full definition (§4.3).

use std::collections::HashSet;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{PgConnection, PgPool};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use super::jobs::{ctx_user, require_enabled};
use super::schemas::{ColumnOptions, ColumnTarget, EmptyCells, ImportMode, JobOwner, MappingOptions, MatchKey};
use super::suggest::normalise;
use super::{MAX_COLUMNS, body_field, coded};
use crate::api::context::RequestContext;
use crate::api::route::Check;
use crate::api::schemas::patch_trimmed;
use crate::auth::permissions::ClassOp;
use crate::config::ImportConfig;
use crate::data::crud::{self, AuditAction, AuditEntry};
use crate::http::error::{AppError, ErrorCode, FieldError};

/// Most saved mappings per instance (§3.5).
pub const MAX_SAVED: i64 = 500;
/// Largest definition, as stored JSON (§3.5).
pub const MAX_DEFINITION_BYTES: usize = 64 * 1024;

/// A column of a saved mapping, found in a file by its header.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schema(as = ImportDefinitionColumn)]
pub struct DefinitionColumn {
    /// The column's header; compared ignoring case, spaces, `_`, `-` and `.`
    #[schema(min_length = 1, max_length = 1000)]
    pub header: String,
    pub target: ColumnTarget,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub empty_cells: Option<EmptyCells>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<ColumnOptions>,
}

/// A mapping without its class, with columns named by header, so it applies
/// to any file with those headers. Save `ignore` targets for the columns the
/// layout skips: a file whose header set equals the definition's headers gets
/// the mapping suggested without being asked (§3.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schema(as = ImportMappingDefinition)]
pub struct MappingDefinition {
    pub mode: ImportMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<MatchKey>,
    #[serde(default = "ignore")]
    pub empty_cells: EmptyCells,
    #[serde(default)]
    pub options: MappingOptions,
    #[schema(max_items = 200)]
    pub columns: Vec<DefinitionColumn>,
}

fn ignore() -> EmptyCells {
    EmptyCells::Ignore
}

impl MappingDefinition {
    /// The target the definition gives a header, if it names it.
    pub fn target_for(&self, header: &str) -> Option<&DefinitionColumn> {
        let wanted = normalise(header);
        self.columns
            .iter()
            .find(|c| c.header == header)
            .or_else(|| self.columns.iter().find(|c| normalise(&c.header) == wanted))
    }

    /// The set of normalised headers the definition names.
    pub fn header_set(&self) -> HashSet<String> {
        self.columns.iter().map(|c| normalise(&c.header)).collect()
    }

    /// Structural problems, reported under `prefix` (e.g. `definition`).
    fn check(&self, prefix: &str) -> Vec<FieldError> {
        let mut errors = Vec::new();
        if self.columns.len() > MAX_COLUMNS as usize {
            errors.push(body_field(&format!("{prefix}.columns"), "At most 200 columns", "too_many"));
        }
        let mut seen = HashSet::new();
        for (i, c) in self.columns.iter().enumerate() {
            if c.header.trim().is_empty() {
                errors.push(body_field(&format!("{prefix}.columns[{i}].header"), "Required", "required"));
            } else if !seen.insert(normalise(&c.header)) {
                errors.push(body_field(
                    &format!("{prefix}.columns[{i}].header"),
                    "Another column has the same header",
                    "duplicate_header",
                ));
            }
        }
        if serde_json::to_vec(self).map(|v| v.len()).unwrap_or(usize::MAX) > MAX_DEFINITION_BYTES {
            errors.push(body_field(prefix, "The definition is larger than 64 KiB", "too_large"));
        }
        errors
    }
}

/// A saved mapping (D9).
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SavedImportMapping {
    pub id: Uuid,
    pub name: String,
    pub description: Option<String>,
    /// The class the mapping imports into, by key
    pub class_key: String,
    pub definition: MappingDefinition,
    /// Send it back with changes and deletes (optimistic concurrency)
    pub version: i32,
    pub created_at: DateTime<Utc>,
    pub created_by: JobOwner,
    pub updated_at: DateTime<Utc>,
    pub updated_by: JobOwner,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SavedImportMappingList {
    /// Sorted by name
    pub data: Vec<SavedImportMapping>,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateSavedImportMapping {
    #[schema(min_length = 1, max_length = 100)]
    pub name: String,
    #[serde(default)]
    #[schema(max_length = 500)]
    pub description: Option<String>,
    #[schema(min_length = 1, max_length = 63)]
    pub class_key: String,
    pub definition: MappingDefinition,
}

impl Check for CreateSavedImportMapping {
    fn check(&self) -> Vec<FieldError> {
        let mut errors = check_name(&self.name);
        errors.extend(self.definition.check("definition"));
        errors
    }
}

/// Changes to a saved mapping; the class cannot change.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateSavedImportMapping {
    /// The version you loaded; if someone saved in between, `409 VERSION_CONFLICT`
    #[schema(minimum = 1)]
    pub version: i32,
    #[serde(default)]
    #[schema(min_length = 1, max_length = 100, nullable = false)]
    pub name: Option<String>,
    /// Null removes the description
    #[serde(default, deserialize_with = "patch_trimmed")]
    #[schema(max_length = 500)]
    pub description: Option<Option<String>>,
    #[serde(default)]
    #[schema(nullable = false)]
    pub definition: Option<MappingDefinition>,
}

impl Check for UpdateSavedImportMapping {
    fn check(&self) -> Vec<FieldError> {
        let mut errors = self.name.as_deref().map(check_name).unwrap_or_default();
        if let Some(d) = &self.definition {
            errors.extend(d.check("definition"));
        }
        errors
    }
}

fn check_name(name: &str) -> Vec<FieldError> {
    if name.trim().is_empty() { vec![body_field("name", "Required", "required")] } else { Vec::new() }
}

#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct ListSavedMappingsQuery {
    /// Only the mappings of this class
    #[param(min_length = 1, max_length = 63)]
    pub class_key: Option<String>,
}

#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct DeleteSavedMappingQuery {
    /// The version you loaded
    #[param(minimum = 1)]
    pub version: i32,
}

#[derive(Debug, sqlx::FromRow)]
struct Row {
    id: Uuid,
    name: String,
    description: Option<String>,
    class_key: String,
    definition: sqlx::types::Json<Value>,
    version: i32,
    created_at: DateTime<Utc>,
    created_by_id: Option<Uuid>,
    created_by_name: String,
    updated_at: DateTime<Utc>,
    updated_by_id: Option<Uuid>,
    updated_by_name: String,
    /// The class's id; null when no class has the key any more.
    class_id: Option<Uuid>,
}

const SELECT: &str = "SELECT m.id, m.name, m.description, m.class_key, m.definition, m.version, m.created_at, \
    m.created_by_id, m.created_by_name, m.updated_at, m.updated_by_id, m.updated_by_name, c.id AS class_id \
    FROM cmdb.import_mappings m LEFT JOIN cmdb.ci_classes c ON c.key = m.class_key";

impl Row {
    fn dto(&self) -> Result<SavedImportMapping, AppError> {
        let definition = serde_json::from_value(self.definition.0.clone()).map_err(|e| {
            tracing::error!(mapping = %self.id, error = %e, "stored import mapping definition does not parse");
            AppError::internal()
        })?;
        Ok(SavedImportMapping {
            id: self.id,
            name: self.name.clone(),
            description: self.description.clone(),
            class_key: self.class_key.clone(),
            definition,
            version: self.version,
            created_at: self.created_at,
            created_by: JobOwner { id: self.created_by_id, name: self.created_by_name.clone() },
            updated_at: self.updated_at,
            updated_by: JobOwner { id: self.updated_by_id, name: self.updated_by_name.clone() },
        })
    }

    fn visible(&self, ctx: &RequestContext) -> bool {
        self.class_id.is_some_and(|c| ctx.require_class(c, ClassOp::View).is_ok())
    }

    /// What the audit log keeps of it.
    fn audit_value(&self) -> Value {
        json!({ "classKey": self.class_key, "name": self.name, "description": self.description,
                "definition": self.definition.0 })
    }
}

fn not_found(id: Uuid) -> AppError {
    AppError::not_found(format!("Saved mapping {id} does not exist"))
}

fn unknown_class(key: &str) -> AppError {
    AppError::validation(vec![body_field("classKey", &format!("CI class \"{key}\" does not exist"), "unknown_class")])
}

fn duplicate_name() -> AppError {
    AppError::new(ErrorCode::Conflict, "This class already has a saved mapping with that name.").with_details(vec![
        body_field("name", "This class already has a saved mapping with that name", "duplicate_name"),
    ])
}

fn is_duplicate_name(e: &sqlx::Error) -> bool {
    matches!(e, sqlx::Error::Database(d) if d.constraint() == Some("import_mappings_name_uq"))
}

fn user_name(ctx: &RequestContext) -> String {
    ctx.principal().map(|p| p.username.clone()).or_else(|| ctx.actor.name.clone()).unwrap_or_default()
}

/// The id of a class the caller can view, by key.
async fn visible_class(conn: &mut PgConnection, ctx: &RequestContext, key: &str) -> sqlx::Result<Option<Uuid>> {
    let id: Option<Uuid> =
        sqlx::query_scalar("SELECT id FROM cmdb.ci_classes WHERE key = $1").bind(key).fetch_optional(conn).await?;
    Ok(id.filter(|c| ctx.require_class(*c, ClassOp::View).is_ok()))
}

async fn fetch(conn: &mut PgConnection, id: Uuid, lock: bool) -> sqlx::Result<Option<Row>> {
    let sql = format!("{SELECT} WHERE m.id = $1{}", if lock { " FOR UPDATE OF m" } else { "" });
    sqlx::query_as(sqlx::AssertSqlSafe(sql)).bind(id).fetch_optional(conn).await
}

/// A saved mapping the caller may see; `404` otherwise.
pub async fn fetch_visible(
    conn: &mut PgConnection,
    ctx: &RequestContext,
    id: Uuid,
) -> Result<SavedImportMapping, AppError> {
    match fetch(conn, id, false).await? {
        Some(r) if r.visible(ctx) => r.dto(),
        _ => Err(not_found(id)),
    }
}

/// The caller's visible saved mappings, of one class or all.
pub async fn visible_of(
    conn: &mut PgConnection,
    ctx: &RequestContext,
    class_key: Option<&str>,
) -> Result<Vec<SavedImportMapping>, AppError> {
    let rows: Vec<Row> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "{SELECT} WHERE ($1::text IS NULL OR m.class_key = $1) ORDER BY lower(m.name), m.name, m.id"
    )))
    .bind(class_key)
    .fetch_all(conn)
    .await?;
    rows.iter().filter(|r| r.visible(ctx)).map(Row::dto).collect()
}

pub async fn list(
    pool: &PgPool,
    ctx: &RequestContext,
    q: &ListSavedMappingsQuery,
) -> Result<SavedImportMappingList, AppError> {
    let mut conn = pool.acquire().await?;
    Ok(SavedImportMappingList { data: visible_of(&mut conn, ctx, q.class_key.as_deref()).await? })
}

pub async fn get(pool: &PgPool, ctx: &RequestContext, id: Uuid) -> Result<SavedImportMapping, AppError> {
    let mut conn = pool.acquire().await?;
    fetch_visible(&mut conn, ctx, id).await
}

pub async fn create(
    pool: &PgPool,
    ctx: &RequestContext,
    cfg: &ImportConfig,
    input: &CreateSavedImportMapping,
) -> Result<SavedImportMapping, AppError> {
    let mut tx = pool.begin().await?;
    require_enabled(&mut tx, cfg).await?;
    if visible_class(&mut tx, ctx, &input.class_key).await?.is_none() {
        return Err(unknown_class(&input.class_key));
    }
    // Serialises creates, so the instance limit holds under concurrency.
    sqlx::query("LOCK TABLE cmdb.import_mappings IN SHARE ROW EXCLUSIVE MODE").execute(&mut *tx).await?;
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM cmdb.import_mappings").fetch_one(&mut *tx).await?;
    if count >= MAX_SAVED {
        return Err(coded(
            ErrorCode::Conflict,
            "This instance already has 500 saved mappings. Delete one you no longer need.",
            "limit_reached",
        ));
    }
    let name = user_name(ctx);
    let inserted: Result<Uuid, sqlx::Error> = sqlx::query_scalar(
        "INSERT INTO cmdb.import_mappings
           (name, description, class_key, definition, created_by_id, created_by_name, updated_by_id, updated_by_name)
         VALUES ($1, $2, $3, $4, $5, $6, $5, $6) RETURNING id",
    )
    .bind(input.name.trim())
    .bind(input.description.as_deref().map(str::trim).filter(|d| !d.is_empty()))
    .bind(&input.class_key)
    .bind(sqlx::types::Json(&input.definition))
    .bind(ctx_user(ctx))
    .bind(&name)
    .fetch_one(&mut *tx)
    .await;
    let id = match inserted {
        Err(e) if is_duplicate_name(&e) => return Err(duplicate_name()),
        other => other?,
    };
    let row = fetch(&mut tx, id, false).await?.ok_or_else(AppError::internal)?;
    let entry = AuditEntry {
        action: AuditAction::Create,
        entity_type: "import_mappings",
        entity_id: id,
        old_value: None,
        new_value: Some(row.audit_value()),
    };
    crud::write_audit(&mut tx, ctx, vec![entry]).await?;
    tx.commit().await?;
    row.dto()
}

/// The mapping, locked, if the caller may change it: `404` when hidden,
/// `403` unless creator or administrator, `409 VERSION_CONFLICT` when stale.
async fn for_change(conn: &mut PgConnection, ctx: &RequestContext, id: Uuid, version: i32) -> Result<Row, AppError> {
    let row = match fetch(conn, id, true).await? {
        Some(r) if r.visible(ctx) => r,
        _ => return Err(not_found(id)),
    };
    let admin = ctx.principal().is_some_and(|p| p.permissions.administrator);
    if !admin && (row.created_by_id.is_none() || row.created_by_id != ctx_user(ctx)) {
        return Err(AppError::new(
            ErrorCode::Forbidden,
            "Only the user who saved this mapping, or an administrator, can change or delete it.",
        ));
    }
    if row.version != version {
        let current = row.version;
        return Err(AppError::new(
            ErrorCode::VersionConflict,
            format!("The saved mapping was changed by someone else (you sent version {version}, current is {current}). Reload and retry."),
        )
        .with_details(vec![body_field("version", &format!("Current version is {current}"), "stale")]));
    }
    Ok(row)
}

pub async fn update(
    pool: &PgPool,
    ctx: &RequestContext,
    cfg: &ImportConfig,
    id: Uuid,
    input: &UpdateSavedImportMapping,
) -> Result<SavedImportMapping, AppError> {
    let mut tx = pool.begin().await?;
    require_enabled(&mut tx, cfg).await?;
    let before = for_change(&mut tx, ctx, id, input.version).await?;
    let description = match &input.description {
        None => before.description.clone(),
        Some(d) => d.clone().filter(|d| !d.is_empty()),
    };
    let definition = match &input.definition {
        None => before.definition.0.clone(),
        Some(d) => serde_json::to_value(d).map_err(|_| AppError::internal())?,
    };
    let updated = sqlx::query(
        "UPDATE cmdb.import_mappings SET name = $2, description = $3, definition = $4, version = version + 1,
           updated_by_id = $5, updated_by_name = $6
         WHERE id = $1",
    )
    .bind(id)
    .bind(input.name.as_deref().map(str::trim).unwrap_or(&before.name))
    .bind(description)
    .bind(sqlx::types::Json(definition))
    .bind(ctx_user(ctx))
    .bind(user_name(ctx))
    .execute(&mut *tx)
    .await;
    match updated {
        Err(e) if is_duplicate_name(&e) => return Err(duplicate_name()),
        other => other?,
    };
    let after = fetch(&mut tx, id, false).await?.ok_or_else(AppError::internal)?;
    let entry = AuditEntry {
        action: AuditAction::Update,
        entity_type: "import_mappings",
        entity_id: id,
        old_value: Some(before.audit_value()),
        new_value: Some(after.audit_value()),
    };
    crud::write_audit(&mut tx, ctx, vec![entry]).await?;
    tx.commit().await?;
    after.dto()
}

/// Deletes a saved mapping. Jobs that used it keep their own copy.
pub async fn delete(pool: &PgPool, ctx: &RequestContext, id: Uuid, version: i32) -> Result<(), AppError> {
    let mut tx = pool.begin().await?;
    let before = for_change(&mut tx, ctx, id, version).await?;
    sqlx::query("DELETE FROM cmdb.import_mappings WHERE id = $1").bind(id).execute(&mut *tx).await?;
    let entry = AuditEntry {
        action: AuditAction::Delete,
        entity_type: "import_mappings",
        entity_id: id,
        old_value: Some(before.audit_value()),
        new_value: None,
    };
    crud::write_audit(&mut tx, ctx, vec![entry]).await?;
    tx.commit().await?;
    Ok(())
}
