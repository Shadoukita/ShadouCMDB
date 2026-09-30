//! UI settings: one versioned, audited document that customises the web UI
//! for every user (branding, navigation, dashboard, list views, layouts), plus
//! the logo and favicon.
//!
//! Any signed-in user reads the settings (the UI applies them); changing them
//! needs `customization.manage`. The branding part and the images are public,
//! because the login page shows them before anyone has signed in.

pub mod assets;
pub mod document;
#[cfg(test)]
mod grid_tests;

use axum::extract::RawPathParams;
use axum::http::{HeaderValue, Method, StatusCode, header};
use base64::Engine;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{PgConnection, PgPool};
use utoipa::openapi::Required;
use utoipa::openapi::path::{Parameter, ParameterBuilder, ParameterIn};
use utoipa::openapi::schema::{ObjectBuilder, Schema, Type};
use utoipa::{IntoParams, ToSchema};

use self::assets::{AssetKind, ImageType};
use self::document::{Issue, UiSettingsDocument, UiTheme};
use crate::api::context::{ActorType, RequestContext};
use crate::api::route::{
    Binary, Body, Check, Either, In, Json, NoBody, NoContent, NoPath, NoQuery, PathInput, Query, Route, StatusOnly,
    WithHeaders, route,
};
use crate::api::schemas::{Page, Paged, ts};
use crate::auth::permissions::GlobalPermission;
use crate::data::crud::{self, AuditAction, AuditEntry};
use crate::data::ui_settings::{self as data, AssetMeta};
use crate::http::error::{AppError, ErrorCode, FieldError, FieldLocation};
use crate::paged;

const TAG: &str = "UI settings";
const SETTINGS_ENTITY: &str = "ui_settings";
const ASSET_ENTITY: &str = "ui_assets";
pub const DEFAULT_APP_NAME: &str = "ShadouCMDB";

// ---------------------------------------------------------------------------
// DTOs
// ---------------------------------------------------------------------------

/// An uploaded image
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UiAsset {
    #[schema(inline)]
    pub kind: AssetKind,
    pub content_type: String,
    /// Bytes
    pub size: i32,
    /// Hex SHA-256 of the file (also its ETag)
    pub sha256: String,
    /// Where to load it from; the `v` parameter changes with the content
    pub url: String,
    #[serde(serialize_with = "ts::serialize")]
    pub updated_at: DateTime<Utc>,
}

impl From<AssetMeta> for UiAsset {
    fn from(m: AssetMeta) -> Self {
        UiAsset {
            kind: AssetKind::parse(&m.kind).unwrap_or(AssetKind::Logo),
            url: format!("/api/v1/ui-settings/assets/{}?v={}", m.kind, &m.sha256[..12]),
            content_type: m.content_type,
            size: m.size,
            sha256: m.sha256,
            updated_at: m.updated_at,
        }
    }
}

/// Logo and favicon (null when not uploaded)
#[derive(Debug, Clone, Default, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UiAssets {
    #[schema(required = true)]
    pub logo: Option<UiAsset>,
    #[schema(required = true)]
    pub favicon: Option<UiAsset>,
}

impl UiAssets {
    fn from_metas(metas: Vec<AssetMeta>) -> Self {
        let mut out = UiAssets::default();
        for m in metas {
            match AssetKind::parse(&m.kind) {
                Some(AssetKind::Logo) => out.logo = Some(m.into()),
                Some(AssetKind::Favicon) => out.favicon = Some(m.into()),
                None => {}
            }
        }
        out
    }
}

/// The current UI settings as the web UI applies them
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UiSettings {
    /// Send it back in PUT; it changes with every save
    pub version: i32,
    #[serde(serialize_with = "ts::serialize")]
    pub updated_at: DateTime<Utc>,
    /// Username of whoever saved this version
    #[schema(required = true)]
    pub updated_by: Option<String>,
    /// The effective settings: references to classes, attributes and lookups that do not exist are left out (see `issues`)
    pub settings: UiSettingsDocument,
    /// What the effective settings ignore or flag, with paths into the stored document
    pub issues: Vec<Issue>,
    pub assets: UiAssets,
}

