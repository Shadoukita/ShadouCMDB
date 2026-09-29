//! SQL for relationships: the joined row (edge + type + both endpoints),
//! filtered lists, and the writes.

use chrono::{DateTime, Utc};
use sqlx::{AssertSqlSafe, PgConnection};
use uuid::Uuid;

use super::crud::{self, Where};

/// An edge with its type and both endpoints (and their classes).
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct RelationshipRow {
    pub id: Uuid,
    pub relationship_type_id: Uuid,
    pub source_ci_id: Uuid,
    pub target_ci_id: Uuid,
    pub notes: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub deleted_at: Option<DateTime<Utc>>,
    pub type_key: String,
    pub type_name: String,
    pub forward_label: String,
    pub reverse_label: String,
    pub is_directional: bool,
    pub source_name: String,
    pub source_class_id: Uuid,
    pub source_class_key: String,
    pub source_class_name: String,
    pub source_deleted_at: Option<DateTime<Utc>>,
    pub target_name: String,
    pub target_class_id: Uuid,
    pub target_class_key: String,
    pub target_class_name: String,
    pub target_deleted_at: Option<DateTime<Utc>>,
}

const COLUMNS: &str = "r.id, r.relationship_type_id, r.source_ci_id, r.target_ci_id, r.notes,
    r.created_at, r.updated_at, r.deleted_at,
    t.key AS type_key, t.name AS type_name, t.forward_label, t.reverse_label, t.is_directional,
    s.label AS source_name, s.class_id AS source_class_id, sc.key AS source_class_key, sc.name AS source_class_name, s.deleted_at AS source_deleted_at,
    g.label AS target_name, g.class_id AS target_class_id, gc.key AS target_class_key, gc.name AS target_class_name, g.deleted_at AS target_deleted_at";

const FROM: &str = "ci_relationships r
    JOIN relationship_types t ON t.id = r.relationship_type_id
    JOIN configuration_items s ON s.id = r.source_ci_id JOIN ci_classes sc ON sc.id = s.class_id
    JOIN configuration_items g ON g.id = r.target_ci_id JOIN ci_classes gc ON gc.id = g.class_id";

pub const SORT_FIELDS: &[&str] = &["createdAt", "updatedAt", "sourceName", "targetName", "typeName"];

fn sort_column(field: &str) -> &'static str {
    match field {
        "updatedAt" => "r.updated_at",
        "sourceName" => "lower(s.label)",
        "targetName" => "lower(g.label)",
        "typeName" => "lower(t.name)",
        _ => "r.created_at",
    }
}

pub async fn list(
    conn: &mut PgConnection,
    filter: crud::Filter<'_>,
    sort_field: &str,
    sort_dir: &str,
    limit: i64,
    offset: i64,
) -> sqlx::Result<(Vec<RelationshipRow>, i64)> {
    let order = format!("{} {sort_dir}, r.id ASC", sort_column(sort_field));
    crud::select_page(conn, FROM, COLUMNS, filter, &order, limit, offset).await
}

pub async fn get(conn: &mut PgConnection, id: Uuid) -> sqlx::Result<Option<RelationshipRow>> {
    sqlx::query_as(AssertSqlSafe(format!("SELECT {COLUMNS} FROM {FROM} WHERE r.id = $1")))
        .bind(id)
        .fetch_optional(conn)
        .await
}

/// Search predicate: source/target CI name, type name and notes.
pub fn search(w: &mut Where<'_>, pattern: &str) {
    let qb = w.and();
    qb.push("(s.label ILIKE ").push_bind(pattern.to_owned());
    qb.push(" OR g.label ILIKE ").push_bind(pattern.to_owned());
    qb.push(" OR r.notes ILIKE ").push_bind(pattern.to_owned());
    qb.push(" OR t.name ILIKE ").push_bind(pattern.to_owned()).push(")");
}

/// Of the given CI ids, the ones that exist (deleted or not), with their class.
pub async fn existing_items(conn: &mut PgConnection, ids: &[Uuid]) -> sqlx::Result<Vec<(Uuid, Uuid)>> {
    sqlx::query_as("SELECT id, class_id FROM configuration_items WHERE id = ANY($1)").bind(ids).fetch_all(conn).await
}

pub async fn insert(
    conn: &mut PgConnection,
    type_id: Uuid,
    source: Uuid,
    target: Uuid,
    notes: Option<&str>,
) -> sqlx::Result<Uuid> {
    sqlx::query_scalar!(
        "INSERT INTO ci_relationships (relationship_type_id, source_ci_id, target_ci_id, notes)
         VALUES ($1, $2, $3, $4) RETURNING id",
        type_id,
        source,
        target,
        notes
    )
    .fetch_one(conn)
    .await
}

/// Locks the edge; Some(deleted_at) when it exists.
pub async fn lock(conn: &mut PgConnection, id: Uuid) -> sqlx::Result<Option<Option<DateTime<Utc>>>> {
    sqlx::query_scalar!("SELECT deleted_at FROM ci_relationships WHERE id = $1 FOR UPDATE", id)
        .fetch_optional(conn)
        .await
}

/// PATCH: `type_id` None keeps the type; `notes` None keeps them, Some(None) clears them.
pub async fn update(
    conn: &mut PgConnection,
    id: Uuid,
    type_id: Option<Uuid>,
    notes: Option<Option<&str>>,
) -> sqlx::Result<()> {
    sqlx::query!(
        "UPDATE ci_relationships
         SET relationship_type_id = COALESCE($2, relationship_type_id),
             notes = CASE WHEN $3 THEN $4 ELSE notes END
         WHERE id = $1",
        id,
        type_id,
        notes.is_some(),
        notes.flatten()
    )
    .execute(conn)
    .await?;
    Ok(())
}

pub async fn soft_delete(conn: &mut PgConnection, id: Uuid) -> sqlx::Result<()> {
    sqlx::query!("UPDATE ci_relationships SET deleted_at = now() WHERE id = $1", id).execute(conn).await?;
    Ok(())
}
