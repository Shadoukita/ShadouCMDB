//! CI notes: storage, access, policy, retention and audit (SHAA-2355).
//!
//! Access follows the CI: the notes of a CI are read with view on its class
//! (a CI the caller may not view answers `404`, like a missing one) and
//! written with edit on it. A note is changed or deleted only by its author,
//! within the edit window of the policy; an administrator may delete any note
//! at any time (removing personal data on request), but never changes one.
//! Every write is audited in the same transaction as an `create`, `update` or
//! `delete` of `ci_notes` whose values name the CI (`ciId`), so the audit log
//! shows it only to callers who may view that CI.

use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{PgConnection, PgPool};
use tokio::sync::watch;
use tokio::task::JoinHandle;
use utoipa::openapi::schema::{ObjectBuilder, Schema, Type};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::api::context::RequestContext;
use crate::api::route::Check;
use crate::api::schemas::{self, NOT_BLANK_PATTERN, Page, Paged};
use crate::auth::permissions::ClassOp;
use crate::data::crud::{self, AuditAction, AuditEntry};
use crate::http::error::{AppError, ErrorCode, FieldError, FieldLocation};
use crate::paged;

/// Longest note, in characters.
pub const MAX_BODY: usize = 10_000;
/// Default edit window (24 hours), as migration 0070 sets it.
pub const DEFAULT_EDIT_WINDOW_MINUTES: i32 = 1440;

pub const ENTITY: &str = "ci_notes";
pub const SETTINGS_ENTITY: &str = "ci_note_settings";
/// The audit entity id of the one settings row (it has no uuid of its own).
pub const SETTINGS_ENTITY_ID: Uuid = Uuid::nil();

// ---------------------------------------------------------------------------
// API shapes
// ---------------------------------------------------------------------------

/// The author of a note; the name stays when the account is deleted
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CiNoteAuthor {
    /// Null once the user was deleted
    pub id: Option<Uuid>,
    pub name: String,
}

/// A note on a configuration item
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CiNote {
    pub id: Uuid,
    pub ci_id: Uuid,
    /// Plain text; line breaks are kept. Render it as text, never as HTML or Markdown
    pub body: String,
    pub author: CiNoteAuthor,
    pub created_at: DateTime<Utc>,
    /// When the author last changed the text; null if never
    pub edited_at: Option<DateTime<Utc>>,
    /// Send it back with changes and deletes (optimistic concurrency)
    pub version: i32,
    /// Until when the author may change or delete the note under the current policy; null when the policy sets no
    /// time limit
    pub editable_until: Option<DateTime<Utc>>,
    /// Whether the caller may change the text: the author, with edit on the CI's class, inside the edit window,
    /// while the CI is not deleted
    pub can_edit: bool,
    /// Whether the caller may delete the note: as for `canEdit` (the CI may be deleted), or an administrator at any
    /// time
    pub can_delete: bool,
}

#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct ListCiNotesQuery {
    /// Page size (1-200)
    #[param(required = false, default = 50, minimum = 1, maximum = 200)]
    limit: i64,
    /// Rows to skip
    #[param(required = false, default = 0, minimum = 0, maximum = 1_000_000)]
    offset: i64,
}
paged!(ListCiNotesQuery);

#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct DeleteCiNoteQuery {
    /// The version you loaded
    #[param(minimum = 1)]
    pub version: i32,
}