/// Branding for the login page (no sign-in needed)
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PublicBranding {
    pub app_name: String,
    #[schema(required = true)]
    pub primary_color: Option<String>,
    #[schema(required = true)]
    pub accent_color: Option<String>,
    #[schema(inline)]
    pub default_theme: UiTheme,
    #[schema(required = true)]
    pub logo: Option<UiAsset>,
    #[schema(required = true)]
    pub favicon: Option<UiAsset>,
}

fn comment_schema() -> Schema {
    crate::api::schemas::nullable_trimmed_schema(500)
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UiSettingsUpdate {
    /// The version you loaded; if someone saved in between, the request fails with 409 VERSION_CONFLICT
    #[schema(minimum = 1)]
    pub version: i32,
    pub settings: UiSettingsDocument,
    /// Note stored with the new version
    #[schema(schema_with = comment_schema)]
    #[serde(default, deserialize_with = "crate::api::schemas::trimmed_opt")]
    pub comment: Option<String>,
}

impl Check for UiSettingsUpdate {
    fn check(&self) -> Vec<FieldError> {
        self.settings.problems("settings.")
    }
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UiSettingsRestore {
    /// The current version you loaded (optimistic concurrency, as for PUT)
    #[schema(minimum = 1)]
    pub version: i32,
    #[schema(schema_with = comment_schema)]
    #[serde(default, deserialize_with = "crate::api::schemas::trimmed_opt")]
    pub comment: Option<String>,
}
impl Check for UiSettingsRestore {}

/// A logo or favicon, base64-encoded
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UiAssetUpload {
    #[schema(inline)]
    pub content_type: ImageType,
    /// The file, base64 (standard alphabet, padding optional)
    #[schema(min_length = 1, max_length = 700_000)]
    pub data: String,
}
impl Check for UiAssetUpload {}

/// A saved version (without the document)
#[derive(Debug, Serialize, ToSchema, sqlx::FromRow)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UiSettingsVersionSummary {
    pub version: i32,
    #[serde(serialize_with = "ts::serialize")]
    pub created_at: DateTime<Utc>,
    #[schema(inline)]
    pub actor_type: ActorType,
    #[schema(required = true)]
    pub actor_name: Option<String>,
    #[schema(required = true)]
    pub comment: Option<String>,
    /// The version in use now
    pub is_current: bool,
}

/// A saved version with its document as stored
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UiSettingsVersion {
    pub version: i32,
    #[serde(serialize_with = "ts::serialize")]
    pub created_at: DateTime<Utc>,
    #[schema(inline)]
    pub actor_type: ActorType,
    #[schema(required = true)]
    pub actor_name: Option<String>,
    #[schema(required = true)]
    pub comment: Option<String>,
    pub is_current: bool,
    pub settings: UiSettingsDocument,
}

#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct VersionListQuery {
    /// Page size (1-200)
    #[param(required = false, default = 20, minimum = 1, maximum = 200)]
    limit: i64,
    /// Rows to skip
    #[param(required = false, default = 0, minimum = 0, maximum = 1_000_000)]
    offset: i64,
}
paged!(VersionListQuery);

// ---------------------------------------------------------------------------
// Path parameters
// ---------------------------------------------------------------------------

fn param_error(field: &str, message: &str) -> AppError {
    AppError::validation(vec![FieldError {
        location: FieldLocation::Params,
        field: field.into(),
        message: message.into(),
        code: "invalid_format".into(),
    }])
}

fn raw<'a>(raw: &'a RawPathParams, name: &str) -> &'a str {
    raw.iter().find(|(k, _)| *k == name).map(|(_, v)| v).unwrap_or_default()
}

/// `{kind}`: logo or favicon.
pub struct AssetKindPath(pub AssetKind);

impl PathInput for AssetKindPath {
    fn params() -> Vec<Parameter> {
        vec![
            ParameterBuilder::new()
                .name("kind")
                .parameter_in(ParameterIn::Path)
                .required(Required::True)
                .schema(Some(ObjectBuilder::new().schema_type(Type::String).enum_values(Some(["logo", "favicon"]))))
                .build(),
        ]
    }
    fn parse(raw_params: &RawPathParams) -> Result<Self, AppError> {
        AssetKind::parse(raw(raw_params, "kind"))
            .map(AssetKindPath)
            .ok_or_else(|| param_error("kind", "Expected logo or favicon"))
    }
}

