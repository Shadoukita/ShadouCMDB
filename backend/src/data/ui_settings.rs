//! SQL for the UI settings document, its version history and the branding assets.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use crate::api::context::RequestContext;

/// What a UI settings document may refer to: every class with its effective attributes, and lookup lists.
#[derive(Debug, Default)]
pub struct Model {
    /// class key -> effective attribute key -> required (and active)
    pub classes: HashMap<String, HashMap<String, bool>>,
    /// lookup list key -> its value keys
    pub lookups: HashMap<String, HashSet<String>>,
}

#[derive(Debug, sqlx::FromRow)]
pub struct CurrentRow {
    pub id: Uuid,
    pub version: i32,
    pub settings: Value,
    pub updated_at: DateTime<Utc>,
    pub updated_by_name: Option<String>,
}

pub async fn current(conn: &mut PgConnection, for_update: bool) -> sqlx::Result<CurrentRow> {
    let sql = if for_update {
        "SELECT id, version, settings, updated_at, updated_by_name FROM ui_settings FOR UPDATE"
    } else {
        "SELECT id, version, settings, updated_at, updated_by_name FROM ui_settings"
    };
    sqlx::query_as(sql).fetch_one(conn).await
}

/// Stores `settings` as the next version and makes it current. Returns the new version.
pub async fn save(
    conn: &mut PgConnection,
    ctx: &RequestContext,
    previous: i32,
    settings: &Value,
    comment: Option<&str>,
) -> sqlx::Result<i32> {
    let version = previous + 1;
    sqlx::query(
        "INSERT INTO ui_settings_versions (version, settings, actor_type, actor_id, actor_name, comment)
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(version)
    .bind(settings)
    .bind(ctx.actor.actor_type.as_str())
    .bind(&ctx.actor.id)
    .bind(&ctx.actor.name)
    .bind(comment)
    .execute(&mut *conn)
    .await?;
    sqlx::query(
        "UPDATE ui_settings SET version = $1, settings = $2, updated_at = now(), updated_by_id = $3, updated_by_name = $4",
    )
    .bind(version)
    .bind(settings)
    .bind(&ctx.actor.id)
    .bind(&ctx.actor.name)
    .execute(&mut *conn)
    .await?;
    Ok(version)
}

#[derive(Debug, sqlx::FromRow)]
pub struct VersionRow {
    pub version: i32,
    pub settings: Value,
    pub created_at: DateTime<Utc>,
    pub actor_type: String,
    pub actor_name: Option<String>,
    pub comment: Option<String>,
}

pub async fn version(pool: &PgPool, version: i32) -> sqlx::Result<Option<VersionRow>> {
    sqlx::query_as("SELECT version, settings, created_at, actor_type, actor_name, comment FROM ui_settings_versions WHERE version = $1")
        .bind(version)
        .fetch_optional(pool)
        .await
}

/// Every class key with its effective attributes (own and inherited), and the lookup lists with their value keys.
pub async fn model(conn: &mut PgConnection) -> sqlx::Result<Model> {
    let mut m = Model::default();
    let classes: Vec<(String,)> = sqlx::query_as("SELECT key FROM ci_classes").fetch_all(&mut *conn).await?;
    for (k,) in classes {
        m.classes.entry(k).or_default();
    }
    let attrs: Vec<(String, String, bool)> = sqlx::query_as(
        "WITH RECURSIVE lineage (class_id, ancestor_id) AS (
           SELECT id, id FROM ci_classes
           UNION ALL
           SELECT l.class_id, c.parent_id FROM lineage l JOIN ci_classes c ON c.id = l.ancestor_id
           WHERE c.parent_id IS NOT NULL
         )
         SELECT c.key, a.key, a.is_required AND a.is_active
         FROM lineage l
         JOIN ci_classes c ON c.id = l.class_id
         JOIN ci_attribute_definitions a ON a.class_id = l.ancestor_id",
    )
    .fetch_all(&mut *conn)
    .await?;
    for (class, attr, required) in attrs {
        m.classes.entry(class).or_default().insert(attr, required);
    }
    let lists: Vec<(String, Option<String>)> =
        sqlx::query_as("SELECT l.key, v.key FROM lookup_lists l LEFT JOIN lookup_list_values v ON v.list_id = l.id")
            .fetch_all(&mut *conn)
            .await?;
    for (list, value) in lists {
        let values = m.lookups.entry(list).or_default();
        values.extend(value);
    }
    Ok(m)
}

// ---------------------------------------------------------------------------
// Assets
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct AssetMeta {
    pub id: Uuid,
    pub kind: String,
    pub content_type: String,
    pub size: i32,
    pub sha256: String,
    pub updated_at: DateTime<Utc>,
}

const ASSET_META: &str = "id, kind, content_type, octet_length(data) AS size, sha256, updated_at";

pub async fn asset_metas(conn: &mut PgConnection) -> sqlx::Result<Vec<AssetMeta>> {
    sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT {ASSET_META} FROM ui_assets ORDER BY kind")))
        .fetch_all(conn)
        .await
}

pub async fn asset_meta(conn: &mut PgConnection, kind: &str, for_update: bool) -> sqlx::Result<Option<AssetMeta>> {
    let lock = if for_update { " FOR UPDATE" } else { "" };
    sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT {ASSET_META} FROM ui_assets WHERE kind = $1{lock}")))
        .bind(kind)
        .fetch_optional(conn)
        .await
}

/// (content type, bytes, sha256)
pub async fn asset_data(pool: &PgPool, kind: &str) -> sqlx::Result<Option<(String, Vec<u8>, String)>> {
    sqlx::query_as("SELECT content_type, data, sha256 FROM ui_assets WHERE kind = $1")
        .bind(kind)
        .fetch_optional(pool)
        .await
}

pub async fn all_asset_data(conn: &mut PgConnection) -> sqlx::Result<Vec<(String, String, Vec<u8>)>> {
    sqlx::query_as("SELECT kind, content_type, data FROM ui_assets ORDER BY kind").fetch_all(conn).await
}

pub async fn put_asset(
    conn: &mut PgConnection,
    kind: &str,
    content_type: &str,
    data: &[u8],
    sha256: &str,
) -> sqlx::Result<AssetMeta> {
    sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "INSERT INTO ui_assets (kind, content_type, data, sha256) VALUES ($1, $2, $3, $4)
         ON CONFLICT (kind) DO UPDATE SET content_type = EXCLUDED.content_type, data = EXCLUDED.data, sha256 = EXCLUDED.sha256
         RETURNING {ASSET_META}"
    )))
    .bind(kind)
    .bind(content_type)
    .bind(data)
    .bind(sha256)
    .fetch_one(conn)
    .await
}

pub async fn delete_asset(conn: &mut PgConnection, kind: &str) -> sqlx::Result<()> {
    sqlx::query("DELETE FROM ui_assets WHERE kind = $1").bind(kind).execute(conn).await?;
    Ok(())
}