fn body_schema() -> Schema {
    ObjectBuilder::new()
        .schema_type(Type::String)
        .min_length(Some(1))
        .max_length(Some(MAX_BODY))
        .pattern(Some(NOT_BLANK_PATTERN))
        .extensions(Some(schemas::multiline_extension()))
        .description(Some("Plain text, up to 10,000 characters; leading and trailing white space is removed"))
        .into()
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateCiNote {
    #[schema(schema_with = body_schema)]
    #[serde(deserialize_with = "schemas::trimmed")]
    pub body: String,
}

impl Check for CreateCiNote {}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateCiNote {
    /// The version you loaded; if the note changed in between, `409 VERSION_CONFLICT`
    #[schema(minimum = 1)]
    pub version: i32,
    #[schema(schema_with = body_schema)]
    #[serde(deserialize_with = "schemas::trimmed")]
    pub body: String,
}

impl Check for UpdateCiNote {}

/// The note policy of the instance
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CiNoteSettings {
    /// How long after posting the author may change or delete their note, in minutes; null: no limit, 0: never
    #[schema(required = true)]
    pub edit_window_minutes: Option<i32>,
    /// Notes older than this many days are deleted by the server (checked hourly); null: kept until deleted
    #[schema(required = true)]
    pub retention_days: Option<i32>,
    pub updated_at: DateTime<Utc>,
    /// Who last changed the policy; null for the installed defaults
    pub updated_by: Option<String>,
}

/// Replaces the note policy
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateCiNoteSettings {
    /// Minutes (0 to 525600, one year); null: no limit, 0: authors cannot change or delete notes
    #[schema(required = true, minimum = 0, maximum = 525_600)]
    pub edit_window_minutes: Option<i32>,
    /// Days (30 to 36500); null: keep notes until they are deleted
    #[schema(required = true, minimum = 30, maximum = 36_500)]
    pub retention_days: Option<i32>,
}

impl Check for UpdateCiNoteSettings {}

// ---------------------------------------------------------------------------
// Policy
// ---------------------------------------------------------------------------

#[derive(Debug, sqlx::FromRow)]
struct SettingsRow {
    edit_window_minutes: Option<i32>,
    retention_days: Option<i32>,
    updated_at: DateTime<Utc>,
    updated_by_name: Option<String>,
}

impl SettingsRow {
    fn dto(self) -> CiNoteSettings {
        CiNoteSettings {
            edit_window_minutes: self.edit_window_minutes,
            retention_days: self.retention_days,
            updated_at: self.updated_at,
            updated_by: self.updated_by_name,
        }
    }

    fn audit_value(&self) -> Value {
        json!({ "editWindowMinutes": self.edit_window_minutes, "retentionDays": self.retention_days })
    }
}

/// The policy row. Migration 0070 creates it and the API role may not delete
/// it; should it be missing anyway, the installed defaults apply.
async fn settings_row(conn: &mut PgConnection, lock: bool) -> sqlx::Result<SettingsRow> {
    let sql = format!(
        "SELECT edit_window_minutes, retention_days, updated_at, updated_by_name FROM cmdb.ci_note_settings{}",
        if lock { " FOR UPDATE" } else { "" }
    );
    let row: Option<SettingsRow> = sqlx::query_as(sqlx::AssertSqlSafe(sql)).fetch_optional(conn).await?;
    Ok(row.unwrap_or(SettingsRow {
        edit_window_minutes: Some(DEFAULT_EDIT_WINDOW_MINUTES),
        retention_days: None,
        updated_at: DateTime::<Utc>::UNIX_EPOCH,
        updated_by_name: None,
    }))
}

pub async fn get_settings(pool: &PgPool) -> Result<CiNoteSettings, AppError> {
    Ok(settings_row(&mut *pool.acquire().await?, false).await?.dto())
}

/// Administrator only. A change is audited as an `update` of `ci_note_settings`.
pub async fn update_settings(
    pool: &PgPool,
    ctx: &RequestContext,
    input: &UpdateCiNoteSettings,
) -> Result<CiNoteSettings, AppError> {
    ctx.require_administrator("change the note policy")?;
    let mut tx = pool.begin().await?;
    let before = settings_row(&mut tx, true).await?;
    if before.edit_window_minutes != input.edit_window_minutes || before.retention_days != input.retention_days {
        let user = ctx.principal().map(|p| (p.user_id, p.username.clone()));
        sqlx::query(
            "INSERT INTO cmdb.ci_note_settings (id, edit_window_minutes, retention_days, updated_by_id, updated_by_name)
             VALUES (true, $1, $2, $3, $4)
             ON CONFLICT (id) DO UPDATE SET edit_window_minutes = EXCLUDED.edit_window_minutes,
               retention_days = EXCLUDED.retention_days, updated_by_id = EXCLUDED.updated_by_id,
               updated_by_name = EXCLUDED.updated_by_name",
        )
        .bind(input.edit_window_minutes)
        .bind(input.retention_days)
        .bind(user.as_ref().map(|u| u.0))
        .bind(user.as_ref().map(|u| u.1.as_str()).or(ctx.actor.name.as_deref()))
        .execute(&mut *tx)
        .await?;
        let after = settings_row(&mut tx, false).await?;
        let entry = AuditEntry {
            action: AuditAction::Update,
            entity_type: SETTINGS_ENTITY,
            entity_id: SETTINGS_ENTITY_ID,
            old_value: Some(before.audit_value()),
            new_value: Some(after.audit_value()),
        };
        crud::write_audit(&mut tx, ctx, vec![entry]).await?;
    }
    let out = settings_row(&mut tx, false).await?.dto();
    tx.commit().await?;
    Ok(out)
}

// ---------------------------------------------------------------------------
// Rows
// ---------------------------------------------------------------------------

#[derive(Debug, sqlx::FromRow)]
struct Row {
    id: Uuid,
    ci_id: Uuid,
    body: String,
    author_id: Option<Uuid>,
    author_name: String,
    created_at: DateTime<Utc>,
    edited_at: Option<DateTime<Utc>>,
    version: i32,
    /// Null when the window is unlimited.
    editable_until: Option<DateTime<Utc>>,
    /// Inside the edit window at the database's `now()`.
    in_window: bool,
}

/// `$1`: the edit window in minutes (null: no limit).
const SELECT: &str = "SELECT n.id, n.ci_id, n.body, n.author_id, n.author_name, n.created_at, n.edited_at, n.version, \
    n.created_at + make_interval(mins => $1::int) AS editable_until, \
    ($1::int IS NULL OR now() < n.created_at + make_interval(mins => $1::int)) AS in_window \
    FROM cmdb.ci_notes n";

impl Row {
    fn audit_value(&self) -> Value {
        json!({ "ciId": self.ci_id, "body": self.body, "authorId": self.author_id, "authorName": self.author_name,
                "createdAt": self.created_at, "editedAt": self.edited_at, "version": self.version })
    }
}

/// The CI a note stream belongs to.
#[derive(Debug, sqlx::FromRow)]
struct Ci {
    class_id: Uuid,
    deleted_at: Option<DateTime<Utc>>,
}

/// The caller as the author of a note.
fn me(ctx: &RequestContext) -> Result<(Uuid, String), AppError> {
    ctx.principal()
        .map(|p| (p.user_id, p.username.clone()))
        .ok_or_else(|| AppError::new(ErrorCode::Unauthenticated, "Sign in first"))
}

fn not_found(id: Uuid) -> AppError {
    AppError::missing("Note", id)
}

/// The CI, after the view check: `404` when missing or not viewable. Writes
/// take `FOR KEY SHARE`, so the CI row cannot go while the note is written
/// but CI edits are not held up.
async fn ci(conn: &mut PgConnection, ctx: &RequestContext, id: Uuid, lock: bool) -> Result<Ci, AppError> {
    let sql = format!(
        "SELECT class_id, deleted_at FROM cmdb.configuration_items WHERE id = $1{}",
        if lock { " FOR KEY SHARE" } else { "" }
    );
    let row: Option<Ci> = sqlx::query_as(sqlx::AssertSqlSafe(sql)).bind(id).fetch_optional(&mut *conn).await?;
    let row = row.ok_or_else(|| AppError::missing("Configuration item", id))?;
    ctx.require_class_visible(row.class_id, "Configuration item", id)?;
    Ok(row)
}

/// The CI for a new or changed note: live, with edit on its class. A deleted
/// CI answers `404`, as the CI endpoints do for a change.
async fn live_editable_ci(conn: &mut PgConnection, ctx: &RequestContext, id: Uuid) -> Result<Ci, AppError> {
    let ci = ci(conn, ctx, id, true).await?;
    if ci.deleted_at.is_some() {
        return Err(AppError::missing("Configuration item", id));
    }
    ctx.require_class(ci.class_id, ClassOp::Edit)?;
    Ok(ci)
}

async fn fetch(
    conn: &mut PgConnection,
    window: Option<i32>,
    ci: Uuid,
    id: Uuid,
    lock: bool,
) -> sqlx::Result<Option<Row>> {
    let sql = format!("{SELECT} WHERE n.id = $2 AND n.ci_id = $3{}", if lock { " FOR UPDATE OF n" } else { "" });
    sqlx::query_as(sqlx::AssertSqlSafe(sql)).bind(window).bind(id).bind(ci).fetch_optional(conn).await
}

fn is_administrator(ctx: &RequestContext) -> bool {
    ctx.require_administrator("").is_ok()
}

/// Whether the caller may change or delete the note as its author: inside the
/// edit window, with edit on the CI's class.
fn author_may(row: &Row, ctx: &RequestContext, ci: &Ci) -> bool {
    let author = ctx.principal().is_some_and(|p| row.author_id == Some(p.user_id));
    author && row.in_window && ctx.require_class(ci.class_id, ClassOp::Edit).is_ok()
}

fn dto(row: Row, ctx: &RequestContext, ci: &Ci) -> CiNote {
    let author_may = author_may(&row, ctx, ci);
    CiNote {
        id: row.id,
        ci_id: row.ci_id,
        author: CiNoteAuthor { id: row.author_id, name: row.author_name },
        body: row.body,
        created_at: row.created_at,
        edited_at: row.edited_at,
        version: row.version,
        editable_until: row.editable_until,
        can_edit: author_may && ci.deleted_at.is_none(),
        can_delete: author_may || is_administrator(ctx),
    }
}

fn version_conflict(sent: i32, row: &Row) -> AppError {
    let current = row.version;
    AppError::new(
        ErrorCode::VersionConflict,
        format!(
            "The note was changed in the meantime (you sent version {sent}, current is {current}). Reload and retry."
        ),
    )
    .with_details(vec![FieldError {
        location: FieldLocation::Body,
        field: "version".into(),
        message: format!("Current version is {current}"),
        code: "stale".into(),
    }])
}

/// Why the author may not change the note, as `403` with a detail code.
fn refused(row: &Row, ctx: &RequestContext, ci: &Ci, what: &str) -> AppError {
    let author = ctx.principal().is_some_and(|p| row.author_id == Some(p.user_id));
    let (message, code) = if !author {
        (format!("Only the author of a note can {what} it."), "not_author")
    } else if !row.in_window {
        (format!("The time to {what} this note has passed (see the note policy)."), "edit_window_closed")
    } else {
        // Without edit on the class; the caller is told by require_class.
        return ctx.require_class(ci.class_id, ClassOp::Edit).err().unwrap_or_else(AppError::internal);
    };
    AppError::new(ErrorCode::Forbidden, message.clone()).with_details(vec![FieldError {
        location: FieldLocation::Params,
        field: "noteId".into(),
        message,
        code: code.into(),
    }])
}

// ---------------------------------------------------------------------------
// Endpoints
// ---------------------------------------------------------------------------

/// The notes of a CI, newest first. Deleted CIs keep theirs, readable.
pub async fn list(
    pool: &PgPool,
    ctx: &RequestContext,
    ci_id: Uuid,
    q: &ListCiNotesQuery,
) -> Result<Page<CiNote>, AppError> {
    let mut conn = pool.acquire().await?;
    let ci = ci(&mut conn, ctx, ci_id, false).await?;
    let window = settings_row(&mut conn, false).await?.edit_window_minutes;
    let rows: Vec<Row> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "{SELECT} WHERE n.ci_id = $2 ORDER BY n.created_at DESC, n.id DESC LIMIT $3 OFFSET $4"
    )))
    .bind(window)
    .bind(ci_id)
    .bind(q.limit)
    .bind(q.offset)
    .fetch_all(&mut *conn)
    .await?;
    let total: i64 = sqlx::query_scalar("SELECT count(*) FROM cmdb.ci_notes WHERE ci_id = $1")
        .bind(ci_id)
        .fetch_one(&mut *conn)
        .await?;
    let data = rows.into_iter().map(|r| dto(r, ctx, &ci)).collect();
    Ok(Page { data, page: q.page_meta(total) })
}

