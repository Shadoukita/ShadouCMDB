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
//! create or revoke tokens of users whose permissions they hold themselves,
//! and a token for another user only with a profile they hold themselves; such
//! a token is also capped at its creator's current permissions (GH#178).

use axum::http::{Method, StatusCode};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{PgConnection, PgPool};
use utoipa::openapi::schema::{KnownFormat, ObjectBuilder, Schema, SchemaFormat, Type};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use super::users::{ProfileRef, must_cover_user};
use crate::api::context::{RequestContext, forbidden};
use crate::api::route::{Body, Check, IdPath, In, Json, NoBody, NoContent, NoPath, NoQuery, Query, Route, route};
use crate::api::schemas::{self, Page, Paged, QueryBool, Sort, UuidList, like_pattern, trimmed, ts, ts_opt};
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
    /// The creator's name, for display
    #[schema(required = true)]
    pub created_by: Option<String>,
    /// The user who created the token; null when the CLI created it, the
    /// creator was deleted, or (for a token older than this field) the
    /// creator is unknown
    #[schema(required = true)]
    pub created_by_user_id: Option<Uuid>,
    /// Created from a session signed in with a second factor. When the owner
    /// must use two-factor authentication, only such tokens are accepted
    pub mfa_verified: bool,
    /// A working token that is refused because its owner must use two-factor
    /// authentication and `mfaVerified` is false; create a new token for it
    pub refused_for_mfa: bool,
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
            created_by_user_id: r.created_by_user_id,
            mfa_verified: r.mfa_verified,
            refused_for_mfa: r.refused_for_mfa,
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
    /// (and, for another owner, you: you must hold the profile's permissions)
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
    /// Tokens created by any of these users (for any owner)
    #[param(schema_with = schemas::uuid_list_schema)]
    created_by: Option<UuidList>,
    #[param(inline)]
    status: Option<TokenStatus>,
    /// true: only the working tokens refused because their owner must use
    /// two-factor authentication (`refusedForMfa`); false: all others
    #[param(inline)]
    refused_for_mfa: Option<QueryBool>,
}
paged!(ApiTokenList);

// ---------------------------------------------------------------------------
// Service
// ---------------------------------------------------------------------------

async fn load(conn: &mut PgConnection, id: Uuid, for_update: bool) -> Result<ApiToken, AppError> {
    let row = data::get(conn, id, for_update).await?.ok_or_else(|| AppError::missing("API token", id))?;
    Ok(row.into())
}

/// The owners whose tokens the caller may see, as for revoking: themselves and
/// the users whose permissions they hold (GH#441). `None`: every owner (an
/// administrator, or no user at all).
async fn visible_owners(conn: &mut PgConnection, ctx: &RequestContext) -> Result<Option<Vec<Uuid>>, AppError> {
    let Some(me) = ctx.principal().filter(|me| !me.permissions.administrator) else { return Ok(None) };
    let owners = auth_data::load_token_owner_permissions(conn).await?;
    // Owners absent from the map hold no profile, so anyone covers them; they
    // are excluded only by an explicit list, hence the NOT ANY below.
    let hidden = owners
        .into_iter()
        .filter(|(id, theirs)| *id != me.user_id && !me.permissions.covers(theirs))
        .map(|(id, _)| id)
        .collect();
    Ok(Some(hidden))
}