/// `{version}`: a positive integer.
pub struct VersionPath(pub i32);

impl PathInput for VersionPath {
    fn params() -> Vec<Parameter> {
        vec![
            ParameterBuilder::new()
                .name("version")
                .parameter_in(ParameterIn::Path)
                .required(Required::True)
                .schema(Some(ObjectBuilder::new().schema_type(Type::Integer).minimum(Some(1)).maximum(Some(i32::MAX))))
                .build(),
        ]
    }
    fn parse(raw_params: &RawPathParams) -> Result<Self, AppError> {
        raw(raw_params, "version")
            .parse::<i32>()
            .ok()
            .filter(|v| *v >= 1)
            .map(VersionPath)
            .ok_or_else(|| param_error("version", "Expected a positive integer"))
    }
}

// ---------------------------------------------------------------------------
// Service
// ---------------------------------------------------------------------------

/// A stored document. Stored documents were validated on save; one that no
/// longer parses (a future format change without a migration) falls back to
/// the defaults rather than breaking the UI.
pub fn parse_stored(v: &Value) -> UiSettingsDocument {
    serde_json::from_value(v.clone()).unwrap_or_else(|e| {
        tracing::warn!(error = %e, "stored UI settings do not match the current schema; using defaults");
        UiSettingsDocument::default()
    })
}

async fn load(conn: &mut PgConnection) -> Result<UiSettings, AppError> {
    let row = data::current(conn, false).await?;
    let model = data::model(conn).await?;
    let (settings, issues) = document::resolve(&parse_stored(&row.settings), &model);
    Ok(UiSettings {
        version: row.version,
        updated_at: row.updated_at,
        updated_by: row.updated_by_name,
        settings,
        issues,
        assets: UiAssets::from_metas(data::asset_metas(conn).await?),
    })
}

pub async fn get(pool: &PgPool) -> Result<UiSettings, AppError> {
    load(&mut *pool.acquire().await?).await
}

fn version_conflict(current: i32, by: Option<&str>) -> AppError {
    AppError::new(
        ErrorCode::VersionConflict,
        format!(
            "The UI settings were changed{} since you loaded them (now version {current}). Reload and apply your changes again.",
            by.map(|b| format!(" by {b}")).unwrap_or_default()
        ),
    )
}

/// Saves `doc` as a new version in the caller's transaction, with its audit
/// row. A document equal to the current one is not saved again. Returns
/// whether a version was written.
pub async fn save_in(
    conn: &mut PgConnection,
    ctx: &RequestContext,
    expected: Option<i32>,
    doc: &UiSettingsDocument,
    comment: Option<&str>,
) -> Result<bool, AppError> {
    let current = data::current(conn, true).await?;
    if let Some(v) = expected
        && v != current.version
    {
        return Err(version_conflict(current.version, current.updated_by_name.as_deref()));
    }
    let doc = doc.clone().normalized();
    let new_value = serde_json::to_value(&doc).map_err(|_| AppError::internal())?;
    let old_doc = parse_stored(&current.settings);
    if old_doc == doc {
        return Ok(false);
    }
    let version = data::save(conn, ctx, current.version, &new_value, comment).await?;
    let entry = AuditEntry {
        action: AuditAction::Update,
        entity_type: SETTINGS_ENTITY,
        entity_id: current.id,
        old_value: Some(json!({ "version": current.version, "settings": current.settings })),
        new_value: Some(json!({ "version": version, "settings": new_value, "comment": comment })),
    };
    crud::write_audit(conn, ctx, vec![entry]).await?;
    Ok(true)
}

pub async fn update(pool: &PgPool, ctx: &RequestContext, body: &UiSettingsUpdate) -> Result<UiSettings, AppError> {
    let mut tx = pool.begin().await?;
    save_in(&mut tx, ctx, Some(body.version), &body.settings, body.comment.as_deref()).await?;
    let out = load(&mut tx).await?;
    tx.commit().await?;
    Ok(out)
}