pub async fn create(
    pool: &PgPool,
    ctx: &RequestContext,
    ci_id: Uuid,
    input: &CreateCiNote,
) -> Result<CiNote, AppError> {
    let (me, username) = me(ctx)?;
    let mut tx = pool.begin().await?;
    let ci = live_editable_ci(&mut tx, ctx, ci_id).await?;
    let window = settings_row(&mut tx, false).await?.edit_window_minutes;
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO cmdb.ci_notes (ci_id, body, author_id, author_name) VALUES ($1, $2, $3, $4) RETURNING id",
    )
    .bind(ci_id)
    .bind(&input.body)
    .bind(me)
    .bind(&username)
    .fetch_one(&mut *tx)
    .await?;
    let row = fetch(&mut tx, window, ci_id, id, false).await?.ok_or_else(AppError::internal)?;
    let entry = AuditEntry {
        action: AuditAction::Create,
        entity_type: ENTITY,
        entity_id: id,
        old_value: None,
        new_value: Some(row.audit_value()),
    };
    crud::write_audit(&mut tx, ctx, vec![entry]).await?;
    let out = dto(row, ctx, &ci);
    tx.commit().await?;
    Ok(out)
}

pub async fn update(
    pool: &PgPool,
    ctx: &RequestContext,
    ci_id: Uuid,
    id: Uuid,
    input: &UpdateCiNote,
) -> Result<CiNote, AppError> {
    let mut tx = pool.begin().await?;
    let ci = live_editable_ci(&mut tx, ctx, ci_id).await?;
    let window = settings_row(&mut tx, false).await?.edit_window_minutes;
    let before = fetch(&mut tx, window, ci_id, id, true).await?.ok_or_else(|| not_found(id))?;
    if !author_may(&before, ctx, &ci) {
        return Err(refused(&before, ctx, &ci, "change"));
    }
    if before.version != input.version {
        return Err(version_conflict(input.version, &before));
    }
    if before.body == input.body {
        let out = dto(before, ctx, &ci);
        tx.commit().await?;
        return Ok(out);
    }
    sqlx::query("UPDATE cmdb.ci_notes SET body = $2, edited_at = now(), version = version + 1 WHERE id = $1")
        .bind(id)
        .bind(&input.body)
        .execute(&mut *tx)
        .await?;
    let after = fetch(&mut tx, window, ci_id, id, false).await?.ok_or_else(AppError::internal)?;
    let entry = AuditEntry {
        action: AuditAction::Update,
        entity_type: ENTITY,
        entity_id: id,
        old_value: Some(before.audit_value()),
        new_value: Some(after.audit_value()),
    };
    crud::write_audit(&mut tx, ctx, vec![entry]).await?;
    let out = dto(after, ctx, &ci);
    tx.commit().await?;
    Ok(out)
}

