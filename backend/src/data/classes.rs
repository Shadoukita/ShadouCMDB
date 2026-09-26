//! Class-hierarchy queries. They lean on the ci_class_lineage() /
//! ci_class_is_a() SQL functions from migration 0002 so the database and the
//! API agree on what "inherits" means.

use chrono::{DateTime, Utc};
use serde_json::{Map, Value};
use sqlx::PgConnection;
use sqlx::types::Json;
use uuid::Uuid;

use crate::modules::classes::AttributeDataType;

/// An attribute definition of a class or one of its ancestors.
#[derive(Debug, Clone)]
pub struct EffectiveAttributeRow {
    pub id: Uuid,
    pub class_id: Uuid,
    pub key: String,
    pub label: String,
    pub description: Option<String>,
    pub data_type: AttributeDataType,
    pub is_required: bool,
    pub enum_values: Option<Json<Vec<String>>>,
    pub reference_class_id: Option<Uuid>,
    pub lookup_list_id: Option<Uuid>,
    pub validation: Option<Json<Map<String, Value>>>,
    pub group_name: Option<String>,
    pub help_text: Option<String>,
    pub default_value: Option<Json<Value>>,
    pub sort_order: i32,
    pub is_active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// 0 = defined on the class itself.
    pub depth: i32,
    pub defined_on_key: String,
    pub defined_on_name: String,
}

pub async fn class_exists(conn: &mut PgConnection, id: Uuid) -> sqlx::Result<bool> {
    Ok(sqlx::query_scalar!("SELECT 1 AS \"one!\" FROM ci_classes WHERE id = $1", id)
        .fetch_optional(conn)
        .await?
        .is_some())
}

/// Attribute definitions of a class and all its ancestors (ancestors first, then sort order).
pub async fn effective_attributes(conn: &mut PgConnection, class_id: Uuid) -> sqlx::Result<Vec<EffectiveAttributeRow>> {
    sqlx::query_as!(
        EffectiveAttributeRow,
        r#"SELECT d.id, d.class_id, d.key, d.label, d.description,
                  d.data_type AS "data_type: AttributeDataType", d.is_required,
                  d.enum_values AS "enum_values: Json<Vec<String>>", d.reference_class_id, d.lookup_list_id,
                  d.validation AS "validation: Json<Map<String, Value>>", d.group_name, d.help_text,
                  d.default_value AS "default_value: Json<Value>", d.sort_order,
                  d.is_active, d.created_at, d.updated_at,
                  l.depth AS "depth!", c.key AS defined_on_key, c.name AS defined_on_name
           FROM ci_class_lineage($1) l
           JOIN ci_attribute_definitions d ON d.class_id = l.class_id
           JOIN ci_classes c ON c.id = d.class_id
           ORDER BY l.depth DESC, d.sort_order, d.key"#,
        class_id
    )
    .fetch_all(conn)
    .await
}

/// Another definition with the same key on an ancestor or descendant would shadow it.
pub async fn attribute_key_clash(
    conn: &mut PgConnection,
    class_id: Uuid,
    key: &str,
    except_id: Uuid,
) -> sqlx::Result<Option<String>> {
    sqlx::query_scalar!(
        "SELECT c.key
         FROM ci_attribute_definitions d JOIN ci_classes c ON c.id = d.class_id
         WHERE d.key = $2 AND d.id <> $3
           AND (ci_class_is_a($1, d.class_id) OR ci_class_is_a(d.class_id, $1))
         LIMIT 1",
        class_id,
        key,
        except_id
    )
    .fetch_optional(conn)
    .await
}

/// `Some(is_active)` when the value belongs to the list, `None` otherwise.
pub async fn lookup_value_state(conn: &mut PgConnection, list_id: Uuid, value_id: Uuid) -> sqlx::Result<Option<bool>> {
    sqlx::query_scalar!("SELECT is_active FROM lookup_list_values WHERE id = $1 AND list_id = $2", value_id, list_id)
        .fetch_optional(conn)
        .await
}

/// Live CIs whose class is exactly this one.
pub async fn class_has_items(conn: &mut PgConnection, class_id: Uuid) -> sqlx::Result<bool> {
    Ok(sqlx::query_scalar!(
        "SELECT 1 AS \"one!\" FROM configuration_items WHERE class_id = $1 AND deleted_at IS NULL LIMIT 1",
        class_id
    )
    .fetch_optional(conn)
    .await?
    .is_some())
}

/// What the CI service needs to know about a class (None when it does not exist).
pub struct ClassInfo {
    pub key: String,
}

pub async fn class_info(conn: &mut PgConnection, class_id: Uuid) -> sqlx::Result<Option<ClassInfo>> {
    sqlx::query_as!(ClassInfo, "SELECT key FROM ci_classes WHERE id = $1", class_id).fetch_optional(conn).await
}

/// Class ids plus every descendant class, so filtering by "hardware" finds servers too.
pub async fn with_descendant_classes(conn: &mut PgConnection, class_ids: &[Uuid]) -> sqlx::Result<Vec<Uuid>> {
    let rows = sqlx::query_scalar!(
        "WITH RECURSIVE down AS (
           SELECT id, 0 AS depth FROM ci_classes WHERE id = ANY($1::uuid[])
           UNION
           SELECT c.id, down.depth + 1 FROM ci_classes c JOIN down ON c.parent_id = down.id WHERE down.depth < 64
         )
         SELECT DISTINCT id FROM down",
        class_ids
    )
    .fetch_all(conn)
    .await?;
    Ok(rows.into_iter().flatten().collect())
}