pub async fn list(pool: &PgPool, ctx: &RequestContext, q: &ApiTokenList) -> Result<Page<ApiToken>, AppError> {
    let mut conn = pool.acquire().await?;
    let hidden = visible_owners(&mut conn, ctx).await?;
    let filter = |w: &mut Where<'_>| {
        if let Some(ids) = &hidden {
            w.and().push("NOT (t.user_id = ANY(").push_bind(ids.clone()).push("))");
        }
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
        if let Some(ids) = &q.created_by {
            w.and().push("t.created_by_user_id = ANY(").push_bind(ids.0.clone()).push(")");
        }
        match q.status {
            Some(TokenStatus::Active) => w.and_sql("t.revoked_at IS NULL AND t.expires_at > now()"),
            Some(TokenStatus::Expired) => w.and_sql("t.revoked_at IS NULL AND t.expires_at <= now()"),
            Some(TokenStatus::Revoked) => w.and_sql("t.revoked_at IS NOT NULL"),
            None => {}
        }
        match q.refused_for_mfa.map(bool::from) {
            Some(true) => w.and_sql(&data::REFUSED_WORKING),
            Some(false) => w.and_sql(&format!("NOT {}", *data::REFUSED_WORKING)),
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
        crud::select_page::<TokenRow>(&mut conn, &data::FROM, &data::COLUMNS, &filter, &order, q.limit, q.offset)
            .await?;
    Ok(Page { data: rows.into_iter().map(ApiToken::from).collect(), page: q.page_meta(total) })
}

/// 404, not 403, for a token whose owner the caller does not cover: the list
/// does not show it either (GH#441).
pub async fn get(pool: &PgPool, ctx: &RequestContext, id: Uuid) -> Result<ApiToken, AppError> {
    let mut conn = pool.acquire().await?;
    let token = load(&mut conn, id, false).await?;
    if let Some(me) = ctx.principal()
        && me.user_id != token.user_id
        && !me.permissions.covers(&auth_data::load_permissions(&mut conn, token.user_id).await?)
    {
        return Err(AppError::missing("API token", id));
    }
    Ok(token)
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
    // Serialised with a password change of the owner or the caller (GH#143):
    // it either revokes this token, or it ended the caller's session first
    // and the mint is refused here.
    let locked: Vec<Uuid> = std::iter::once(owner_id).chain(me.filter(|&id| id != owner_id)).collect();
    auth_data::share_lock_users(&mut tx, &locked).await?;
    // The token inherits whether the creating session proved a second factor
    // (GH#200). Without a user (an operator command) the operator has host
    // access; a user without a session cannot get here (session-only route)
    // and would count as unverified.
    let mfa_verified = match ctx.principal().map(|p| p.session_id()) {
        None => true,
        Some(None) => false,
        Some(Some(session_id)) => match auth_data::session_mfa_verified(&mut tx, session_id).await? {
            Some(verified) => verified,
            None => return Err(AppError::new(ErrorCode::Unauthenticated, "Your session has ended; sign in again")),
        },
    };
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
    // For another owner the profile is the ceiling that outlives today's
    // owner: it must not grant more than the caller holds (GH#178).
    if Some(owner_id) != me
        && let Some(p) = ctx.principal()
        && !p.permissions.covers(&data::profile_permissions(&mut tx, b.profile_id).await?)
    {
        return Err(forbidden("This permission profile grants permissions you do not hold yourself"));
    }
    // No token that would be refused at its first use.
    if data::refused_for_mfa_if_created(&mut tx, owner_id, mfa_verified).await? {
        let message = if Some(owner_id) == me {
            "You must use two-factor authentication, so your tokens must be created from a session signed in with \
             a second factor. Set up two-factor authentication and sign in again."
                .to_owned()
        } else {
            format!(
                "{} must use two-factor authentication, so tokens for this account must be created from a session \
                 signed in with a second factor. Set up two-factor authentication for your own account and sign in \
                 again.",
                owner.username
            )
        };
        return Err(AppError::new(ErrorCode::MfaRequiredForToken, message));
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
            created_by_user_id: me,
            mfa_verified,
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
/// 404, like [`get`], for a token whose owner the caller does not cover, even
/// once it is revoked: a 403 would tell that it exists (GH#458).
pub async fn revoke(pool: &PgPool, ctx: &RequestContext, id: Uuid) -> Result<(), AppError> {
    let mut tx = pool.begin().await?;
    let before = load(&mut tx, id, true).await?;
    if let Some(me) = ctx.principal()
        && me.user_id != before.user_id
        && !me.permissions.covers(&auth_data::load_permissions(&mut tx, before.user_id).await?)
    {
        return Err(AppError::missing("API token", id));
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

/// Revokes every token of the user that still works, in the transaction that
/// sets their password, disables or deletes them: a token minted with a stolen
/// password must not outlive the reset. With `created_for_others` (an
/// administrator's reset, disabling, deleting), also every working token the
/// user created for another owner: the account may have been compromised or
/// its holder has left, and such a token would otherwise outlive the change
/// (GH#145, GH#183). `because` names the change in the log ("password reset").
/// Each gets an update row; returns how many were revoked.
///
/// Call it before anything else in the transaction writes to the audit log:
/// every audit insert takes the chain head, and taking it before these token
/// rows lets two resets wait on each other (GH#166).
pub async fn revoke_all_of_user(
    conn: &mut PgConnection,
    ctx: &RequestContext,
    user_id: Uuid,
    created_for_others: bool,
    because: &str,
) -> Result<usize, AppError> {
    let rows = data::active_of_user(conn, user_id, created_for_others).await?;
    revoke_rows(conn, ctx, user_id, rows, because).await
}

async fn revoke_rows(
    conn: &mut PgConnection,
    ctx: &RequestContext,
    user_id: Uuid,
    rows: Vec<TokenRow>,
    because: &str,
) -> Result<usize, AppError> {
    let by = ctx.actor.name.clone().unwrap_or_else(|| ctx.actor.actor_type.as_str().to_owned());
    let mut entries = Vec::new();
    for row in rows {
        let before = ApiToken::from(row);
        data::revoke(conn, before.id, &by).await?;
        let after = load(conn, before.id, false).await?;
        let whose = if after.user_id == user_id { "its owner's" } else { "its creator's" };
        tracing::info!(token = %after.token_prefix, owner = %after.username, "API token revoked with {whose} {because}");
        entries.push(AuditEntry {
            action: AuditAction::Update,
            entity_type: TOKEN_ENTITY,
            entity_id: after.id,
            old_value: Some(crud::json(&before)),
            new_value: Some(crud::json(&after)),
        });
    }
    let revoked = entries.len();
    crud::write_audit(conn, ctx, entries).await?;
    Ok(revoked)
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
            .summary("List API tokens (paginated, searchable, filterable by owner, creator and status); never their secrets")
            .description("Lists your own tokens and those of users whose permissions you hold yourself (all tokens for an administrator); the page total counts the same rows.")
            .requires(manage)
            .session_only()
            .handle(|api, In(NoPath, Query(q), NoBody): In<NoPath, Query<ApiTokenList>, NoBody>| async move {
                Ok(Json(list(&api.pool, &api.ctx, &q).await?))
            }),
        route(Method::GET, BY_ID, "getApiToken")
            .tag(TAG)
            .summary("Get one API token (without its secret)")
            .description("404 when the owner holds permissions you do not (your own tokens are always readable).")
            .requires(manage)
            .session_only()
            .errors(&[ErrorCode::NotFound])
            .handle(|api, In(IdPath(id), NoQuery, NoBody): In<IdPath, NoQuery, NoBody>| async move {
                Ok(Json(get(&api.pool, &api.ctx, id).await?))
            }),
        route(Method::POST, BASE, "createApiToken")
            .tag(TAG)
            .summary("Create an API token; the response carries its secret, shown this once")
            .description(
                "The token acts as its owner (`userId`, default yourself), limited to what `profileId` allows: its permissions are those the owner and the profile both grant, and for a token you create for another owner also only those you hold at the time of use. `expiresAt` is required, in the future and at most 366 days away. 403 when the owner, or for another owner the profile, holds permissions you do not, and 403 `MFA_REQUIRED_FOR_TOKEN` when the owner must use two-factor authentication and your session did not sign in with a second factor (the token would be refused). The token records that as `mfaVerified`. 400 when the owner is disabled or the owner or profile does not exist.",
            )
            .status(StatusCode::CREATED)
            .requires(manage)
            .recent_reauthentication()
            .errors(&[ErrorCode::MfaRequiredForToken])
            .handle(|api, In(NoPath, NoQuery, Body(b)): In<NoPath, NoQuery, Body<ApiTokenCreate>>| async move {
                Ok(Json(create(&api.pool, &api.ctx, &b).await?))
            }),
        route(Method::DELETE, BY_ID, "revokeApiToken")
            .tag(TAG)
            .summary("Revoke an API token (it stays listed as revoked; revoking twice is a no-op)")
            .description("404, as for a missing token, when the owner holds permissions you do not (your own tokens are always revocable).")
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
        app_with(pool, CookieSecure::Never)
    }

    /// The real router with `cookie_secure` in place of `CookieSecure::Never`.
    pub(crate) fn app_with(pool: sqlx::PgPool, cookie_secure: CookieSecure) -> Router {
        build_app(pool, cookie_secure, crate::http::Capacity::new(512, StdDuration::from_secs(10)))
    }

    /// The real router with `capacity` (HTTP_MAX_CONCURRENT_REQUESTS) in place of the default.
    pub(crate) fn app_with_capacity(pool: sqlx::PgPool, capacity: crate::http::Capacity) -> Router {
        build_app(pool, CookieSecure::Never, capacity)
    }

    /// The real router with `capacity`, and `configure` applied to the authentication settings.
    pub(crate) fn app_with_auth(
        pool: sqlx::PgPool,
        capacity: crate::http::Capacity,
        configure: impl FnOnce(&mut AuthConfig),
    ) -> Router {
        build_app_with(pool, CookieSecure::Never, capacity, configure)
    }

    fn build_app(pool: sqlx::PgPool, cookie_secure: CookieSecure, capacity: crate::http::Capacity) -> Router {
        build_app_with(pool, cookie_secure, capacity, |_| {})
    }

    /// The real router with these bulk import limits.
    pub(crate) fn app_with_imports(pool: sqlx::PgPool, imports: crate::config::ImportConfig) -> Router {
        build_app_full(
            pool,
            CookieSecure::Never,
            crate::http::Capacity::new(512, StdDuration::from_secs(10)),
            |_| {},
            imports,
            Default::default(),
            None,
        )
    }

    /// The real router with these business service limits.
    pub(crate) fn app_with_business_services(
        pool: sqlx::PgPool,
        limits: crate::config::BusinessServiceConfig,
    ) -> Router {
        build_app_full(
            pool,
            CookieSecure::Never,
            crate::http::Capacity::new(512, StdDuration::from_secs(10)),
            |_| {},
            Default::default(),
            limits,
            None,
        )
    }

    /// The real router with this inventory export cap (else derived from the pool's size).
    pub(crate) fn app_with_exports(pool: sqlx::PgPool, exports: crate::config::ExportConfig) -> Router {
        build_app_full(
            pool,
            CookieSecure::Never,
            crate::http::Capacity::new(512, StdDuration::from_secs(10)),
            |_| {},
            Default::default(),
            Default::default(),
            Some(exports),
        )
    }

    fn build_app_with(
        pool: sqlx::PgPool,
        cookie_secure: CookieSecure,
        capacity: crate::http::Capacity,
        configure: impl FnOnce(&mut AuthConfig),
    ) -> Router {
        build_app_full(pool, cookie_secure, capacity, configure, Default::default(), Default::default(), None)
    }

    fn build_app_full(
        pool: sqlx::PgPool,
        cookie_secure: CookieSecure,
        capacity: crate::http::Capacity,
        configure: impl FnOnce(&mut AuthConfig),
        imports: crate::config::ImportConfig,
        business_services: crate::config::BusinessServiceConfig,
        exports: Option<crate::config::ExportConfig>,
    ) -> Router {
        let mut auth = AuthConfig {
            session_idle: StdDuration::from_secs(3600),
            session_max_age: StdDuration::from_secs(3600),
            cookie_secure,
            public_url: None,
            oidc_allowed_hosts: None,
            setup_token: Some(crate::auth::setup_token::TEST_TOKEN.into()),
            setup_token_file: None,
            trusted_proxies: Default::default(),
            sign_in_failure_floor: std::time::Duration::ZERO,
        };
        configure(&mut auth);
        let cfg = Config {
            api_host: "127.0.0.1".into(),
            api_port: 3000,
            cors_origins: Vec::new(),
            csp_report_uri: None,
            api_docs: ApiDocs::Off,
            http: HttpConfig {
                header_read_timeout: StdDuration::from_secs(10),
                request_timeout: StdDuration::from_secs(120),
                body_timeout: StdDuration::from_secs(30),
                send_timeout: StdDuration::from_secs(60),
                max_concurrent_requests: 512,
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
            encryption: Default::default(),
            impact: Default::default(),
            imports: imports.clone(),
            business_services,
            exports: Default::default(),
        };
        let mut state = AppState::new(pool, auth, crate::secrets::Keyring::for_tests())
            .importing(&imports)
            .with_business_services(business_services);
        if let Some(exports) = exports {
            state = state.with_exports(exports);
        }
        router(AppState { capacity, ..state }, &cfg)
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

        let setup = json!({ "username": "owner", "email": "owner@example.test", "displayName": "Owner", "password": "correct horse battery", "setupToken": crate::auth::setup_token::TEST_TOKEN });
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
        // GH#179: the path is recorded before it is validated, so it is kept bounded;
        // replaying the token adds no row within the minute.
        let long = format!("/api/v1/configuration-items/{}", "x".repeat(65_000));
        let (status, v, _) = call(&app, "GET", &long, &tok, None).await;
        assert_eq!((status, v["error"]["message"].as_str()), (401, Some("This API token has been revoked")));
        for _ in 0..3 {
            let (status, _, _) = call(&app, "GET", "/api/v1/audit-log", &tok, None).await;
            assert_eq!(status, 401);
        }
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
        let refused = &rows[7].3;
        let kept = refused["path"].as_str().unwrap();
        assert_eq!(kept.chars().count(), crate::auth::events::TOKEN_PATH_MAX + 1, "{kept:.80}");
        assert!(kept.starts_with("/api/v1/configuration-items/xxx") && kept.ends_with('…'));
        assert_eq!((refused["pathLength"].as_u64(), refused.get("unrecordedRefusals")), (Some(65_028), None));
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

        let setup = json!({ "username": "owner", "email": "owner@example.test", "displayName": "Owner", "password": "correct horse battery", "setupToken": crate::auth::setup_token::TEST_TOKEN });
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
        let peer = json!({ "username": "peer", "email": "peer@example.test", "displayName": "Peer", "password": "another long passphrase" });
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
        let minted = json!({ "username": "minted", "email": "minted@example.test", "displayName": "Minted", "password": "a token-made passphrase",
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

    pub(crate) fn session_of(me: &Value, headers: &HeaderMap) -> Creds {
        let cookie = headers
            .get_all(header::SET_COOKIE)
            .iter()
            .map(|v| v.to_str().unwrap().split(';').next().unwrap().to_owned())
            .collect::<Vec<_>>()
            .join("; ");
        Creds { cookie: Some(cookie), csrf: me["csrfToken"].as_str().map(str::to_owned), bearer: None }
    }

    /// The session a response renewed (GH#510): its cookies, and the CSRF
    /// token from the CSRF cookie, as a browser reading it would.
    pub(crate) fn renewed(headers: &HeaderMap) -> Creds {
        let cookies: Vec<String> = headers
            .get_all(header::SET_COOKIE)
            .iter()
            .map(|v| v.to_str().unwrap().split(';').next().unwrap().to_owned())
            .collect();
        let csrf = cookies.iter().find_map(|c| c.strip_prefix("shadoucmdb_csrf=")).map(str::to_owned);
        assert!(csrf.is_some(), "the response renews the session: {cookies:?}");
        Creds { cookie: Some(cookies.join("; ")), csrf, bearer: None }
    }

    /// A new password, set by an administrator or by the owner, revokes the
    /// owner's API tokens: a token minted with a stolen password must not
    /// survive the reset (GH#124).
    #[tokio::test]
    async fn setting_a_password_revokes_the_owners_tokens() {
        let Some(db) = scratch::database("setting_a_password_revokes_the_owners_tokens").await else { return };
        let app = app(db.pool.clone());
        let pool = &db.pool;

        let setup = json!({ "username": "admin", "email": "admin@example.test", "displayName": "Admin", "password": "correct horse battery", "setupToken": crate::auth::setup_token::TEST_TOKEN });
        let (status, me, headers) = call(&app, "POST", "/api/v1/setup", &Creds::default(), Some(setup)).await;
        assert_eq!(status, 201, "{me}");
        let admin = session_of(&me, &headers);
        let administrators: uuid::Uuid =
            sqlx::query_scalar("SELECT id FROM permission_profiles WHERE name = 'Administrator'")
                .fetch_one(pool)
                .await
                .unwrap();
        let alice = json!({ "username": "alice", "email": "alice@example.test", "displayName": "Alice", "password": "alice first password",
            "profileIds": [administrators] });
        let (status, v, _) = call(&app, "POST", "/api/v1/admin/users", &admin, Some(alice)).await;
        assert_eq!(status, 201, "{v}");
        let alice_id = v["id"].as_str().unwrap().to_owned();
        let login = json!({ "username": "alice", "password": "alice first password" });
        let (status, me, headers) = call(&app, "POST", "/api/v1/auth/login", &Creds::default(), Some(login)).await;
        assert_eq!(status, 200, "{me}");
        let alice_session = session_of(&me, &headers);

        let expires = (chrono::Utc::now() + chrono::Duration::days(30)).to_rfc3339();
        let mint = |session: Creds, name: &'static str| {
            let app = app.clone();
            let expires = expires.clone();
            async move {
                let body = json!({ "name": name, "profileId": administrators, "expiresAt": expires });
                let (status, created, _) = call(&app, "POST", super::BASE, &session, Some(body)).await;
                assert_eq!(status, 201, "{created}");
                Creds { bearer: created["secret"].as_str().map(str::to_owned), ..Creds::default() }
            }
        };
        let works = |tok: Creds| {
            let app = app.clone();
            async move { call(&app, "GET", "/api/v1/audit-log?limit=1", &tok, None).await }
        };

        // An administrator resets Alice's password: her token is revoked by them.
        let alices = mint(alice_session.clone(), "alice script").await;
        let admins = mint(admin.clone(), "admin script").await;
        assert_eq!(works(alices.clone()).await.0, 200);
        let reset = json!({ "password": "alice second password" });
        let path = format!("/api/v1/admin/users/{alice_id}/password");
        let (status, v, _) = call(&app, "PUT", &path, &admin, Some(reset)).await;
        assert_eq!(status, 200, "{v}");
        let (status, v, _) = works(alices).await;
        assert_eq!((status, v["error"]["message"].as_str()), (401, Some("This API token has been revoked")));
        let revoked: Vec<(String, Option<String>)> =
            sqlx::query_as("SELECT name, revoked_by FROM api_tokens WHERE revoked_at IS NOT NULL ORDER BY name")
                .fetch_all(pool)
                .await
                .unwrap();
        assert_eq!(revoked, vec![("alice script".to_owned(), Some("admin".to_owned()))]);
        // Only hers: the administrator's own token keeps working.
        assert_eq!(works(admins.clone()).await.0, 200);

        // Alice changes her own password: her tokens go, her session continues under a new cookie.
        let alice_session = {
            let login = json!({ "username": "alice", "password": "alice second password" });
            let (status, me, headers) = call(&app, "POST", "/api/v1/auth/login", &Creds::default(), Some(login)).await;
            assert_eq!(status, 200, "{me}");
            session_of(&me, &headers)
        };
        let alices = mint(alice_session.clone(), "alice again").await;
        let change = json!({ "currentPassword": "alice second password", "newPassword": "alice third password" });
        let (status, v, headers) = call(&app, "PUT", "/api/v1/auth/password", &alice_session, Some(change)).await;
        assert!(status < 300, "{status} {v}");
        assert_eq!(works(alices).await.0, 401);
        let (status, _, _) = call(&app, "GET", "/api/v1/auth/me", &renewed(&headers), None).await;
        assert_eq!(status, 200, "the caller's session continues under its new cookie");
        let (status, _, _) = call(&app, "GET", "/api/v1/auth/me", &alice_session, None).await;
        assert_eq!(status, 401, "the old cookie no longer works (GH#510)");
        let (rotated,): (Value,) = sqlx::query_as(
            "SELECT new_value FROM audit_log WHERE action = 'session.revoke' AND new_value->>'reason' = 'rotated'",
        )
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(rotated["username"].as_str(), Some("alice"), "{rotated}");
        assert!(rotated["replacedBy"].is_string(), "the audit names the new session: {rotated}");
        let (by,): (Option<String>,) = sqlx::query_as("SELECT revoked_by FROM api_tokens WHERE name = 'alice again'")
            .fetch_one(pool)
            .await
            .unwrap();
        assert_eq!(by.as_deref(), Some("alice"));
        assert_eq!(works(admins).await.0, 200);

        // Each revocation is audited as an update of the token.
        let audited: Vec<(String, Option<String>, Value)> = sqlx::query_as(
            "SELECT a.actor_name, t.name, a.new_value FROM audit_log a JOIN api_tokens t ON t.id = a.entity_id
             WHERE a.entity_type = 'api_tokens' AND a.action = 'update' ORDER BY a.id",
        )
        .fetch_all(pool)
        .await
        .unwrap();
        let summary: Vec<(&str, Option<&str>, Option<&str>)> =
            audited.iter().map(|(a, n, v)| (a.as_str(), n.as_deref(), v["status"].as_str())).collect();
        assert_eq!(
            summary,
            vec![("admin", Some("alice script"), Some("revoked")), ("alice", Some("alice again"), Some("revoked"))]
        );

        db.drop().await;
    }

    /// An administrator's reset of a user's password also revokes the tokens
    /// that user minted for other owners; their own password change does not.
    /// `createdBy` finds them either way (GH#145).
    #[tokio::test]
    async fn resetting_a_password_revokes_the_tokens_the_user_minted_for_others() {
        let Some(db) = scratch::database("resetting_a_password_revokes_tokens_minted_for_others").await else {
            return;
        };
        let app = app(db.pool.clone());
        let pool = &db.pool;

        let setup = json!({ "username": "admin", "email": "admin@example.test", "displayName": "Admin", "password": "correct horse battery", "setupToken": crate::auth::setup_token::TEST_TOKEN });
        let (status, me, headers) = call(&app, "POST", "/api/v1/setup", &Creds::default(), Some(setup)).await;
        assert_eq!(status, 201, "{me}");
        let admin = session_of(&me, &headers);
        let administrators: uuid::Uuid =
            sqlx::query_scalar("SELECT id FROM permission_profiles WHERE name = 'Administrator'")
                .fetch_one(pool)
                .await
                .unwrap();
        let mut ids = Vec::new();
        for name in ["alice", "bob"] {
            let body = json!({ "username": name, "email": format!("{name}@example.test"), "displayName": name, "password": format!("{name} first password"),
                "profileIds": [administrators] });
            let (status, v, _) = call(&app, "POST", "/api/v1/admin/users", &admin, Some(body)).await;
            assert_eq!(status, 201, "{v}");
            ids.push(v["id"].as_str().unwrap().to_owned());
        }
        let (alice_id, bob_id) = (ids[0].clone(), ids[1].clone());
        let sign_in = |password: &'static str| {
            let app = app.clone();
            async move {
                let login = json!({ "username": "alice", "password": password });
                let (status, me, headers) =
                    call(&app, "POST", "/api/v1/auth/login", &Creds::default(), Some(login)).await;
                assert_eq!(status, 200, "{me}");
                session_of(&me, &headers)
            }
        };
        let alice = sign_in("alice first password").await;

        let expires = (chrono::Utc::now() + chrono::Duration::days(30)).to_rfc3339();
        let mint = |session: Creds, owner: String, name: &'static str| {
            let app = app.clone();
            let expires = expires.clone();
            async move {
                let body = json!({ "name": name, "userId": owner, "profileId": administrators, "expiresAt": expires });
                let (status, created, _) = call(&app, "POST", super::BASE, &session, Some(body)).await;
                assert_eq!(status, 201, "{created}");
                Creds { bearer: created["secret"].as_str().map(str::to_owned), ..Creds::default() }
            }
        };
        let works = |tok: Creds| {
            let app = app.clone();
            async move { call(&app, "GET", "/api/v1/audit-log?limit=1", &tok, None).await }
        };

        // Alice mints a token for Bob; the administrator mints one for Bob too.
        let alices_for_bob = mint(alice.clone(), bob_id.clone(), "minted by alice").await;
        let admins_for_bob = mint(admin.clone(), bob_id.clone(), "minted by admin").await;
        let (status, v, _) = call(&app, "GET", &format!("{}?createdBy={alice_id}", super::BASE), &admin, None).await;
        assert_eq!(status, 200, "{v}");
        let listed: Vec<(&str, &str, &str)> = v["data"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| {
                (t["name"].as_str().unwrap(), t["userId"].as_str().unwrap(), t["createdByUserId"].as_str().unwrap())
            })
            .collect();
        assert_eq!(listed, vec![("minted by alice", bob_id.as_str(), alice_id.as_str())]);

        // Her own password change leaves the token she minted for Bob alone.
        let change = json!({ "currentPassword": "alice first password", "newPassword": "alice second password" });
        let (status, v, _) = call(&app, "PUT", "/api/v1/auth/password", &alice, Some(change)).await;
        assert!(status < 300, "{status} {v}");
        assert_eq!(works(alices_for_bob.clone()).await.0, 200);

        // The issue's repro: the administrator resets Alice's password.
        let reset = json!({ "password": "alice third password" });
        let (status, v, _) =
            call(&app, "PUT", &format!("/api/v1/admin/users/{alice_id}/password"), &admin, Some(reset)).await;
        assert_eq!(status, 200, "{v}");
        let (status, v, _) = works(alices_for_bob).await;
        assert_eq!((status, v["error"]["message"].as_str()), (401, Some("This API token has been revoked")));
        assert_eq!(works(admins_for_bob.clone()).await.0, 200, "Bob's token from another creator keeps working");
        let revoked: Vec<(String, Option<String>)> =
            sqlx::query_as("SELECT name, revoked_by FROM api_tokens WHERE revoked_at IS NOT NULL ORDER BY name")
                .fetch_all(pool)
                .await
                .unwrap();
        assert_eq!(revoked, vec![("minted by alice".to_owned(), Some("admin".to_owned()))]);
        let audited: Vec<(String, Value)> = sqlx::query_as(
            "SELECT a.actor_name, a.new_value FROM audit_log a
             WHERE a.entity_type = 'api_tokens' AND a.action = 'update' ORDER BY a.id",
        )
        .fetch_all(pool)
        .await
        .unwrap();
        let summary: Vec<(&str, Option<&str>, Option<&str>)> =
            audited.iter().map(|(a, v)| (a.as_str(), v["name"].as_str(), v["status"].as_str())).collect();
        assert_eq!(summary, vec![("admin", Some("minted by alice"), Some("revoked"))]);

        // Deleting the creator revokes the token too (GH#183); the creator is
        // forgotten, the revoked token stays with its owner.
        let alice = sign_in("alice third password").await;
        let again = mint(alice, bob_id.clone(), "minted before deletion").await;
        let (status, v, _) = call(&app, "DELETE", &format!("/api/v1/admin/users/{alice_id}"), &admin, None).await;
        assert_eq!(status, 200, "{v}");
        let (status, v, _) = works(again).await;
        assert_eq!((status, v["error"]["message"].as_str()), (401, Some("This API token has been revoked")));
        assert_eq!(works(admins_for_bob).await.0, 200);
        let (creator, revoked_by): (Option<uuid::Uuid>, Option<String>) = sqlx::query_as(
            "SELECT created_by_user_id, revoked_by FROM api_tokens WHERE name = 'minted before deletion'",
        )
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!((creator, revoked_by.as_deref()), (None, Some("admin")));

        db.drop().await;
    }

    /// The issue's repro (GH#183): Alice mints a token for the service account
    /// Bob and leaves. Disabling her revokes it with her own tokens, while
    /// Bob's token from another creator keeps working; enabling her again
    /// brings neither back.
    #[tokio::test]
    async fn disabling_a_user_revokes_the_tokens_they_minted_for_others() {
        let Some(db) = scratch::database("disabling_a_user_revokes_tokens_minted_for_others").await else {
            return;
        };
        let app = app(db.pool.clone());
        let pool = &db.pool;

        let setup = json!({ "username": "admin", "email": "admin@example.test", "displayName": "Admin", "password": "correct horse battery", "setupToken": crate::auth::setup_token::TEST_TOKEN });
        let (status, me, headers) = call(&app, "POST", "/api/v1/setup", &Creds::default(), Some(setup)).await;
        assert_eq!(status, 201, "{me}");
        let admin = session_of(&me, &headers);
        let administrators: uuid::Uuid =
            sqlx::query_scalar("SELECT id FROM permission_profiles WHERE name = 'Administrator'")
                .fetch_one(pool)
                .await
                .unwrap();
        let mut ids = Vec::new();
        for name in ["alice", "bob"] {
            let body = json!({ "username": name, "email": format!("{name}@example.test"), "displayName": name, "password": format!("{name} first password"),
                "profileIds": [administrators] });
            let (status, v, _) = call(&app, "POST", "/api/v1/admin/users", &admin, Some(body)).await;
            assert_eq!(status, 201, "{v}");
            ids.push(v["id"].as_str().unwrap().to_owned());
        }
        let (alice_id, bob_id) = (ids[0].clone(), ids[1].clone());
        let login = json!({ "username": "alice", "password": "alice first password" });
        let (status, me, headers) = call(&app, "POST", "/api/v1/auth/login", &Creds::default(), Some(login)).await;
        assert_eq!(status, 200, "{me}");
        let alice = session_of(&me, &headers);

        let expires = (chrono::Utc::now() + chrono::Duration::days(30)).to_rfc3339();
        let mint = |session: Creds, owner: String, name: &'static str| {
            let app = app.clone();
            let expires = expires.clone();
            async move {
                let body = json!({ "name": name, "userId": owner, "profileId": administrators, "expiresAt": expires });
                let (status, created, _) = call(&app, "POST", super::BASE, &session, Some(body)).await;
                assert_eq!(status, 201, "{created}");
                Creds { bearer: created["secret"].as_str().map(str::to_owned), ..Creds::default() }
            }
        };
        let works = |tok: Creds| {
            let app = app.clone();
            async move { call(&app, "GET", "/api/v1/audit-log?limit=1", &tok, None).await }
        };
        let alices_for_bob = mint(alice.clone(), bob_id.clone(), "minted by alice").await;
        let alices_own = mint(alice, alice_id.clone(), "alice's own").await;
        let admins_for_bob = mint(admin.clone(), bob_id.clone(), "minted by admin").await;

        let user = format!("/api/v1/admin/users/{alice_id}");
        let (status, v, _) = call(&app, "PATCH", &user, &admin, Some(json!({ "isActive": false }))).await;
        assert_eq!(status, 200, "{v}");
        let (status, v, _) = works(alices_for_bob.clone()).await;
        assert_eq!((status, v["error"]["message"].as_str()), (401, Some("This API token has been revoked")));
        assert_eq!(works(admins_for_bob.clone()).await.0, 200, "Bob's token from another creator keeps working");

        let (status, v, _) = call(&app, "PATCH", &user, &admin, Some(json!({ "isActive": true }))).await;
        assert_eq!(status, 200, "{v}");
        assert_eq!(works(alices_for_bob).await.0, 401, "enabling her again does not bring it back");
        assert_eq!(works(alices_own).await.0, 401);
        let revoked: Vec<(String, Option<String>)> =
            sqlx::query_as("SELECT name, revoked_by FROM api_tokens WHERE revoked_at IS NOT NULL ORDER BY name")
                .fetch_all(pool)
                .await
                .unwrap();
        let admin_name = Some("admin".to_owned());
        assert_eq!(
            revoked,
            vec![("alice's own".to_owned(), admin_name.clone()), ("minted by alice".to_owned(), admin_name)]
        );
        let audited: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM audit_log
             WHERE entity_type = 'api_tokens' AND action = 'update' AND new_value->>'status' = 'revoked'",
        )
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(audited, 2, "each revocation has its audit row");

        db.drop().await;
    }

    /// The issue's repro (GH#178): Alice holds only users.manage and mints a
    /// token for low-privilege Bob. She cannot scope it wider than her own
    /// rights, and a token that already is (minted before the fix) stays
    /// capped at her rights when Bob is promoted.
    #[tokio::test]
    async fn a_token_minted_for_another_user_is_capped_at_its_creators_rights() {
        let Some(db) = scratch::database("a_token_minted_for_another_user_is_capped_at_its_creators_rights").await
        else {
            return;
        };
        let app = app(db.pool.clone());
        let pool = &db.pool;

        let setup = json!({ "username": "admin", "email": "admin@example.test", "displayName": "Admin", "password": "correct horse battery", "setupToken": crate::auth::setup_token::TEST_TOKEN });
        let (status, me, headers) = call(&app, "POST", "/api/v1/setup", &Creds::default(), Some(setup)).await;
        assert_eq!(status, 201, "{me}");
        let admin = session_of(&me, &headers);
        let administrators: uuid::Uuid =
            sqlx::query_scalar("SELECT id FROM permission_profiles WHERE name = 'Administrator'")
                .fetch_one(pool)
                .await
                .unwrap();
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
        let mut ids = Vec::new();
        for (name, profiles) in [("alice", json!([managers])), ("bob", json!([]))] {
            let body = json!({ "username": name, "email": format!("{name}@example.test"), "displayName": name, "password": format!("{name} first password"),
                "profileIds": profiles });
            let (status, v, _) = call(&app, "POST", "/api/v1/admin/users", &admin, Some(body)).await;
            assert_eq!(status, 201, "{v}");
            ids.push(v["id"].as_str().unwrap().to_owned());
        }
        let bob_id = ids[1].clone();
        let login = json!({ "username": "alice", "password": "alice first password" });
        let (status, me, headers) = call(&app, "POST", "/api/v1/auth/login", &Creds::default(), Some(login)).await;
        assert_eq!(status, 200, "{me}");
        let alice = session_of(&me, &headers);

        // She covers Bob, but not the Administrator profile: refused, nothing stored.
        let expires = (chrono::Utc::now() + chrono::Duration::days(30)).to_rfc3339();
        let wide = json!({ "name": "wide", "userId": bob_id, "profileId": administrators, "expiresAt": expires });
        let (status, v, _) = call(&app, "POST", super::BASE, &alice, Some(wide)).await;
        assert_eq!((status, code(&v)), (403, "FORBIDDEN"), "{v}");
        let stored: i64 = sqlx::query_scalar("SELECT count(*) FROM api_tokens").fetch_one(pool).await.unwrap();
        assert_eq!(stored, 0);

        // A profile she holds herself is fine.
        let narrow = json!({ "name": "for bob", "userId": bob_id, "profileId": managers, "expiresAt": expires });
        let (status, created, _) = call(&app, "POST", super::BASE, &alice, Some(narrow)).await;
        assert_eq!(status, 201, "{created}");
        let tok = Creds { bearer: created["secret"].as_str().map(str::to_owned), ..Creds::default() };

        // As if minted before the fix with the Administrator profile; then Bob
        // is promoted. The token gets what Bob, its profile and Alice all allow.
        sqlx::query("UPDATE api_tokens SET profile_id = $1").bind(administrators).execute(pool).await.unwrap();
        let promote = json!({ "profileIds": [administrators] });
        let (status, v, _) = call(&app, "PATCH", &format!("/api/v1/admin/users/{bob_id}"), &admin, Some(promote)).await;
        assert_eq!(status, 200, "{v}");
        let (status, v, _) = call(&app, "GET", "/api/v1/admin/users", &tok, None).await;
        assert_eq!(status, 200, "within Alice's rights: {v}");
        let (status, v, _) = call(&app, "GET", "/api/v1/audit-log?limit=1", &tok, None).await;
        assert_eq!((status, code(&v)), (403, "FORBIDDEN"), "audit.view is Bob's, not Alice's");

        // The administrator's token for Bob, with the same profile, has it all.
        let body = json!({ "name": "by admin", "userId": bob_id, "profileId": administrators, "expiresAt": expires });
        let (status, created, _) = call(&app, "POST", super::BASE, &admin, Some(body)).await;
        assert_eq!(status, 201, "{created}");
        let admins = Creds { bearer: created["secret"].as_str().map(str::to_owned), ..Creds::default() };
        assert_eq!(call(&app, "GET", "/api/v1/audit-log?limit=1", &admins, None).await.0, 200);

        // The cap comes on top of the owner's requireMfa rule (GH#200): once
        // Bob must use a second factor, Alice's token is refused outright.
        sqlx::query("UPDATE permission_profiles SET require_mfa = true WHERE id = $1")
            .bind(administrators)
            .execute(pool)
            .await
            .unwrap();
        let (status, v, _) = call(&app, "GET", "/api/v1/admin/users", &tok, None).await;
        assert_eq!((status, code(&v)), (401, "UNAUTHENTICATED"), "{v}");
        assert!(v["error"]["message"].as_str().unwrap().contains("must use two-factor authentication"), "{v}");

        db.drop().await;
    }

    /// A user manager sees the tokens of the users they cover and their own,
    /// as for revoking, but not an administrator's: not in the list or its
    /// total, and 404 on reading one (GH#441).
    #[tokio::test]
    async fn a_user_manager_sees_only_the_tokens_of_users_they_cover() {
        let Some(db) = scratch::database("a_user_manager_sees_only_the_tokens_of_users_they_cover").await else {
            return;
        };
        let app = app(db.pool.clone());
        let pool = &db.pool;

        let setup = json!({ "username": "admin", "email": "admin@example.test", "displayName": "Admin", "password": "correct horse battery", "setupToken": crate::auth::setup_token::TEST_TOKEN });
        let (status, me, headers) = call(&app, "POST", "/api/v1/setup", &Creds::default(), Some(setup)).await;
        assert_eq!(status, 201, "{me}");
        let admin = session_of(&me, &headers);
        let administrators: uuid::Uuid =
            sqlx::query_scalar("SELECT id FROM permission_profiles WHERE name = 'Administrator'")
                .fetch_one(pool)
                .await
                .unwrap();
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
        let mut ids = Vec::new();
        for (name, profiles) in [("alice", json!([managers])), ("bob", json!([]))] {
            let body = json!({ "username": name, "email": format!("{name}@example.test"), "displayName": name, "password": format!("{name} first password"),
                "profileIds": profiles });
            let (status, v, _) = call(&app, "POST", "/api/v1/admin/users", &admin, Some(body)).await;
            assert_eq!(status, 201, "{v}");
            ids.push(v["id"].as_str().unwrap().to_owned());
        }
        let bob_id = ids[1].clone();
        let login = json!({ "username": "alice", "password": "alice first password" });
        let (status, me, headers) = call(&app, "POST", "/api/v1/auth/login", &Creds::default(), Some(login)).await;
        assert_eq!(status, 200, "{me}");
        let alice = session_of(&me, &headers);

        let expires = (chrono::Utc::now() + chrono::Duration::days(30)).to_rfc3339();
        let mint = async |who: &Creds, name: &str, owner: Option<&str>| {
            let mut body = json!({ "name": name, "profileId": managers, "expiresAt": expires });
            if let Some(owner) = owner {
                body["userId"] = json!(owner);
            }
            let (status, v, _) = call(&app, "POST", super::BASE, who, Some(body)).await;
            assert_eq!(status, 201, "{v}");
            v["token"]["id"].as_str().unwrap().to_owned()
        };
        let admins = mint(&admin, "admin's own", None).await;
        let alices = mint(&alice, "alice's own", None).await;
        let bobs = mint(&alice, "for bob", Some(&bob_id)).await;

        let names = |v: &Value| {
            let mut n: Vec<String> =
                v["data"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap().to_owned()).collect();
            n.sort();
            n
        };
        let (status, v, _) = call(&app, "GET", super::BASE, &alice, None).await;
        assert_eq!(status, 200, "{v}");
        assert_eq!(names(&v), ["alice's own", "for bob"], "{v}");
        assert_eq!(v["page"]["total"], 2, "{v}");
        let (status, v, _) = call(&app, "GET", &format!("{}?q=admin", super::BASE), &alice, None).await;
        assert_eq!((status, v["page"]["total"].as_i64()), (200, Some(0)), "{v}");
        let (status, v, _) = call(&app, "GET", &format!("{}/{admins}", super::BASE), &alice, None).await;
        assert_eq!((status, code(&v)), (404, "NOT_FOUND"), "{v}");
        for id in [&alices, &bobs] {
            let (status, v, _) = call(&app, "GET", &format!("{}/{id}", super::BASE), &alice, None).await;
            assert_eq!(status, 200, "{v}");
        }

        // Revoking it answers the same 404 and changes nothing (GH#458).
        let (status, v, _) = call(&app, "DELETE", &format!("{}/{admins}", super::BASE), &alice, None).await;
        assert_eq!((status, code(&v)), (404, "NOT_FOUND"), "{v}");
        let unknown = uuid::Uuid::new_v4().to_string();
        let (status, missing, _) = call(&app, "DELETE", &format!("{}/{unknown}", super::BASE), &alice, None).await;
        assert_eq!(status, 404, "{missing}");
        let shape = |mut v: Value, id: &str| {
            v["error"]["requestId"].take();
            v.to_string().replace(id, "ID")
        };
        assert_eq!(shape(v.clone(), &admins), shape(missing, &unknown));
        let revoked: Option<chrono::DateTime<chrono::Utc>> =
            sqlx::query_scalar("SELECT revoked_at FROM api_tokens WHERE id = $1::uuid")
                .bind(&admins)
                .fetch_one(pool)
                .await
                .unwrap();
        assert_eq!(revoked, None);
        let audited: i64 =
            sqlx::query_scalar("SELECT count(*) FROM audit_log WHERE entity_id = $1::uuid AND action = 'update'")
                .bind(&admins)
                .fetch_one(pool)
                .await
                .unwrap();
        assert_eq!(audited, 0);
        // Bob's, whom she covers, she revokes.
        let (status, v, _) = call(&app, "DELETE", &format!("{}/{bobs}", super::BASE), &alice, None).await;
        assert_eq!(status, 204, "{v}");

        // The administrator sees everything.
        let (status, v, _) = call(&app, "GET", super::BASE, &admin, None).await;
        assert_eq!((status, v["page"]["total"].as_i64()), (200, Some(3)), "{v}");
        let (status, v, _) = call(&app, "GET", &format!("{}/{admins}", super::BASE), &admin, None).await;
        assert_eq!(status, 200, "{v}");

        // Once Bob is promoted, the token she minted for him is hidden too.
        let promote = json!({ "profileIds": [administrators] });
        let (status, v, _) = call(&app, "PATCH", &format!("/api/v1/admin/users/{bob_id}"), &admin, Some(promote)).await;
        assert_eq!(status, 200, "{v}");
        let (status, v, _) = call(&app, "GET", super::BASE, &alice, None).await;
        assert_eq!(status, 200, "{v}");
        assert_eq!(names(&v), ["alice's own"], "{v}");
        assert_eq!(v["page"]["total"], 1, "{v}");
        let (status, v, _) = call(&app, "GET", &format!("{}/{bobs}", super::BASE), &alice, None).await;
        assert_eq!((status, code(&v)), (404, "NOT_FOUND"), "{v}");
        // Revoked already, but hidden first: 404, not the no-op 204.
        let (status, v, _) = call(&app, "DELETE", &format!("{}/{bobs}", super::BASE), &alice, None).await;
        assert_eq!((status, code(&v)), (404, "NOT_FOUND"), "{v}");

        db.drop().await;
    }

    /// A mint in flight while the owner's password is reset waits for the
    /// reset and is then refused, as the reset ended the session it came
    /// with: no token minted with the old password survives (GH#143).
    #[tokio::test]
    async fn a_token_minted_during_a_password_reset_does_not_survive_it() {
        let Some(db) = scratch::database("a_token_minted_during_a_password_reset_does_not_survive_it").await else {
            return;
        };
        let app = app(db.pool.clone());
        let pool = &db.pool;

        let setup = json!({ "username": "admin", "email": "admin@example.test", "displayName": "Admin", "password": "correct horse battery", "setupToken": crate::auth::setup_token::TEST_TOKEN });
        let (status, me, headers) = call(&app, "POST", "/api/v1/setup", &Creds::default(), Some(setup)).await;
        assert_eq!(status, 201, "{me}");
        let admin = session_of(&me, &headers);
        let administrators: uuid::Uuid =
            sqlx::query_scalar("SELECT id FROM permission_profiles WHERE name = 'Administrator'")
                .fetch_one(pool)
                .await
                .unwrap();
        let alice = json!({ "username": "alice", "email": "alice@example.test", "displayName": "Alice", "password": "alice first password",
            "profileIds": [administrators] });
        let (status, v, _) = call(&app, "POST", "/api/v1/admin/users", &admin, Some(alice)).await;
        assert_eq!(status, 201, "{v}");
        let alice_id: uuid::Uuid = v["id"].as_str().unwrap().parse().unwrap();
        let login = json!({ "username": "alice", "password": "alice first password" });
        let (status, me, headers) = call(&app, "POST", "/api/v1/auth/login", &Creds::default(), Some(login)).await;
        assert_eq!(status, 200, "{me}");
        let alice_session = session_of(&me, &headers);

        // The reset's steps, held open: Alice's row locked, her sessions ended,
        // her tokens (none yet) revoked.
        let mut reset = pool.begin().await.unwrap();
        crate::data::auth::get_user(&mut reset, alice_id, true).await.unwrap();
        crate::data::auth::delete_user_sessions(&mut reset, alice_id, None).await.unwrap();
        let ctx = crate::api::context::RequestContext::system("admin", "reset");
        super::revoke_all_of_user(&mut reset, &ctx, alice_id, true, "password reset").await.unwrap();

        // Her session passed authentication before the reset commits.
        let expires = (chrono::Utc::now() + chrono::Duration::days(30)).to_rfc3339();
        let body = json!({ "name": "raced", "profileId": administrators, "expiresAt": expires });
        let mint = tokio::spawn({
            let app = app.clone();
            async move { call(&app, "POST", super::BASE, &alice_session, Some(body)).await }
        });
        let mut waiting = false;
        for _ in 0..200 {
            let n: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM pg_stat_activity WHERE datname = current_database() AND wait_event_type = 'Lock'",
            )
            .fetch_one(pool)
            .await
            .unwrap();
            if n > 0 {
                waiting = true;
                break;
            }
            tokio::time::sleep(StdDuration::from_millis(25)).await;
        }
        assert!(waiting, "the mint waits for the reset's lock on the owner");
        reset.commit().await.unwrap();

        let (status, v, _) = mint.await.unwrap();
        assert_eq!((status, code(&v)), (401, "UNAUTHENTICATED"), "{v}");
        let tokens: i64 = sqlx::query_scalar("SELECT count(*) FROM api_tokens").fetch_one(pool).await.unwrap();
        assert_eq!(tokens, 0);

        db.drop().await;
    }

    /// Two administrators reset the passwords of two users who minted tokens
    /// for each other, at the same time and with no session open (sessions
    /// would serialise the resets on the audit chain head): both succeed and
    /// every token is revoked. The token rows were locked in two statements,
    /// in opposite orders, and one reset failed with a deadlock (GH#166).
    #[tokio::test]
    async fn concurrent_resets_of_users_who_minted_tokens_for_each_other_both_succeed() {
        let Some(db) = scratch::database("concurrent_resets_of_users_who_minted_tokens_for_each_other").await else {
            return;
        };
        let app = app(db.pool.clone());
        let pool = &db.pool;

        let setup = json!({ "username": "admin", "email": "admin@example.test", "displayName": "Admin", "password": "correct horse battery", "setupToken": crate::auth::setup_token::TEST_TOKEN });
        let (status, me, headers) = call(&app, "POST", "/api/v1/setup", &Creds::default(), Some(setup)).await;
        assert_eq!(status, 201, "{me}");
        let admin = session_of(&me, &headers);
        let administrators: uuid::Uuid =
            sqlx::query_scalar("SELECT id FROM permission_profiles WHERE name = 'Administrator'")
                .fetch_one(pool)
                .await
                .unwrap();
        let mut ids = Vec::new();
        for name in ["alice", "bob", "second"] {
            let body = json!({ "username": name, "email": format!("{name}@example.test"), "displayName": name, "password": format!("{name} first password"),
                "profileIds": [administrators] });
            let (status, v, _) = call(&app, "POST", "/api/v1/admin/users", &admin, Some(body)).await;
            assert_eq!(status, 201, "{v}");
            ids.push(v["id"].as_str().unwrap().parse::<uuid::Uuid>().unwrap());
        }
        let (alice_id, bob_id) = (ids[0], ids[1]);
        let login = json!({ "username": "second", "password": "second first password" });
        let (status, me, headers) = call(&app, "POST", "/api/v1/auth/login", &Creds::default(), Some(login)).await;
        assert_eq!(status, 200, "{me}");
        let second = session_of(&me, &headers);

        for round in 0..10 {
            // Alice's token for Bob and Bob's for Alice, written directly so
            // that neither has a session open.
            let mut conn = pool.acquire().await.unwrap();
            for (owner, creator) in [(bob_id, alice_id), (alice_id, bob_id)] {
                let name = format!("round {round} for {owner}");
                let hash = crate::auth::session::token_hash(&crate::auth::token::new_secret());
                crate::data::api_tokens::insert(
                    &mut conn,
                    &crate::data::api_tokens::NewToken {
                        name: &name,
                        user_id: owner,
                        profile_id: administrators,
                        token_hash: &hash,
                        token_prefix: &name[..8],
                        expires_at: chrono::Utc::now() + chrono::Duration::days(1),
                        created_by: None,
                        created_by_user_id: Some(creator),
                        mfa_verified: false,
                    },
                )
                .await
                .unwrap();
            }
            drop(conn);

            let reset = |session: Creds, id: uuid::Uuid| {
                let app = app.clone();
                let body = json!({ "password": format!("password of round {round}") });
                tokio::spawn(async move {
                    call(&app, "PUT", &format!("/api/v1/admin/users/{id}/password"), &session, Some(body)).await
                })
            };
            let (a, b) = (reset(admin.clone(), alice_id), reset(second.clone(), bob_id));
            for (status, v, _) in [a.await.unwrap(), b.await.unwrap()] {
                assert_eq!(status, 200, "round {round}: {v}");
            }
            let working: i64 = sqlx::query_scalar("SELECT count(*) FROM api_tokens WHERE revoked_at IS NULL")
                .fetch_one(pool)
                .await
                .unwrap();
            assert_eq!(working, 0, "round {round}");
        }

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

        let setup = json!({ "username": "owner", "email": "owner@example.test", "displayName": "Owner", "password": "correct horse battery", "setupToken": crate::auth::setup_token::TEST_TOKEN });
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

        // Reading stays open to the token.
        let (status, v, _) = call(&app, "GET", "/api/v1/admin/identity-providers", &tok, None).await;
        assert_eq!(status, 200, "{v}");
        let (status, v, _) = call(&app, "GET", &idp_path, &tok, None).await;
        assert_eq!(status, 200, "{v}");

        // Every provider write is refused, and so is the connection test: it makes the server
        // connect out with the stored secret (GitHub #192).
        let writes = [
            ("POST", format!("{idp_path}/test"), Some(json!({}))),
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
                "session_only",
                "session_only",
                "session_only",
                "session_only",
                "session_only",
                "session_only"
            ]
        );

        // A session still administers and tests providers.
        let (status, v, _) = call(&app, "POST", &format!("{idp_path}/test"), &session, Some(json!({}))).await;
        assert_eq!((status, v["ok"].as_bool()), (200, Some(false)), "{v}");
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
    /// the change would outlive the token's revocation. Nor export the
    /// configuration (GitHub #445).
    #[tokio::test]
    async fn permission_profile_administration_needs_a_session() {
        let Some(db) = scratch::database("permission_profile_administration_needs_a_session").await else { return };
        let app = app(db.pool.clone());
        let pool = &db.pool;

        let setup = json!({ "username": "owner", "email": "owner@example.test", "displayName": "Owner", "password": "correct horse battery", "setupToken": crate::auth::setup_token::TEST_TOKEN });
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

        // Reading profiles stays open to the token.
        let (status, v, _) = call(&app, "GET", "/api/v1/admin/profiles", &tok, None).await;
        assert_eq!(status, 200, "{v}");
        let (status, v, _) = call(&app, "GET", &admin_path, &tok, None).await;
        assert_eq!(status, 200, "{v}");
        // The configuration export is not (GitHub #445): a leaked token would
        // otherwise pull the data model, profiles and mappings in one call.
        let (status, v, _) = call(&app, "GET", "/api/v1/admin/config/export", &tok, None).await;
        assert_eq!((status, code(&v)), (403, "FORBIDDEN"), "{v}");
        // GH#414: it is audited, so a session must send the CSRF token (a
        // cross-site navigation carries the Lax cookie but no header).
        let exports = || async {
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM audit_log WHERE action = 'export'")
                .fetch_one(pool)
                .await
                .unwrap()
        };
        let no_csrf = Creds { csrf: None, ..session.clone() };
        let (status, v, _) = call(&app, "GET", "/api/v1/admin/config/export", &no_csrf, None).await;
        assert_eq!((status, code(&v)), (403, "CSRF_TOKEN_INVALID"), "{v}");
        assert_eq!(exports().await, 0, "a refused export is not recorded");
        let (status, mut file, _) = call(&app, "GET", "/api/v1/admin/config/export", &session, None).await;
        assert_eq!(status, 200, "{file}");
        assert_eq!(exports().await, 1);
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
        assert_eq!(outcomes[..2], ["accepted", "accepted"]);
        assert_eq!(outcomes[2..], ["session_only"; 8]);

        // A session still administers profiles and imports.
        let (status, v, _) = call(&app, "POST", "/api/v1/admin/config/import?mode=apply", &session, Some(file)).await;
        assert_eq!(status, 200, "{v}");
        let (status, v, _) = call(&app, "DELETE", &profile_path, &session, None).await;
        assert_eq!(status, 204, "{v}");

        db.drop().await;
    }
}
