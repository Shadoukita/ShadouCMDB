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
    /// class key -> name (for the templates made of class layouts)
    pub class_names: HashMap<String, String>,
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
    let classes: Vec<(String, String)> =
        sqlx::query_as("SELECT key, name FROM ci_classes").fetch_all(&mut *conn).await?;
    for (k, name) in classes {
        m.classes.entry(k.clone()).or_default();
        m.class_names.insert(k, name);
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
// CIs' own layouts
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct OverrideRow {
    pub ci_id: Uuid,
    pub template_key: Option<String>,
    pub layout: Option<Value>,
    pub version: i32,
    pub updated_at: DateTime<Utc>,
    pub updated_by_name: Option<String>,
}

const OVERRIDE_COLUMNS: &str = "ci_id, template_key, layout, version, updated_at, updated_by_name";

pub async fn layout_override(
    conn: &mut PgConnection,
    ci_id: Uuid,
    for_update: bool,
) -> sqlx::Result<Option<OverrideRow>> {
    let lock = if for_update { " FOR UPDATE" } else { "" };
    sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {OVERRIDE_COLUMNS} FROM ci_layout_overrides WHERE ci_id = $1{lock}"
    )))
    .bind(ci_id)
    .fetch_optional(conn)
    .await
}

/// Creates or replaces a CI's own layout: a template key or a layout (exactly one is Some).
pub async fn put_layout_override(
    conn: &mut PgConnection,
    ctx: &RequestContext,
    ci_id: Uuid,
    template_key: Option<&str>,
    layout: Option<&Value>,
) -> sqlx::Result<OverrideRow> {
    sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "INSERT INTO ci_layout_overrides (ci_id, template_key, layout, updated_by_type, updated_by_id, updated_by_name)
         VALUES ($1, $2, $3, $4, $5, $6)
         ON CONFLICT (ci_id) DO UPDATE SET template_key = EXCLUDED.template_key, layout = EXCLUDED.layout,
           version = ci_layout_overrides.version + 1, updated_by_type = EXCLUDED.updated_by_type,
           updated_by_id = EXCLUDED.updated_by_id, updated_by_name = EXCLUDED.updated_by_name
         RETURNING {OVERRIDE_COLUMNS}"
    )))
    .bind(ci_id)
    .bind(template_key)
    .bind(layout)
    .bind(ctx.actor.actor_type.as_str())
    .bind(&ctx.actor.id)
    .bind(&ctx.actor.name)
    .fetch_one(conn)
    .await
}

pub async fn delete_layout_override(conn: &mut PgConnection, ci_id: Uuid) -> sqlx::Result<()> {
    sqlx::query("DELETE FROM ci_layout_overrides WHERE ci_id = $1").bind(ci_id).execute(conn).await?;
    Ok(())
}

/// Per template key: how many live (not deleted) CIs use it as their own layout, and their classes.
pub async fn template_use(conn: &mut PgConnection) -> sqlx::Result<HashMap<String, (i64, Vec<Uuid>)>> {
    let rows: Vec<(String, i64, Vec<Uuid>)> = sqlx::query_as(
        "SELECT o.template_key, count(*), array_agg(DISTINCT ci.class_id)
         FROM ci_layout_overrides o JOIN configuration_items ci ON ci.id = o.ci_id
         WHERE o.template_key IS NOT NULL AND ci.deleted_at IS NULL
         GROUP BY o.template_key",
    )
    .fetch_all(conn)
    .await?;
    Ok(rows.into_iter().map(|(k, n, classes)| (k, (n, classes))).collect())
}

/// class key -> (class id, live CIs of exactly that class with a layout of their own, template or custom)
pub async fn own_layout_counts(conn: &mut PgConnection) -> sqlx::Result<HashMap<String, (Uuid, i64)>> {
    let rows: Vec<(String, Uuid, i64)> = sqlx::query_as(
        "SELECT c.key, c.id, count(ci.id)
         FROM ci_classes c
         LEFT JOIN configuration_items ci ON ci.class_id = c.id AND ci.deleted_at IS NULL
           AND EXISTS (SELECT 1 FROM ci_layout_overrides o WHERE o.ci_id = ci.id)
         GROUP BY c.key, c.id",
    )
    .fetch_all(conn)
    .await?;
    Ok(rows.into_iter().map(|(k, id, n)| (k, (id, n))).collect())
}

/// template key -> a live CI in `visible` (None: any class) that shows it as its own layout: the first by
/// label, then id.
pub async fn template_samples(
    conn: &mut PgConnection,
    visible: Option<&[Uuid]>,
) -> sqlx::Result<HashMap<String, Uuid>> {
    let rows: Vec<(String, Uuid)> = sqlx::query_as(
        "SELECT DISTINCT ON (o.template_key) o.template_key, ci.id
         FROM ci_layout_overrides o JOIN configuration_items ci ON ci.id = o.ci_id
         WHERE o.template_key IS NOT NULL AND ci.deleted_at IS NULL
           AND ($1::uuid[] IS NULL OR ci.class_id = ANY($1))
         ORDER BY o.template_key, lower(ci.label), ci.id",
    )
    .bind(visible)
    .fetch_all(conn)
    .await?;
    Ok(rows.into_iter().collect())
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