pub async fn restore(
    pool: &PgPool,
    ctx: &RequestContext,
    version: i32,
    body: &UiSettingsRestore,
) -> Result<UiSettings, AppError> {
    let old = data::version(pool, version).await?.ok_or_else(|| AppError::missing("UI settings version", version))?;
    // A version saved before an upgrade that renamed fields (e.g. migration 0016) would come back as
    // the defaults; refuse it instead of silently discarding the administrator's settings.
    let doc: UiSettingsDocument = serde_json::from_value(old.settings.clone()).map_err(|_| {
        AppError::conflict(format!(
            "Version {version} was saved before an upgrade changed the settings format and cannot be restored; \
             the upgrade saved a converted copy of the settings current at the time as a newer version"
        ))
    })?;
    let comment = body.comment.clone().unwrap_or_else(|| format!("Restored version {version}"));
    let mut tx = pool.begin().await?;
    save_in(&mut tx, ctx, Some(body.version), &doc, Some(&comment)).await?;
    let out = load(&mut tx).await?;
    tx.commit().await?;
    Ok(out)
}

const SUMMARY_COLUMNS: &str =
    "version, created_at, actor_type, actor_name, comment, version = (SELECT version FROM ui_settings) AS is_current";

pub async fn list_versions(pool: &PgPool, q: &VersionListQuery) -> Result<Page<UiSettingsVersionSummary>, AppError> {
    let (rows, total) = crud::select_page::<UiSettingsVersionSummary>(
        &mut *pool.acquire().await?,
        "ui_settings_versions",
        SUMMARY_COLUMNS,
        &|_| {},
        "version DESC",
        q.limit,
        q.offset,
    )
    .await?;
    Ok(Page { data: rows, page: q.page_meta(total) })
}

fn actor_type(s: &str) -> ActorType {
    match s {
        "user" => ActorType::User,
        "api_client" => ActorType::ApiClient,
        "import" => ActorType::Import,
        _ => ActorType::System,
    }
}

pub async fn get_version(pool: &PgPool, version: i32) -> Result<UiSettingsVersion, AppError> {
    let row = data::version(pool, version).await?.ok_or_else(|| AppError::missing("UI settings version", version))?;
    let current = data::current(&mut *pool.acquire().await?, false).await?.version;
    Ok(UiSettingsVersion {
        version: row.version,
        created_at: row.created_at,
        actor_type: actor_type(&row.actor_type),
        actor_name: row.actor_name,
        comment: row.comment,
        is_current: row.version == current,
        settings: parse_stored(&row.settings),
    })
}

pub async fn branding(pool: &PgPool) -> Result<PublicBranding, AppError> {
    let mut conn = pool.acquire().await?;
    let row = data::current(&mut conn, false).await?;
    let b = parse_stored(&row.settings).branding;
    let assets = UiAssets::from_metas(data::asset_metas(&mut conn).await?);
    Ok(PublicBranding {
        app_name: b.app_name.unwrap_or_else(|| DEFAULT_APP_NAME.into()),
        primary_color: b.primary_color,
        accent_color: b.accent_color,
        default_theme: b.default_theme,
        logo: assets.logo,
        favicon: assets.favicon,
    })
}

