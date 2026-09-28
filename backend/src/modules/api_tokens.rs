//! Administration > API tokens: personal and service tokens for scripts and integrations.
//!
//! A token acts for its owner (a user; for a service, a dedicated account) and
//! is scoped to one permission profile: it may do what both the owner and the
//! profile allow (see [`crate::auth::token`]). It has a mandatory expiry and
//! can be revoked. The secret is in the create response only; the database
//! keeps its SHA-256.
//!
//! Managing tokens needs `users.manage` and a signed-in session (a token cannot
//! mint or revoke tokens). As for accounts, a non-administrator can only
//! create or revoke tokens of users whose permissions they hold themselves.

use axum::http::{Method, StatusCode};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{PgConnection, PgPool};
use utoipa::openapi::schema::{KnownFormat, ObjectBuilder, Schema, SchemaFormat, Type};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use super::users::{ProfileRef, must_cover_user};
use crate::api::context::RequestContext;
use crate::api::route::{Body, Check, IdPath, In, Json, NoBody, NoContent, NoPath, NoQuery, Query, Route, route};
use crate::api::schemas::{self, Page, Paged, Sort, UuidList, like_pattern, trimmed, ts, ts_opt};
use crate::auth::events::TOKEN_ENTITY;
use crate::auth::permissions::GlobalPermission;
use crate::auth::{session, token};
use crate::data::api_tokens::{self as data, TokenRow};
use crate::data::auth as auth_data;
use crate::data::crud::{self, AuditAction, AuditEntry, Where};
use crate::http::error::{AppError, ErrorCode};
use crate::paged;

/// Longest lifetime a token can be given.
pub const MAX_LIFETIME_DAYS: i64 = 366;

// ---------------------------------------------------------------------------
// Schemas
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum TokenStatus {
    /// Usable (while its owner is active and its profile exists)
    Active,
    Expired,
    Revoked,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ApiToken {
    pub id: Uuid,
    pub name: String,
    /// The owner: the token acts as this user, within its profile
    pub user_id: Uuid,
    pub username: String,
    /// A disabled owner's tokens are refused
    pub owner_is_active: bool,
    /// The scope; null once the profile was deleted (the token is then refused)
    #[schema(required = true)]
    pub profile: Option<ProfileRef>,
    /// The first characters of the secret, to recognise it; never the secret
    pub token_prefix: String,
    #[schema(inline)]
    pub status: TokenStatus,
    #[serde(serialize_with = "ts::serialize")]
    pub expires_at: DateTime<Utc>,
    #[serde(serialize_with = "ts_opt::serialize")]
    #[schema(required = true)]
    pub revoked_at: Option<DateTime<Utc>>,
    #[schema(required = true)]
    pub revoked_by: Option<String>,
    #[serde(serialize_with = "ts_opt::serialize")]
    #[schema(required = true)]
    pub last_used_at: Option<DateTime<Utc>>,
    /// Client address of the last accepted request (evidence only)
    #[schema(required = true)]
    pub last_used_ip: Option<String>,
    #[schema(required = true)]
    pub created_by: Option<String>,
    #[serde(serialize_with = "ts::serialize")]
    pub created_at: DateTime<Utc>,
}

impl From<TokenRow> for ApiToken {
    fn from(r: TokenRow) -> Self {
        let status = match (r.revoked_at.is_some(), r.expired) {
            (true, _) => TokenStatus::Revoked,
            (false, true) => TokenStatus::Expired,
            (false, false) => TokenStatus::Active,
        };
        let profile = match (r.profile_id, r.profile_name) {
            (Some(id), Some(name)) => Some(ProfileRef { id, name, is_builtin: r.profile_is_builtin.unwrap_or(false) }),
            _ => None,
        };
        ApiToken {
            id: r.id,
            name: r.name,
            user_id: r.user_id,
            username: r.username,
            owner_is_active: r.user_active,
            profile,
            token_prefix: r.token_prefix,
            status,
            expires_at: r.expires_at,
            revoked_at: r.revoked_at,
            revoked_by: r.revoked_by,
            last_used_at: r.last_used_at,
            last_used_ip: r.last_used_ip.map(|n| n.ip().to_string()),
            created_by: r.created_by,
            created_at: r.created_at,
        }
    }
}

/// A new token and its secret. The secret is shown here only: store it now.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreatedApiToken {
    pub token: ApiToken,
    /// Send as `Authorization: Bearer <secret>`. Not retrievable later.
    pub secret: String,
}

fn token_name_schema() -> Schema {
    ObjectBuilder::new()
        .schema_type(Type::String)
        .min_length(Some(1))
        .max_length(Some(200))
        .pattern(Some(r"\S"))
        .description(Some("What the token is for, e.g. \"backup script\""))
        .into()
}