pub async fn delete(pool: &PgPool, ctx: &RequestContext, ci_id: Uuid, id: Uuid, version: i32) -> Result<(), AppError> {
    let mut tx = pool.begin().await?;
    let ci = ci(&mut tx, ctx, ci_id, true).await?;
    let window = settings_row(&mut tx, false).await?.edit_window_minutes;
    let before = fetch(&mut tx, window, ci_id, id, true).await?.ok_or_else(|| not_found(id))?;
    if !author_may(&before, ctx, &ci) && !is_administrator(ctx) {
        return Err(refused(&before, ctx, &ci, "delete"));
    }
    if before.version != version {
        return Err(version_conflict(version, &before));
    }
    sqlx::query("DELETE FROM cmdb.ci_notes WHERE id = $1").bind(id).execute(&mut *tx).await?;
    let entry = AuditEntry {
        action: AuditAction::Delete,
        entity_type: ENTITY,
        entity_id: id,
        old_value: Some(before.audit_value()),
        new_value: None,
    };
    crud::write_audit(&mut tx, ctx, vec![entry]).await?;
    tx.commit().await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Retention
// ---------------------------------------------------------------------------

/// Notes deleted per transaction by the sweep.
const SWEEP_BATCH: i64 = 500;
/// How often the server applies the retention period.
const SWEEP_EVERY: Duration = Duration::from_secs(3600);

/// A note removed by the sweep: id, CI, author id and name, creation time.
type SweptNote = (Uuid, Uuid, Option<Uuid>, String, DateTime<Utc>);

/// Deletes the notes older than the retention period, `shift` added to the
/// database's `now()` (tests move the clock forward). Each note is audited as
/// a `delete` of `ci_notes` by the system actor `note retention`, with its CI,
/// author and time but without its text: removing the text is the point.
/// Several servers may sweep at once; a note is deleted, and audited, once.
pub async fn sweep(pool: &PgPool, shift: chrono::Duration) -> sqlx::Result<u64> {
    let shift = sqlx::postgres::types::PgInterval::try_from(shift).unwrap_or_default();
    let ctx = RequestContext::system("note retention", format!("retention:{}", Uuid::new_v4()));
    let mut total = 0;
    loop {
        let mut tx = pool.begin().await?;
        let Some(days) = settings_row(&mut tx, false).await?.retention_days else { return Ok(total) };
        let gone: Vec<SweptNote> = sqlx::query_as(
            "DELETE FROM cmdb.ci_notes WHERE id IN (
               SELECT id FROM cmdb.ci_notes WHERE created_at < now() + $1 - make_interval(days => $2)
               ORDER BY created_at LIMIT $3 FOR UPDATE SKIP LOCKED)
             RETURNING id, ci_id, author_id, author_name, created_at",
        )
        .bind(shift)
        .bind(days)
        .bind(SWEEP_BATCH)
        .fetch_all(&mut *tx)
        .await?;
        let n = gone.len() as u64;
        let entries = gone
            .into_iter()
            .map(|(id, ci_id, author_id, author_name, created_at)| AuditEntry {
                action: AuditAction::Delete,
                entity_type: ENTITY,
                entity_id: id,
                old_value: Some(json!({ "ciId": ci_id, "authorId": author_id, "authorName": author_name,
                                        "createdAt": created_at, "retentionDays": days })),
                new_value: None,
            })
            .collect();
        crud::write_audit(&mut tx, &ctx, entries).await?;
        tx.commit().await?;
        total += n;
        if n < SWEEP_BATCH as u64 {
            return Ok(total);
        }
    }
}

/// The retention sweep of this process.
pub struct Retention {
    stop: watch::Sender<bool>,
    task: JoinHandle<()>,
}

impl Retention {
    pub async fn stop(self) {
        let _ = self.stop.send(true);
        if tokio::time::timeout(Duration::from_secs(10), self.task).await.is_err() {
            tracing::warn!("the note retention sweep did not stop within 10 s");
        }
    }
}

/// Applies the retention period soon after start, then every hour.
pub fn spawn_retention(pool: PgPool) -> Retention {
    let (stop, mut rx) = watch::channel(false);
    let task = tokio::spawn(async move {
        let mut wait = Duration::from_secs(60);
        loop {
            tokio::select! {
                _ = tokio::time::sleep(wait) => {}
                _ = rx.changed() => return,
            }
            wait = SWEEP_EVERY;
            match sweep(&pool, chrono::Duration::zero()).await {
                Ok(0) => {}
                Ok(n) => tracing::info!(deleted = n, "note retention: deleted notes older than the retention period"),
                Err(e) => tracing::warn!(error = %e, "note retention sweep failed; retried in an hour"),
            }
        }
    });
    Retention { stop, task }
}