pub fn sha256_hex(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

/// Decodes and checks an upload: (type, bytes).
pub fn decode_upload(kind: AssetKind, content_type: ImageType, b64: &str) -> Result<Vec<u8>, Vec<FieldError>> {
    let compact: String = b64.chars().filter(|c| !c.is_ascii_whitespace()).collect();
    let bytes =
        base64::engine::general_purpose::STANDARD_NO_PAD.decode(compact.trim_end_matches('=')).map_err(|_| {
            vec![FieldError {
                location: FieldLocation::Body,
                field: "data".into(),
                message: "Not valid base64".into(),
                code: "invalid_format".into(),
            }]
        })?;
    assets::check(kind, content_type, &bytes).map_err(|(field, code, message)| {
        vec![FieldError { location: FieldLocation::Body, field: field.into(), message, code: code.into() }]
    })?;
    Ok(bytes)
}

fn asset_json(m: &AssetMeta) -> Value {
    crud::json(&UiAsset::from(m.clone()))
}

/// Stores an image in the caller's transaction, with its audit row.
pub async fn put_asset_in(
    conn: &mut PgConnection,
    ctx: &RequestContext,
    kind: AssetKind,
    content_type: ImageType,
    bytes: &[u8],
) -> Result<UiAsset, AppError> {
    let before = data::asset_meta(conn, kind.as_str(), true).await?;
    let meta = data::put_asset(conn, kind.as_str(), content_type.as_str(), bytes, &sha256_hex(bytes)).await?;
    let entry = AuditEntry {
        action: if before.is_some() { AuditAction::Update } else { AuditAction::Create },
        entity_type: ASSET_ENTITY,
        entity_id: meta.id,
        old_value: before.as_ref().map(asset_json),
        new_value: Some(asset_json(&meta)),
    };
    crud::write_audit(conn, ctx, vec![entry]).await?;
    Ok(meta.into())
}

/// Removes an image in the caller's transaction; false when there was none.
pub async fn delete_asset_in(conn: &mut PgConnection, ctx: &RequestContext, kind: AssetKind) -> Result<bool, AppError> {
    let Some(before) = data::asset_meta(conn, kind.as_str(), true).await? else { return Ok(false) };
    data::delete_asset(conn, kind.as_str()).await?;
    let entry = AuditEntry {
        action: AuditAction::Delete,
        entity_type: ASSET_ENTITY,
        entity_id: before.id,
        old_value: Some(asset_json(&before)),
        new_value: None,
    };
    crud::write_audit(conn, ctx, vec![entry]).await?;
    Ok(true)
}

pub async fn upload(
    pool: &PgPool,
    ctx: &RequestContext,
    kind: AssetKind,
    b: &UiAssetUpload,
) -> Result<UiAsset, AppError> {
    let bytes = decode_upload(kind, b.content_type, &b.data).map_err(AppError::validation)?;
    let mut tx = pool.begin().await?;
    let out = put_asset_in(&mut tx, ctx, kind, b.content_type, &bytes).await?;
    tx.commit().await?;
    Ok(out)
}

pub async fn remove_asset(pool: &PgPool, ctx: &RequestContext, kind: AssetKind) -> Result<(), AppError> {
    let mut tx = pool.begin().await?;
    if !delete_asset_in(&mut tx, ctx, kind).await? {
        return Err(AppError::not_found(format!("No {} has been uploaded", kind.as_str())));
    }
    tx.commit().await?;
    Ok(())
}

/// Served with a sandboxing CSP and nosniff so an SVG can never run script on this origin.
const ASSET_CSP: &str = "default-src 'none'; style-src 'unsafe-inline'; sandbox";

type AssetResponse = Either<WithHeaders<Binary>, WithHeaders<StatusOnly>>;

pub async fn serve_asset(
    pool: &PgPool,
    kind: AssetKind,
    headers: &axum::http::HeaderMap,
) -> Result<AssetResponse, AppError> {
    let (content_type, bytes, sha) = data::asset_data(pool, kind.as_str())
        .await?
        .ok_or_else(|| AppError::not_found(format!("No {} has been uploaded", kind.as_str())))?;
    let etag = format!("\"{sha}\"");
    let common = |etag: &str| {
        vec![
            (header::ETAG, HeaderValue::from_str(etag).unwrap_or(HeaderValue::from_static("\"\""))),
            // No Cache-Control here: `http::security_headers` sets `no-store` on every API path and
            // would overwrite it. A new upload shows at once either way (the URL's ?v= also changes).
            (header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff")),
            (header::CONTENT_SECURITY_POLICY, HeaderValue::from_static(ASSET_CSP)),
        ]
    };
    let not_modified = headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.split(',').any(|t| t.trim() == etag || t.trim() == "*"));
    if not_modified {
        return Ok(Either::Right(WithHeaders(StatusOnly(StatusCode::NOT_MODIFIED), common(&etag))));
    }
    let content_type = HeaderValue::from_str(&content_type).map_err(|_| AppError::internal())?;
    Ok(Either::Left(WithHeaders(Binary { content_type, body: bytes }, common(&etag))))
}

// ---------------------------------------------------------------------------
// Routes
// ---------------------------------------------------------------------------

pub fn routes() -> Vec<Route> {
    vec![
        route(Method::GET, "/api/v1/ui-settings", "getUiSettings")
            .tag(TAG)
            .summary("The UI settings every user sees")
            .description(
                "Any signed-in user may read them; the web UI applies them for everyone. `settings` is the effective \
                 document: entries that refer to classes, attributes, statuses, environments or locations that do not \
                 exist are left out and listed in `issues` (they stay in the stored document, see the versions).",
            )
            .handle(|api, In(NoPath, NoQuery, NoBody): In<NoPath, NoQuery, NoBody>| async move {
                Ok(Json(get(&api.pool).await?))
            }),
        route(Method::PUT, "/api/v1/ui-settings", "updateUiSettings")
            .tag(TAG)
            .summary("Replace the UI settings (saved as a new version)")
            .description(
                "Replaces the whole document. Send the `version` you loaded: if the settings were saved in between, \
                 the request fails with 409 VERSION_CONFLICT. The document is validated against its schema and the \
                 cross-field rules (400 with per-field details); references to classes or attributes that do not \
                 exist are accepted and reported in `issues`. Saving an unchanged document does not create a version. \
                 Each saved version is kept and audited.",
            )
            .requires(GlobalPermission::CustomizationManage)
            .errors(&[ErrorCode::VersionConflict])
            .handle(|api, In(NoPath, NoQuery, Body(b)): In<NoPath, NoQuery, Body<UiSettingsUpdate>>| async move {
                Ok(Json(update(&api.pool, &api.ctx, &b).await?))
            }),
        route(Method::GET, "/api/v1/ui-settings/branding", "getPublicBranding")
            .tag(TAG)
            .summary("App name, colours, theme, logo and favicon for the login page")
            .description("Public: no session needed.")
            .public()
            .handle(|api, In(NoPath, NoQuery, NoBody): In<NoPath, NoQuery, NoBody>| async move {
                Ok(Json(branding(&api.pool).await?))
            }),
        route(Method::GET, "/api/v1/ui-settings/versions", "listUiSettingsVersions")
            .tag(TAG)
            .summary("Saved versions of the UI settings, newest first")
            .requires(GlobalPermission::CustomizationManage)
            .handle(
                |api, In(NoPath, Query(q), NoBody): In<NoPath, Query<VersionListQuery>, NoBody>| async move {
                    Ok(Json(list_versions(&api.pool, &q).await?))
                },
            ),
        route(Method::GET, "/api/v1/ui-settings/versions/{version}", "getUiSettingsVersion")
            .tag(TAG)
            .summary("One saved version, with its document as stored")
            .requires(GlobalPermission::CustomizationManage)
            .errors(&[ErrorCode::NotFound])
            .handle(|api, In(VersionPath(v), NoQuery, NoBody): In<VersionPath, NoQuery, NoBody>| async move {
                Ok(Json(get_version(&api.pool, v).await?))
            }),
        route(Method::POST, "/api/v1/ui-settings/versions/{version}/restore", "restoreUiSettingsVersion")
            .tag(TAG)
            .summary("Make an earlier version current again (saved as a new version)")
            .description(
                "409 CONFLICT for a version saved before an upgrade changed the settings format (e.g. migration 0016); the upgrade saved a converted copy as a newer version.",
            )
            .requires(GlobalPermission::CustomizationManage)
            .errors(&[ErrorCode::NotFound, ErrorCode::VersionConflict, ErrorCode::Conflict])
            .handle(
                |api, In(VersionPath(v), NoQuery, Body(b)): In<VersionPath, NoQuery, Body<UiSettingsRestore>>| async move {
                    Ok(Json(restore(&api.pool, &api.ctx, v, &b).await?))
                },
            ),
        route(Method::GET, "/api/v1/ui-settings/assets/{kind}", "getUiAsset")
            .tag(TAG)
            .summary("The logo or favicon image")
            .description(
                "Public (the login page shows it). Answers with the image bytes, an ETag and `Cache-Control: no-store` \
                 (like every API response); send `If-None-Match` to get 304 Not Modified. Served with a sandboxing Content-Security-Policy.",
            )
            .public()
            .errors(&[ErrorCode::NotFound])
            .also_returns(StatusCode::NOT_MODIFIED, "Not modified (If-None-Match matched the ETag); no body")
            .handle(|api, In(AssetKindPath(kind), NoQuery, NoBody): In<AssetKindPath, NoQuery, NoBody>| async move {
                serve_asset(&api.pool, kind, &api.headers).await
            }),
        route(Method::PUT, "/api/v1/ui-settings/assets/{kind}", "uploadUiAsset")
            .tag(TAG)
            .summary("Upload or replace the logo or favicon")
            .description(
                "JSON body with the content type and the base64 file. Logo: PNG, JPEG, WebP or SVG up to 512 KiB. \
                 Favicon: PNG, ICO or SVG up to 128 KiB. The content must match the declared type. SVGs must be \
                 well-formed and use only allowlisted drawing elements and attributes: no scripts, event handlers, \
                 animation, links, foreign content, DTD subsets or processing instructions, and references only \
                 within the file (`#id`) or to embedded PNG, JPEG, GIF or WebP data. Anything else is refused with \
                 `unsafe_content`.",
            )
            .requires(GlobalPermission::CustomizationManage)
            .handle(
                |api, In(AssetKindPath(kind), NoQuery, Body(b)): In<AssetKindPath, NoQuery, Body<UiAssetUpload>>| async move {
                    Ok(Json(upload(&api.pool, &api.ctx, kind, &b).await?))
                },
            ),
        route(Method::DELETE, "/api/v1/ui-settings/assets/{kind}", "deleteUiAsset")
            .tag(TAG)
            .summary("Remove the logo or favicon (the UI falls back to the built-in one)")
            .requires(GlobalPermission::CustomizationManage)
            .errors(&[ErrorCode::NotFound])
            .handle(|api, In(AssetKindPath(kind), NoQuery, NoBody): In<AssetKindPath, NoQuery, NoBody>| async move {
                remove_asset(&api.pool, &api.ctx, kind).await?;
                Ok(NoContent)
            }),
    ]
}

#[cfg(test)]
mod tests {
    use axum::http::header;
    use serde_json::json;

    use crate::db::scratch;
    use crate::modules::api_tokens::tests::{Creds, app, call, code};

    /// Section kinds through the real router: saved, returned, validated (SHAA-299).
    #[tokio::test]
    async fn layouts_place_notes_and_built_in_panels() {
        let Some(db) = scratch::database("layouts_place_notes_and_built_in_panels").await else { return };
        let app = app(db.pool.clone());
        let body = json!({ "username": "owner", "displayName": "Owner", "password": "correct horse battery", "setupToken": crate::auth::setup_token::TEST_TOKEN });
        let (status, me, headers) = call(&app, "POST", "/api/v1/setup", &Creds::default(), Some(body)).await;
        assert_eq!(status, 201, "{me}");
        let cookie = headers
            .get_all(header::SET_COOKIE)
            .iter()
            .map(|v| v.to_str().unwrap().split(';').next().unwrap().to_owned())
            .collect::<Vec<_>>()
            .join("; ");
        let s = Creds { cookie: Some(cookie), csrf: me["csrfToken"].as_str().map(str::to_owned), bearer: None };
        let (status, v, _) =
            call(&app, "POST", "/api/v1/ci-classes", &s, Some(json!({ "key": "server", "name": "Server" }))).await;
        assert_eq!(status, 201, "{v}");
        let (_, current, _) = call(&app, "GET", "/api/v1/ui-settings", &s, None).await;
        let version = current["version"].as_i64().unwrap();

        let layout = json!({ "classKey": "server", "tabs": [
            { "key": "main", "label": "Main", "sections": [
                { "key": "hint", "label": "Read me", "kind": "note", "text": "Patch window: *Sunday* 02:00." },
                { "key": "core", "label": "Core", "fields": [{ "field": "ident", "width": 1 }] } ] },
            { "key": "context", "label": "Context", "sections": [
                { "key": "rel", "label": "Relationships", "kind": "relations" },
                { "key": "log", "label": "Audit trail", "kind": "audit", "collapsed": true } ] } ] });
        let put = |settings| json!({ "version": version, "settings": settings });

        // Each built-in panel once per layout: the later placement is the one reported.
        let mut twice = layout.clone();
        twice["tabs"][0]["sections"][1] = json!({ "key": "rel2", "label": "Again", "kind": "relations" });
        let (status, v, _) =
            call(&app, "PUT", "/api/v1/ui-settings", &s, Some(put(json!({ "layouts": [twice] })))).await;
        assert_eq!((status, code(&v)), (400, "VALIDATION_ERROR"), "{v}");
        assert_eq!(v["error"]["details"][0]["field"], "settings.layouts.0.tabs.1.sections.0.kind", "{v}");
        // Note text is limited.
        let mut long = layout.clone();
        long["tabs"][0]["sections"][0]["text"] = json!("x".repeat(super::document::NOTE_MAX_CHARS + 1));
        let (status, v, _) =
            call(&app, "PUT", "/api/v1/ui-settings", &s, Some(put(json!({ "layouts": [long] })))).await;
        assert_eq!(status, 400, "{v}");
        assert_eq!(v["error"]["details"][0]["field"], "settings.layouts.0.tabs.0.sections.0.text", "{v}");
        // Unknown kinds are refused by the schema.
        let mut html = layout.clone();
        html["tabs"][0]["sections"][0]["kind"] = json!("html");
        let (status, _, _) =
            call(&app, "PUT", "/api/v1/ui-settings", &s, Some(put(json!({ "layouts": [html] })))).await;
        assert_eq!(status, 400);
        // The deprecated v1 `panels` are write-only, not secret: their labels are one line (SHAA-765).
        for label in ["Ops\nTeam", "\u{202E}evil"] {
            let v1 = json!({ "classKey": "server", "panels": [{ "key": "p", "label": label, "fields": [] }] });
            let (status, v, _) =
                call(&app, "PUT", "/api/v1/ui-settings", &s, Some(put(json!({ "layouts": [v1] })))).await;
            assert_eq!(status, 400, "{label:?}: {v}");
            assert_eq!(v["error"]["details"][0]["field"], "settings.layouts.0.panels.0.label", "{v}");
            assert_eq!(v["error"]["details"][0]["code"], "invalid_character", "{v}");
        }
        // Secrets stay exempt: a tab in a login password is a wrong password, not a 400.
        let wrong = json!({ "username": "owner", "password": "correct\thorse battery" });
        let (status, v, _) = call(&app, "POST", "/api/v1/auth/login", &Creds::default(), Some(wrong)).await;
        assert_eq!(status, 401, "{v}");

        let (status, saved, _) =
            call(&app, "PUT", "/api/v1/ui-settings", &s, Some(put(json!({ "layouts": [layout.clone()] })))).await;
        assert_eq!(status, 200, "{saved}");
        assert_eq!(saved["issues"], json!([]), "{saved}");
        let (_, got, _) = call(&app, "GET", "/api/v1/ui-settings", &s, None).await;
        let tabs = &got["settings"]["layouts"][0]["tabs"];
        assert_eq!(tabs[0]["sections"][0]["kind"], "note");
        assert_eq!(tabs[0]["sections"][0]["text"], "Patch window: *Sunday* 02:00.");
        assert!(tabs[0]["sections"][1].get("kind").is_none(), "field sections stay as before: {got}");
        assert_eq!(tabs[1]["sections"][0]["kind"], "relations");
        assert_eq!(
            (tabs[1]["sections"][1]["kind"].as_str(), tabs[1]["sections"][1]["collapsed"].as_bool()),
            (Some("audit"), Some(true))
        );

        db.drop().await;
    }
}