fn expires_at_schema() -> Schema {
    ObjectBuilder::new()
        .schema_type(Type::String)
        .format(Some(SchemaFormat::KnownFormat(KnownFormat::DateTime)))
        .description(Some("In the future, at most 366 days from now (ISO 8601)"))
        .into()
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ApiTokenCreate {
    #[schema(schema_with = token_name_schema)]
    #[serde(deserialize_with = "trimmed")]
    pub name: String,
    /// The owner; default: yourself. Use a dedicated user for a service token.
    #[schema(nullable = false)]
    pub user_id: Option<Uuid>,
    /// The scope: the token may do what both this profile and its owner allow
    pub profile_id: Uuid,
    #[schema(schema_with = expires_at_schema)]
    pub expires_at: DateTime<Utc>,
}

impl Check for ApiTokenCreate {}

fn sort_schema() -> Schema {
    schemas::sort_schema(&["createdAt", "name", "expiresAt", "lastUsedAt"], "-createdAt")
}

#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct ApiTokenList {
    /// Page size (1-200)
    #[param(required = false, default = 50, minimum = 1, maximum = 200)]
    limit: i64,
    /// Rows to skip
    #[param(required = false, default = 0, minimum = 0, maximum = 1_000_000)]
    offset: i64,
    /// Matches the token name, its prefix and the owner's username
    #[param(schema_with = schemas::search_schema)]
    q: Option<String>,
    #[param(required = false, schema_with = sort_schema)]
    sort: Sort,
    /// Tokens owned by any of these users
    #[param(schema_with = schemas::uuid_list_schema)]
    user_id: Option<UuidList>,
    #[param(inline)]
    status: Option<TokenStatus>,
}
paged!(ApiTokenList);

// ---------------------------------------------------------------------------
// Service
// ---------------------------------------------------------------------------

async fn load(conn: &mut PgConnection, id: Uuid, for_update: bool) -> Result<ApiToken, AppError> {
    let row = data::get(conn, id, for_update).await?.ok_or_else(|| AppError::missing("API token", id))?;
    Ok(row.into())
}

pub async fn list(pool: &PgPool, q: &ApiTokenList) -> Result<Page<ApiToken>, AppError> {
    let filter = |w: &mut Where<'_>| {
        if let Some(text) = &q.q {
            let p = like_pattern(text);
            w.and()
                .push("(t.name ILIKE ")
                .push_bind(p.clone())
                .push(" OR t.token_prefix ILIKE ")
                .push_bind(p.clone())
                .push(" OR u.username ILIKE ")
                .push_bind(p)
                .push(")");
        }
        if let Some(ids) = &q.user_id {
            w.and().push("t.user_id = ANY(").push_bind(ids.0.clone()).push(")");
        }
        match q.status {
            Some(TokenStatus::Active) => w.and_sql("t.revoked_at IS NULL AND t.expires_at > now()"),
            Some(TokenStatus::Expired) => w.and_sql("t.revoked_at IS NULL AND t.expires_at <= now()"),
            Some(TokenStatus::Revoked) => w.and_sql("t.revoked_at IS NOT NULL"),
            None => {}
        }
    };
    let column = match q.sort.field.as_str() {
        "name" => "lower(t.name)",
        "expiresAt" => "t.expires_at",
        "lastUsedAt" => "t.last_used_at",
        _ => "t.created_at",
    };
    let order = format!("{column} {} NULLS LAST, t.id", q.sort.dir());
    let (rows, total) =
        crud::select_page::<TokenRow>(pool, data::FROM, data::COLUMNS, &filter, &order, q.limit, q.offset).await?;
    Ok(Page { data: rows.into_iter().map(ApiToken::from).collect(), page: q.page_meta(total) })
}

pub async fn get(pool: &PgPool, id: Uuid) -> Result<ApiToken, AppError> {
    load(&mut *pool.acquire().await?, id, false).await
}

pub async fn create(pool: &PgPool, ctx: &RequestContext, b: &ApiTokenCreate) -> Result<CreatedApiToken, AppError> {
    let now = Utc::now();
    if b.expires_at <= now {
        return Err(AppError::field("expiresAt", "Must be in the future", "out_of_range"));
    }
    if b.expires_at > now + Duration::days(MAX_LIFETIME_DAYS) {
        let message = format!("At most {MAX_LIFETIME_DAYS} days from now");
        return Err(AppError::field("expiresAt", message, "out_of_range"));
    }
    let me = ctx.principal().map(|p| p.user_id);
    let Some(owner_id) = b.user_id.or(me) else {
        return Err(AppError::field("userId", "Required", "required"));
    };

    let mut tx = pool.begin().await?;
    let owner = auth_data::get_user(&mut tx, owner_id, false).await?;
    let Some(owner) = owner else {
        return Err(AppError::field("userId", "User does not exist", "not_found"));
    };
    if !owner.is_active {
        return Err(AppError::field("userId", "User is disabled", "inactive"));
    }
    if Some(owner_id) != me {
        must_cover_user(&mut tx, ctx, owner_id).await?;
    }
    if auth_data::existing_profiles(&mut tx, &[b.profile_id]).await?.is_empty() {
        return Err(AppError::field("profileId", "Permission profile does not exist", "not_found"));
    }

    let secret = token::new_secret();
    let prefix = token::shown_prefix(&secret);
    let id = data::insert(
        &mut tx,
        &data::NewToken {
            name: &b.name,
            user_id: owner_id,
            profile_id: b.profile_id,
            token_hash: &session::token_hash(&secret),
            token_prefix: &prefix,
            expires_at: b.expires_at,
            created_by: ctx.actor.name.as_deref(),
        },
    )
    .await?;
    let dto = load(&mut tx, id, false).await?;
    let entry = AuditEntry {
        action: AuditAction::Create,
        entity_type: TOKEN_ENTITY,
        entity_id: id,
        old_value: None,
        new_value: Some(crud::json(&dto)),
    };
    crud::write_audit(&mut tx, ctx, vec![entry]).await?;
    tx.commit().await?;
    tracing::info!(token = %prefix, owner = %owner.username, "API token created");
    Ok(CreatedApiToken { token: dto, secret })
}

/// Revokes the token; revoking it again changes nothing.
pub async fn revoke(pool: &PgPool, ctx: &RequestContext, id: Uuid) -> Result<(), AppError> {
    let mut tx = pool.begin().await?;
    let before = load(&mut tx, id, true).await?;
    if ctx.principal().is_none_or(|me| me.user_id != before.user_id) {
        must_cover_user(&mut tx, ctx, before.user_id).await?;
    }
    if before.revoked_at.is_some() {
        return Ok(());
    }
    let by = ctx.actor.name.clone().unwrap_or_else(|| ctx.actor.actor_type.as_str().to_owned());
    data::revoke(&mut tx, id, &by).await?;
    let dto = load(&mut tx, id, false).await?;
    let entry = AuditEntry {
        action: AuditAction::Update,
        entity_type: TOKEN_ENTITY,
        entity_id: id,
        old_value: Some(crud::json(&before)),
        new_value: Some(crud::json(&dto)),
    };
    crud::write_audit(&mut tx, ctx, vec![entry]).await?;
    tx.commit().await?;
    tracing::info!(token = %dto.token_prefix, "API token revoked");
    Ok(())
}

/// Audits the deletion of a user's tokens, in the transaction that deletes the user.
pub async fn audit_deleted_with_owner(
    conn: &mut PgConnection,
    ctx: &RequestContext,
    user_id: Uuid,
) -> Result<(), AppError> {
    let entries = data::of_user(conn, user_id)
        .await?
        .into_iter()
        .map(|row| {
            let dto = ApiToken::from(row);
            AuditEntry {
                action: AuditAction::Delete,
                entity_type: TOKEN_ENTITY,
                entity_id: dto.id,
                old_value: Some(crud::json(&dto)),
                new_value: None,
            }
        })
        .collect();
    crud::write_audit(conn, ctx, entries).await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Routes
// ---------------------------------------------------------------------------

const TAG: &str = "API tokens";
const BASE: &str = "/api/v1/admin/api-tokens";
const BY_ID: &str = "/api/v1/admin/api-tokens/{id}";

pub fn routes() -> Vec<Route> {
    let manage = GlobalPermission::UsersManage;
    vec![
        route(Method::GET, BASE, "listApiTokens")
            .tag(TAG)
            .summary("List API tokens (paginated, searchable, filterable by owner and status); never their secrets")
            .requires(manage)
            .session_only()
            .handle(|api, In(NoPath, Query(q), NoBody): In<NoPath, Query<ApiTokenList>, NoBody>| async move {
                Ok(Json(list(&api.pool, &q).await?))
            }),
        route(Method::GET, BY_ID, "getApiToken")
            .tag(TAG)
            .summary("Get one API token (without its secret)")
            .requires(manage)
            .session_only()
            .errors(&[ErrorCode::NotFound])
            .handle(|api, In(IdPath(id), NoQuery, NoBody): In<IdPath, NoQuery, NoBody>| async move {
                Ok(Json(get(&api.pool, id).await?))
            }),
        route(Method::POST, BASE, "createApiToken")
            .tag(TAG)
            .summary("Create an API token; the response carries its secret, shown this once")
            .description(
                "The token acts as its owner (`userId`, default yourself), limited to what `profileId` allows: its permissions are those the owner and the profile both grant. `expiresAt` is required, in the future and at most 366 days away. 403 when the owner holds permissions you do not. 400 when the owner is disabled or the owner or profile does not exist.",
            )
            .status(StatusCode::CREATED)
            .requires(manage)
            .session_only()
            .handle(|api, In(NoPath, NoQuery, Body(b)): In<NoPath, NoQuery, Body<ApiTokenCreate>>| async move {
                Ok(Json(create(&api.pool, &api.ctx, &b).await?))
            }),
        route(Method::DELETE, BY_ID, "revokeApiToken")
            .tag(TAG)
            .summary("Revoke an API token (it stays listed as revoked; revoking twice is a no-op)")
            .description("403 when the owner holds permissions you do not (your own tokens are always revocable).")
            .requires(manage)
            .session_only()
            .errors(&[ErrorCode::NotFound])
            .handle(|api, In(IdPath(id), NoQuery, NoBody): In<IdPath, NoQuery, NoBody>| async move {
                revoke(&api.pool, &api.ctx, id).await?;
                Ok(NoContent)
            }),
    ]
}

#[cfg(test)]
pub(crate) mod tests {
    use std::time::Duration as StdDuration;

    use axum::Router;
    use axum::body::Body as HttpBody;
    use axum::http::{HeaderMap, Request, header};
    use serde_json::{Value, json};
    use tower::ServiceExt;

    use crate::config::{ApiDocs, AuthConfig, Config, CookieSecure, DatabaseConfig, HttpConfig, SslMode};
    use crate::db::scratch;
    use crate::http::{AppState, router};

    /// The real router on a scratch database (also used by the MFA tests).
    pub(crate) fn app(pool: sqlx::PgPool) -> Router {
        let auth = AuthConfig {
            session_idle: StdDuration::from_secs(3600),
            session_max_age: StdDuration::from_secs(3600),
            cookie_secure: CookieSecure::Never,
            public_url: None,
        };
        let cfg = Config {
            api_host: "127.0.0.1".into(),
            api_port: 3000,
            cors_origins: Vec::new(),
            csp_report_uri: None,
            api_docs: ApiDocs::Off,
            http: HttpConfig {
                header_read_timeout: StdDuration::from_secs(10),
                request_timeout: StdDuration::from_secs(120),
            },
            database: DatabaseConfig {
                url: Some("postgres://unused".into()),
                host: None,
                port: 5432,
                database: None,
                user: None,
                password: None,
                ssl: SslMode::Disable,
                ssl_ca_file: None,
                pool_max: 1,
                statement_timeout: StdDuration::ZERO,
                connect_timeout: StdDuration::from_secs(1),
                roles: Default::default(),
            },
            migration_url: None,
            maintenance_url: None,
            auth: auth.clone(),
            audit: Default::default(),
        };
        router(AppState::new(pool, auth), &cfg)
    }

    #[derive(Default, Clone)]
    pub(crate) struct Creds {
        pub cookie: Option<String>,
        pub csrf: Option<String>,
        pub bearer: Option<String>,
    }

    pub(crate) async fn call(
        app: &Router,
        method: &str,
        path: &str,
        creds: &Creds,
        body: Option<Value>,
    ) -> (u16, Value, HeaderMap) {
        let mut req = Request::builder().method(method).uri(path).header(header::USER_AGENT, "token-test");
        if let Some(c) = &creds.cookie {
            req = req.header(header::COOKIE, c);
        }
        if let Some(c) = &creds.csrf {
            req = req.header("x-csrf-token", c);
        }
        if let Some(b) = &creds.bearer {
            req = req.header(header::AUTHORIZATION, format!("Bearer {b}"));
        }
        let req = match body {
            Some(b) => req.header(header::CONTENT_TYPE, "application/json").body(HttpBody::from(b.to_string())),
            None => req.body(HttpBody::empty()),
        };
        let res = app.clone().oneshot(req.unwrap()).await.unwrap();
        let status = res.status().as_u16();
        let headers = res.headers().clone();
        let bytes = axum::body::to_bytes(res.into_body(), 1 << 20).await.unwrap();
        (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null), headers)
    }

    pub(crate) fn code(v: &Value) -> &str {
        v["error"]["code"].as_str().unwrap_or_default()
    }

    /// The whole lifecycle through the real router: create with a session,
    /// use (scoped, no CSRF), refusals, no fall-back to the cookie, revoke,
    /// expiry, and what the audit log and the table keep.
    #[tokio::test]
    async fn api_tokens_are_scoped_audited_and_revocable() {
        let Some(db) = scratch::database("api_tokens_are_scoped_audited_and_revocable").await else { return };
        let app = app(db.pool.clone());
        let pool = &db.pool;

        let setup = json!({ "username": "owner", "displayName": "Owner", "password": "correct horse battery" });
        let (status, me, headers) = call(&app, "POST", "/api/v1/setup", &Creds::default(), Some(setup)).await;
        assert_eq!(status, 201, "{me}");
        let cookie = headers
            .get_all(header::SET_COOKIE)
            .iter()
            .map(|v| v.to_str().unwrap().split(';').next().unwrap().to_owned())
            .collect::<Vec<_>>()
            .join("; ");
        let session =
            Creds { cookie: Some(cookie.clone()), csrf: me["csrfToken"].as_str().map(str::to_owned), bearer: None };
        let owner_id = me["user"]["id"].as_str().unwrap().to_owned();

        // A scope that may only read the audit log and view every class.
        let readers: uuid::Uuid =
            sqlx::query_scalar("INSERT INTO permission_profiles (name) VALUES ('Readers') RETURNING id")
                .fetch_one(pool)
                .await
                .unwrap();
        sqlx::query(
            "INSERT INTO permission_profile_global_permissions (profile_id, permission) VALUES ($1, 'audit.view')",
        )
        .bind(readers)
        .execute(pool)
        .await
        .unwrap();

        // Creating needs the session's CSRF token like any other write.
        let expires = (chrono::Utc::now() + chrono::Duration::days(30)).to_rfc3339();
        let create = json!({ "name": "backup script", "profileId": readers, "expiresAt": expires });
        let no_csrf = Creds { csrf: None, ..session.clone() };
        let (status, v, _) = call(&app, "POST", super::BASE, &no_csrf, Some(create.clone())).await;
        assert_eq!((status, code(&v)), (403, "CSRF_TOKEN_INVALID"));
        let too_long = json!({ "name": "x", "profileId": readers,
            "expiresAt": (chrono::Utc::now() + chrono::Duration::days(400)).to_rfc3339() });
        let (status, v, _) = call(&app, "POST", super::BASE, &session, Some(too_long)).await;
        assert_eq!((status, v["error"]["details"][0]["field"].as_str()), (400, Some("expiresAt")));

        let (status, created, _) = call(&app, "POST", super::BASE, &session, Some(create)).await;
        assert_eq!(status, 201, "{created}");
        let secret = created["secret"].as_str().unwrap().to_owned();
        let token_id = created["token"]["id"].as_str().unwrap().to_owned();
        assert!(secret.starts_with("scmdb_") && secret.len() == 70);
        assert_eq!(created["token"]["tokenPrefix"].as_str(), Some(&secret[..14]));
        assert_eq!(created["token"]["status"], "active");
        assert_eq!(created["token"]["userId"].as_str(), Some(owner_id.as_str()));

        // The secret is never readable again.
        let (status, listed, _) = call(&app, "GET", super::BASE, &session, None).await;
        assert_eq!((status, listed["page"]["total"].as_i64()), (200, Some(1)));
        assert!(!listed.to_string().contains(&secret));

        let tok = Creds { bearer: Some(secret.clone()), ..Creds::default() };
        // In scope: works without any cookie or CSRF token.
        let (status, _, _) = call(&app, "GET", "/api/v1/audit-log?limit=1", &tok, None).await;
        assert_eq!(status, 200);
        // The owner is an administrator, but the scope does not grant users.manage.
        let (status, v, _) = call(&app, "GET", "/api/v1/admin/users", &tok, None).await;
        assert_eq!((status, code(&v)), (403, "FORBIDDEN"));
        // Authenticated-only routes work; class rights come from the scope (none here).
        let (status, v, _) = call(&app, "GET", "/api/v1/configuration-items", &tok, None).await;
        assert_eq!((status, v["page"]["total"].as_i64()), (200, Some(0)), "{v}");
        // Tokens cannot mint tokens or act as a session.
        let later = json!({ "name": "child", "profileId": readers, "expiresAt": expires });
        let (status, v, _) = call(&app, "POST", super::BASE, &tok, Some(later)).await;
        assert_eq!((status, code(&v)), (403, "FORBIDDEN"));
        let (status, _, _) = call(&app, "GET", "/api/v1/auth/me", &tok, None).await;
        assert_eq!(status, 403);

        // A Bearer header means token auth only: a bad token next to a live
        // session cookie is 401, not a CSRF-free ride on the session.
        let mixed = Creds { bearer: Some("scmdb_nope".into()), ..no_csrf.clone() };
        let (status, v, _) = call(&app, "PUT", "/api/v1/ui-settings", &mixed, Some(json!({}))).await;
        assert_eq!((status, code(&v)), (401, "UNAUTHENTICATED"));
        let session_token = cookie.split("; ").find_map(|c| c.strip_prefix("shadoucmdb_session=")).unwrap();
        let (status, _, _) = call(
            &app,
            "GET",
            "/api/v1/audit-log",
            &Creds { bearer: Some(session_token.into()), ..Creds::default() },
            None,
        )
        .await;
        assert_eq!(status, 401, "a session token is not an API token");

        // Stored hashed; the use is recorded.
        let (hash, last_used): (Vec<u8>, Option<chrono::DateTime<chrono::Utc>>) =
            sqlx::query_as("SELECT token_hash, last_used_at FROM api_tokens").fetch_one(pool).await.unwrap();
        assert_eq!(hash, crate::auth::session::token_hash(&secret));
        assert!(last_used.is_some());

        // Revoke (session + CSRF): the token stops working at once.
        let by_id = format!("{}/{token_id}", super::BASE);
        let (status, _, _) = call(&app, "DELETE", &by_id, &session, None).await;
        assert_eq!(status, 204);
        let (status, v, _) = call(&app, "DELETE", &by_id, &session, None).await;
        assert_eq!(status, 204, "revoking twice is a no-op: {v}");
        let (status, v, _) = call(&app, "GET", "/api/v1/audit-log", &tok, None).await;
        assert_eq!((status, v["error"]["message"].as_str()), (401, Some("This API token has been revoked")));
        let (_, got, _) = call(&app, "GET", &by_id, &session, None).await;
        assert_eq!((got["status"].as_str(), got["revokedBy"].as_str()), (Some("revoked"), Some("owner")));

        // Expiry.
        let expires = (chrono::Utc::now() + chrono::Duration::days(1)).to_rfc3339();
        let create = json!({ "name": "short", "profileId": readers, "expiresAt": expires });
        let (_, created, _) = call(&app, "POST", super::BASE, &session, Some(create)).await;
        let short = Creds { bearer: created["secret"].as_str().map(str::to_owned), ..Creds::default() };
        sqlx::query("UPDATE api_tokens SET created_at = now() - interval '2 days', expires_at = now() - interval '1 second' WHERE name = 'short'")
            .execute(pool)
            .await
            .unwrap();
        let (status, v, _) = call(&app, "GET", "/api/v1/audit-log", &short, None).await;
        assert_eq!((status, v["error"]["message"].as_str()), (401, Some("This API token has expired")));

        // The audit trail: create, revoke and every use of a known token, never the secret.
        let rows: Vec<(String, String, String, Value)> = sqlx::query_as(
            "SELECT action, actor_type, coalesce(actor_name, ''), coalesce(new_value, old_value) FROM audit_log
             WHERE entity_type = 'api_tokens' AND entity_id = $1::uuid ORDER BY id",
        )
        .bind(&token_id)
        .fetch_all(pool)
        .await
        .unwrap();
        let summary: Vec<(&str, &str, &str)> =
            rows.iter().map(|(a, t, _, v)| (a.as_str(), t.as_str(), v["outcome"].as_str().unwrap_or("-"))).collect();
        assert_eq!(
            summary,
            vec![
                ("create", "user", "-"),
                ("token.use", "api_client", "accepted"),
                ("token.use", "api_client", "forbidden"),
                ("token.use", "api_client", "accepted"),
                ("token.use", "api_client", "session_only"),
                ("token.use", "api_client", "session_only"),
                ("update", "user", "-"),
                ("token.use", "api_client", "revoked"),
            ]
        );
        let used = &rows[1].3;
        assert_eq!((used["method"].as_str(), used["path"].as_str()), (Some("GET"), Some("/api/v1/audit-log")));
        assert_eq!(
            (used["operationId"].as_str(), used["userAgent"].as_str()),
            (Some("listAuditLog"), Some("token-test"))
        );
        let everything: String = sqlx::query_scalar(
            "SELECT string_agg(coalesce(old_value::text, '') || coalesce(new_value::text, ''), '') FROM audit_log",
        )
        .fetch_one(pool)
        .await
        .unwrap();
        assert!(!everything.contains(&secret[6..]) && !everything.contains(&hex::encode(&hash)));

        db.drop().await;
    }

    /// A token scoped to users.manage may read accounts but not create,
    /// change, delete or set the password of one: a credential it minted
    /// would outlive the token's revocation (GH #119).
    #[tokio::test]
    async fn user_administration_needs_a_session() {
        let Some(db) = scratch::database("user_administration_needs_a_session").await else { return };
        let app = app(db.pool.clone());
        let pool = &db.pool;

        let setup = json!({ "username": "owner", "displayName": "Owner", "password": "correct horse battery" });
        let (status, me, headers) = call(&app, "POST", "/api/v1/setup", &Creds::default(), Some(setup)).await;
        assert_eq!(status, 201, "{me}");
        let cookie = headers
            .get_all(header::SET_COOKIE)
            .iter()
            .map(|v| v.to_str().unwrap().split(';').next().unwrap().to_owned())
            .collect::<Vec<_>>()
            .join("; ");
        let session = Creds { cookie: Some(cookie), csrf: me["csrfToken"].as_str().map(str::to_owned), bearer: None };

        let managers: uuid::Uuid =
            sqlx::query_scalar("INSERT INTO permission_profiles (name) VALUES ('User managers') RETURNING id")
                .fetch_one(pool)
                .await
                .unwrap();
        sqlx::query(
            "INSERT INTO permission_profile_global_permissions (profile_id, permission) VALUES ($1, 'users.manage')",
        )
        .bind(managers)
        .execute(pool)
        .await
        .unwrap();
        let peer = json!({ "username": "peer", "displayName": "Peer", "password": "another long passphrase" });
        let (status, peer, _) = call(&app, "POST", "/api/v1/admin/users", &session, Some(peer)).await;
        assert_eq!(status, 201, "{peer}");
        let peer_id = peer["id"].as_str().unwrap().to_owned();

        let expires = (chrono::Utc::now() + chrono::Duration::days(30)).to_rfc3339();
        let create = json!({ "name": "provisioning", "profileId": managers, "expiresAt": expires });
        let (status, created, _) = call(&app, "POST", super::BASE, &session, Some(create)).await;
        assert_eq!(status, 201, "{created}");
        let tok = Creds { bearer: created["secret"].as_str().map(str::to_owned), ..Creds::default() };

        // Reading stays open to the token.
        let (status, v, _) = call(&app, "GET", "/api/v1/admin/users", &tok, None).await;
        assert_eq!((status, v["page"]["total"].as_i64()), (200, Some(2)), "{v}");
        let peer_path = format!("/api/v1/admin/users/{peer_id}");
        let (status, _, _) = call(&app, "GET", &peer_path, &tok, None).await;
        assert_eq!(status, 200);

        // Every account write is refused.
        let minted = json!({ "username": "minted", "displayName": "Minted", "password": "a token-made passphrase",
            "profileIds": [managers] });
        let writes = [
            ("POST", "/api/v1/admin/users".to_owned(), Some(minted)),
            ("PATCH", peer_path.clone(), Some(json!({ "profileIds": [managers] }))),
            ("PATCH", peer_path.clone(), Some(json!({ "isActive": false }))),
            ("PUT", format!("{peer_path}/password"), Some(json!({ "password": "a token-chosen passphrase" }))),
            ("DELETE", peer_path.clone(), None),
        ];
        for (method, path, body) in writes {
            let (status, v, _) = call(&app, method, &path, &tok, body).await;
            assert_eq!((status, code(&v)), (403, "FORBIDDEN"), "{method} {path}: {v}");
        }

        let (users, peer_active): (i64, bool) = sqlx::query_as(
            "SELECT (SELECT count(*) FROM users), (SELECT is_active FROM users WHERE username = 'peer')",
        )
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!((users, peer_active), (2, true));
        let outcomes: Vec<String> =
            sqlx::query_scalar("SELECT new_value->>'outcome' FROM audit_log WHERE action = 'token.use' ORDER BY id")
                .fetch_all(pool)
                .await
                .unwrap();
        assert_eq!(
            outcomes,
            ["accepted", "accepted", "session_only", "session_only", "session_only", "session_only", "session_only"]
        );

        // A session still administers accounts.
        let (status, v, _) = call(
            &app,
            "PUT",
            &format!("{peer_path}/password"),
            &session,
            Some(json!({ "password": "a fresh long passphrase" })),
        )
        .await;
        assert_eq!(status, 200, "{v}");

        db.drop().await;
    }

    /// A token with the Administrator profile may read identity providers
    /// and run the connection test, but not add, change (settings or group
    /// mappings) or delete one: a provider or mapping it set up would keep
    /// signing people in after the token's revocation (GH #137).
    #[tokio::test]
    async fn identity_provider_administration_needs_a_session() {
        let Some(db) = scratch::database("identity_provider_administration_needs_a_session").await else { return };
        let app = app(db.pool.clone());
        let pool = &db.pool;

        let setup = json!({ "username": "owner", "displayName": "Owner", "password": "correct horse battery" });
        let (status, me, headers) = call(&app, "POST", "/api/v1/setup", &Creds::default(), Some(setup)).await;
        assert_eq!(status, 201, "{me}");
        let cookie = headers
            .get_all(header::SET_COOKIE)
            .iter()
            .map(|v| v.to_str().unwrap().split(';').next().unwrap().to_owned())
            .collect::<Vec<_>>()
            .join("; ");
        let session = Creds { cookie: Some(cookie), csrf: me["csrfToken"].as_str().map(str::to_owned), bearer: None };

        let administrators: uuid::Uuid =
            sqlx::query_scalar("SELECT id FROM permission_profiles WHERE is_builtin AND name = 'Administrator'")
                .fetch_one(pool)
                .await
                .unwrap();
        // Nothing listens on port 1, so the connection test fails fast.
        let directory = |name: &str, group: &str| {
            json!({ "kind": "ldap", "name": name,
                "ldap": { "url": "ldaps://127.0.0.1:1", "userBaseDn": "dc=example,dc=com" },
                "groupMappings": [{ "group": group, "profileId": administrators }] })
        };
        let (status, idp, _) = call(
            &app,
            "POST",
            "/api/v1/admin/identity-providers",
            &session,
            Some(directory("Corporate AD", "cn=cmdb-admins,dc=example,dc=com")),
        )
        .await;
        assert_eq!(status, 201, "{idp}");
        let idp_path = format!("/api/v1/admin/identity-providers/{}", idp["id"].as_str().unwrap());

        let expires = (chrono::Utc::now() + chrono::Duration::days(30)).to_rfc3339();
        let create = json!({ "name": "automation", "profileId": administrators, "expiresAt": expires });
        let (status, created, _) = call(&app, "POST", super::BASE, &session, Some(create)).await;
        assert_eq!(status, 201, "{created}");
        let tok = Creds { bearer: created["secret"].as_str().map(str::to_owned), ..Creds::default() };

        // Reading and the connection test (it changes nothing) stay open to the token.
        let (status, v, _) = call(&app, "GET", "/api/v1/admin/identity-providers", &tok, None).await;
        assert_eq!(status, 200, "{v}");
        let (status, v, _) = call(&app, "GET", &idp_path, &tok, None).await;
        assert_eq!(status, 200, "{v}");
        let (status, v, _) = call(&app, "POST", &format!("{idp_path}/test"), &tok, Some(json!({}))).await;
        assert_eq!((status, v["ok"].as_bool()), (200, Some(false)), "{v}");

        // Every provider write is refused.
        let writes = [
            (
                "POST",
                "/api/v1/admin/identity-providers".to_owned(),
                Some(directory("Rogue directory", "cn=everyone,dc=example,dc=com")),
            ),
            (
                "PATCH",
                idp_path.clone(),
                Some(
                    json!({ "groupMappings": [{ "group": "cn=everyone,dc=example,dc=com", "profileId": administrators }] }),
                ),
            ),
            ("PATCH", idp_path.clone(), Some(json!({ "ldap": { "url": "ldaps://attacker.example.com" } }))),
            ("PATCH", idp_path.clone(), Some(json!({ "isEnabled": false }))),
            ("DELETE", idp_path.clone(), None),
        ];
        for (method, path, body) in writes {
            let (status, v, _) = call(&app, method, &path, &tok, body).await;
            assert_eq!((status, code(&v)), (403, "FORBIDDEN"), "{method} {path}: {v}");
        }

        let (status, v, _) = call(&app, "GET", &idp_path, &session, None).await;
        assert_eq!(status, 200, "{v}");
        assert_eq!(
            (v["isEnabled"].as_bool(), v["ldap"]["url"].as_str(), v["groupMappings"][0]["group"].as_str()),
            (Some(true), Some("ldaps://127.0.0.1:1"), Some("cn=cmdb-admins,dc=example,dc=com")),
            "{v}"
        );
        let providers: i64 =
            sqlx::query_scalar("SELECT count(*) FROM identity_providers").fetch_one(pool).await.unwrap();
        assert_eq!(providers, 1);
        let outcomes: Vec<String> =
            sqlx::query_scalar("SELECT new_value->>'outcome' FROM audit_log WHERE action = 'token.use' ORDER BY id")
                .fetch_all(pool)
                .await
                .unwrap();
        assert_eq!(
            outcomes,
            [
                "accepted",
                "accepted",
                "accepted",
                "session_only",
                "session_only",
                "session_only",
                "session_only",
                "session_only"
            ]
        );

        // A session still administers providers.
        let remap =
            json!({ "groupMappings": [{ "group": "cn=cmdb-owners,dc=example,dc=com", "profileId": administrators }] });
        let (status, v, _) = call(&app, "PATCH", &idp_path, &session, Some(remap)).await;
        assert_eq!(status, 200, "{v}");
        let (status, v, _) = call(&app, "DELETE", &idp_path, &session, None).await;
        assert_eq!(status, 204, "{v}");

        db.drop().await;
    }

    /// GitHub #154: a token must not widen or weaken a permission profile
    /// (including `requireMfa` on the built-in one) or import profiles, since
    /// the change would outlive the token's revocation.
    #[tokio::test]
    async fn permission_profile_administration_needs_a_session() {
        let Some(db) = scratch::database("permission_profile_administration_needs_a_session").await else { return };
        let app = app(db.pool.clone());
        let pool = &db.pool;

        let setup = json!({ "username": "owner", "displayName": "Owner", "password": "correct horse battery" });
        let (status, me, headers) = call(&app, "POST", "/api/v1/setup", &Creds::default(), Some(setup)).await;
        assert_eq!(status, 201, "{me}");
        let cookie = headers
            .get_all(header::SET_COOKIE)
            .iter()
            .map(|v| v.to_str().unwrap().split(';').next().unwrap().to_owned())
            .collect::<Vec<_>>()
            .join("; ");
        let session = Creds { cookie: Some(cookie), csrf: me["csrfToken"].as_str().map(str::to_owned), bearer: None };

        let administrators: uuid::Uuid =
            sqlx::query_scalar("SELECT id FROM permission_profiles WHERE is_builtin AND name = 'Administrator'")
                .fetch_one(pool)
                .await
                .unwrap();
        let admin_path = format!("/api/v1/admin/profiles/{administrators}");
        let readers = json!({ "name": "Readers", "globalPermissions": ["audit.view"], "classPermissions": [] });
        let (status, profile, _) = call(&app, "POST", "/api/v1/admin/profiles", &session, Some(readers)).await;
        assert_eq!(status, 201, "{profile}");
        let profile_path = format!("/api/v1/admin/profiles/{}", profile["id"].as_str().unwrap());

        let expires = (chrono::Utc::now() + chrono::Duration::days(30)).to_rfc3339();
        let create = json!({ "name": "automation", "profileId": administrators, "expiresAt": expires });
        let (status, created, _) = call(&app, "POST", super::BASE, &session, Some(create)).await;
        assert_eq!(status, 201, "{created}");
        let tok = Creds { bearer: created["secret"].as_str().map(str::to_owned), ..Creds::default() };

        // Reading profiles and exporting the configuration stay open to the token.
        let (status, v, _) = call(&app, "GET", "/api/v1/admin/profiles", &tok, None).await;
        assert_eq!(status, 200, "{v}");
        let (status, v, _) = call(&app, "GET", &admin_path, &tok, None).await;
        assert_eq!(status, 200, "{v}");
        let (status, mut file, _) = call(&app, "GET", "/api/v1/admin/config/export", &tok, None).await;
        assert_eq!(status, 200, "{file}");
        file["permissionProfiles"][0]["globalPermissions"] = json!(["audit.view", "users.manage"]);

        // Every profile write, and the import, is refused.
        let writes = [
            ("PATCH", admin_path.clone(), Some(json!({ "requireMfa": true }))),
            ("PATCH", profile_path.clone(), Some(json!({ "globalPermissions": ["audit.view", "users.manage"] }))),
            ("POST", "/api/v1/admin/profiles".to_owned(), Some(json!({ "name": "Rogue", "globalPermissions": [] }))),
            ("POST", format!("{admin_path}/clone"), Some(json!({ "name": "Rogue administrators" }))),
            ("DELETE", profile_path.clone(), None),
            ("POST", "/api/v1/admin/config/import?mode=apply".to_owned(), Some(file.clone())),
            ("POST", "/api/v1/admin/config/import?mode=dry_run".to_owned(), Some(file.clone())),
        ];
        for (method, path, body) in writes {
            let (status, v, _) = call(&app, method, &path, &tok, body).await;
            assert_eq!((status, code(&v)), (403, "FORBIDDEN"), "{method} {path}: {v}");
        }

        let (_, v, _) = call(&app, "GET", &admin_path, &session, None).await;
        assert_eq!(v["requireMfa"], json!(false), "{v}");
        let (_, v, _) = call(&app, "GET", &profile_path, &session, None).await;
        assert_eq!(v["globalPermissions"], json!(["audit.view"]), "{v}");
        let rogue: i64 = sqlx::query_scalar("SELECT count(*) FROM permission_profiles WHERE name LIKE 'Rogue%'")
            .fetch_one(pool)
            .await
            .unwrap();
        assert_eq!(rogue, 0);
        let outcomes: Vec<String> =
            sqlx::query_scalar("SELECT new_value->>'outcome' FROM audit_log WHERE action = 'token.use' ORDER BY id")
                .fetch_all(pool)
                .await
                .unwrap();
        assert_eq!(outcomes[..3], ["accepted", "accepted", "accepted"]);
        assert_eq!(outcomes[3..], ["session_only"; 7]);

        // A session still administers profiles and imports.
        let (status, v, _) = call(&app, "POST", "/api/v1/admin/config/import?mode=apply", &session, Some(file)).await;
        assert_eq!(status, 200, "{v}");
        let (status, v, _) = call(&app, "DELETE", &profile_path, &session, None).await;
        assert_eq!(status, 204, "{v}");

        db.drop().await;
    }
}
