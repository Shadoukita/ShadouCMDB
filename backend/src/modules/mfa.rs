//! Two-factor authentication: TOTP authenticator apps and one-time recovery
//! codes. Setting up, confirming and turning off one's own MFA, new recovery
//! codes, and an administrator's reset for a user who lost their device.
//!
//! The sign-in step itself (POST /api/v1/auth/login/mfa) lives with the rest
//! of sign-in in [`super::auth`]; it uses [`verify_second_factor`] from here.

use axum::http::{Method, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::{PgConnection, PgPool, Postgres, Transaction};
use utoipa::ToSchema;
use utoipa::openapi::schema::{ObjectBuilder, Schema, Type};
use uuid::Uuid;

use super::auth::{confirm_current_password, confirm_current_password_attempt, password_field_schema};
use super::users;
use crate::api::context::{RequestContext, unauthenticated};
use crate::api::route::{Body, Check, IdPath, In, Json, NoBody, NoContent, NoPath, NoQuery, Route, route};
use crate::auth::events::{self, LoginMethod, RevokeReason};
use crate::auth::permissions::GlobalPermission;
use crate::auth::secret::Secret;
use crate::auth::throttle::Attempt;
use crate::auth::{AuthState, Principal, totp};
use crate::data::auth as auth_data;
use crate::data::crud::AuditAction;
use crate::data::mfa as data;
use crate::http::error::{AppError, ErrorCode};
use crate::secrets::{self, Keyring, sealed};

// ---------------------------------------------------------------------------
// Schemas
// ---------------------------------------------------------------------------

/// The user's two-factor state.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MfaStatus {
    /// An authenticator app is set up: sign-in asks for its code after the password
    pub totp_enabled: bool,
    /// A permission profile the user holds requires MFA (local and directory
    /// accounts; never OIDC accounts, whose provider runs its own second factor)
    pub required: bool,
    /// Required, and this session did not prove a second factor against a set-up
    /// authenticator (none is set up, or the session was opened with the
    /// password alone): until then the session only reaches sign-out, /auth/me
    /// and the MFA set-up routes (others answer 403 MFA_ENROLMENT_REQUIRED).
    /// With an authenticator already set up, sign in again with a code.
    pub enrolment_required: bool,
    /// Unused recovery codes
    pub recovery_codes_remaining: i64,
}

impl From<data::Status> for MfaStatus {
    fn from(s: data::Status) -> Self {
        MfaStatus {
            totp_enabled: s.totp_enabled,
            required: s.required,
            enrolment_required: s.required && !(s.totp_enabled && s.session_verified),
            recovery_codes_remaining: s.recovery_codes_remaining,
        }
    }
}

/// A new authenticator secret, to be confirmed with a code from the app.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TotpEnrolment {
    /// Base32, for typing into the app by hand
    pub secret: String,
    /// `otpauth://totp/...`: show it as a QR code
    pub otpauth_uri: String,
    /// HMAC algorithm (always SHA1, what every app supports)
    pub algorithm: String,
    pub digits: u32,
    /// Seconds per code
    pub period: u32,
}

/// One-time recovery codes. Shown only in this response: only their hashes are kept.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RecoveryCodes {
    /// Each signs in once in place of an authenticator code (case, dashes and spaces do not matter)
    pub codes: Vec<String>,
}

pub fn code_schema() -> Schema {
    ObjectBuilder::new()
        .schema_type(Type::String)
        .min_length(Some(6))
        .max_length(Some(64))
        .description(Some("The 6-digit code from the authenticator app, or an unused recovery code"))
        .into()
}

fn totp_code_schema() -> Schema {
    ObjectBuilder::new()
        .schema_type(Type::String)
        .min_length(Some(6))
        .max_length(Some(16))
        .description(Some("The 6-digit code the app shows for the new secret"))
        .into()
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PasswordConfirmation {
    #[schema(schema_with = password_field_schema)]
    current_password: Secret,
}
impl Check for PasswordConfirmation {}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TotpConfirmation {
    #[schema(schema_with = totp_code_schema)]
    code: String,
}
impl Check for TotpConfirmation {}

/// The password and a current second factor, to change MFA settings.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MfaReauthentication {
    #[schema(schema_with = password_field_schema)]
    current_password: Secret,
    #[schema(schema_with = code_schema)]
    code: String,
}
impl Check for MfaReauthentication {}

// ---------------------------------------------------------------------------
// Service
// ---------------------------------------------------------------------------

/// `session`: the caller's session (an OIDC sign-in may have proven MFA).
pub async fn status(conn: &mut PgConnection, user_id: Uuid, session: Option<Uuid>) -> Result<MfaStatus, AppError> {
    Ok(data::status(conn, user_id, session).await?.into())
}

/// Outcome of [`verify_second_factor`].
pub enum Verdict {
    Accepted(LoginMethod),
    /// A wrong or reused code.
    Wrong,
    /// The stored authenticator secret did not decrypt (altered, or copied from
    /// another row). Fails closed; recovery codes still work, and an
    /// administrator can reset the user's MFA.
    Undecryptable,
}

impl Verdict {
    /// `reason` of the `mfa.failure` event, when there is more to say than "wrong code".
    pub fn failure_reason(&self) -> Option<&'static str> {
        matches!(self, Verdict::Undecryptable).then_some("secret_undecryptable")
    }
}

/// The user's authenticator secret, decrypted. Logs an undecryptable one (no secret in the log).
fn open_secret(keyring: &Keyring, user_id: Uuid, t: &data::Totp) -> Option<secrets::Secret> {
    match sealed::open_totp_secret(keyring, user_id, t.key_id, &t.secret) {
        Ok(secret) => Some(secret),
        Err(err) => {
            tracing::error!(
                user_id = %user_id,
                error = ?err,
                "the user's authenticator secret does not decrypt; their authenticator codes are refused until an \
                 administrator resets their two-factor authentication"
            );
            None
        }
    }
}

/// Checks `input` against the user's authenticator (a 6-digit code, each
/// usable once) or their unused recovery codes (used up by this call). In the
/// caller's transaction.
pub async fn verify_second_factor(
    conn: &mut PgConnection,
    keyring: &Keyring,
    user_id: Uuid,
    input: &str,
) -> Result<Verdict, AppError> {
    if totp::looks_like_code(input) {
        let Some(t) = data::get_totp(conn, user_id, true).await?.filter(|t| t.confirmed) else {
            return Ok(Verdict::Wrong);
        };
        let Some(secret) = open_secret(keyring, user_id, &t) else { return Ok(Verdict::Undecryptable) };
        let Some(step) = totp::verify(&secret, input, totp::current_step(), t.last_used_step) else {
            return Ok(Verdict::Wrong);
        };
        let used = data::use_step(conn, user_id, step).await?;
        return Ok(if used { Verdict::Accepted(LoginMethod::Totp) } else { Verdict::Wrong });
    }
    let Some(canonical) = totp::normalise_recovery_code(input) else { return Ok(Verdict::Wrong) };
    let used = data::use_recovery_code(conn, user_id, &totp::recovery_code_hash(&canonical)).await?;
    Ok(if used { Verdict::Accepted(LoginMethod::RecoveryCode) } else { Verdict::Wrong })
}

/// Writes `mfa.recovery_code_used` with the number of codes left.
pub async fn audit_recovery_code_used(
    conn: &mut PgConnection,
    ctx: &RequestContext,
    user_id: Uuid,
    username: &str,
    stage: &str,
) -> Result<(), AppError> {
    let left = data::status(conn, user_id, None).await?.recovery_codes_remaining;
    let extra = json!({ "stage": stage, "recoveryCodesRemaining": left });
    events::mfa(conn, ctx, AuditAction::MfaRecoveryCodeUsed, user_id, username, extra).await?;
    Ok(())
}

fn me(ctx: &RequestContext) -> Result<&Principal, AppError> {
    ctx.principal().ok_or_else(unauthenticated)
}

fn already_enabled() -> AppError {
    AppError::conflict("Two-factor authentication is already set up; turn it off first to set up a new authenticator")
}

fn not_enabled() -> AppError {
    AppError::conflict("Two-factor authentication is not set up")
}

/// Fresh recovery codes replace the old ones; returns them in clear, once.
async fn new_recovery_codes(conn: &mut PgConnection, user_id: Uuid) -> Result<RecoveryCodes, AppError> {
    let codes = totp::new_recovery_codes();
    let hashes: Vec<Vec<u8>> = codes
        .iter()
        .map(|c| totp::recovery_code_hash(&totp::normalise_recovery_code(c).expect("generated codes are valid")))
        .collect();
    data::set_recovery_codes(conn, user_id, &hashes).await?;
    Ok(RecoveryCodes { codes })
}

/// Starts setting up an authenticator (again, if a set-up was left unfinished).
async fn enrol(
    pool: &PgPool,
    auth: &AuthState,
    ctx: &RequestContext,
    b: PasswordConfirmation,
) -> Result<TotpEnrolment, AppError> {
    let me = me(ctx)?;
    confirm_current_password(pool, auth, me, &b.current_password).await?;
    let mut tx = pool.begin().await?;
    auth_data::get_user(&mut tx, me.user_id, true).await?;
    if data::get_totp(&mut tx, me.user_id, false).await?.is_some_and(|t| t.confirmed) {
        return Err(already_enabled());
    }
    let secret = totp::new_secret();
    data::put_pending_totp(&mut tx, me.user_id, &sealed::seal_totp_secret(&auth.keyring, me.user_id, &secret)).await?;
    tx.commit().await?;
    Ok(TotpEnrolment {
        secret: totp::base32(&secret),
        otpauth_uri: totp::otpauth_uri(&me.username, &secret),
        algorithm: "SHA1".into(),
        digits: totp::DIGITS as u32,
        period: totp::STEP_SECONDS as u32,
    })
}

/// A code from the app proves it holds the secret: MFA is on from now on.
async fn confirm(
    pool: &PgPool,
    auth: &AuthState,
    ctx: &RequestContext,
    b: TotpConfirmation,
) -> Result<RecoveryCodes, AppError> {
    let me = me(ctx)?;
    let mut tx = pool.begin().await?;
    // The user's row first, as a sign-in locks it before opening a session:
    // one that checked for an authenticator before this commits either
    // committed its session first (which the sweep below ends) or waits and
    // is refused (GH#303).
    auth_data::lock_sign_in(&mut tx, me.user_id).await?;
    let Some(t) = data::get_totp(&mut tx, me.user_id, true).await? else {
        return Err(AppError::conflict("Start the set-up first (POST /api/v1/auth/mfa/totp)"));
    };
    if t.confirmed {
        return Err(already_enabled());
    }
    let Some(secret) = open_secret(&auth.keyring, me.user_id, &t) else {
        return Err(AppError::conflict(
            "The set-up in progress cannot be read; start the set-up again (POST /api/v1/auth/mfa/totp)",
        ));
    };
    let Some(step) = totp::verify(&secret, &b.code, totp::current_step(), None) else {
        return Err(AppError::field(
            "code",
            "The code does not match; check the app's clock and enter the current code",
            "invalid_code",
        ));
    };
    data::confirm_totp(&mut tx, me.user_id, step).await?;
    // This request proved the new authenticator: the session counts as
    // signed in with a second factor (e.g. for creating API tokens, GH#200).
    if let Some(session_id) = me.session_id() {
        auth_data::mark_session_mfa_verified(&mut tx, session_id).await?;
    }
    let codes = new_recovery_codes(&mut tx, me.user_id).await?;
    let extra = json!({ "method": "totp", "recoveryCodes": codes.codes.len() });
    events::mfa(&mut tx, ctx, AuditAction::MfaEnrol, me.user_id, &me.username, extra).await?;
    // Sessions opened with the password alone never proved the new
    // authenticator: they end, so none can change the password or end the
    // verified sessions (GH#292). Their users sign in again with a code.
    let ended = auth_data::delete_unverified_sessions(&mut tx, me.user_id, me.session_id()).await?;
    events::revoked(&mut tx, ctx, &ended, RevokeReason::MfaEnrolled).await?;
    tx.commit().await?;
    tracing::info!(user = %me.username, "two-factor authentication set up");
    Ok(codes)
}

/// After the password (its `attempt`), a current second factor. A wrong code
/// counts against the same per-user lock as a wrong password and is audited as
/// `mfa.failure`; only a right code clears the count. Takes the caller's
/// transaction and hands it back on success; on a wrong code it is rolled back
/// before the failure is audited in a transaction of its own, so the request
/// never holds two pooled connections at once (GH#177).
async fn reauthenticate(
    pool: &PgPool,
    auth: &AuthState,
    ctx: &RequestContext,
    mut tx: Transaction<'static, Postgres>,
    attempt: Attempt<'_>,
    b: &MfaReauthentication,
    stage: &str,
) -> Result<Transaction<'static, Postgres>, AppError> {
    let me = me(ctx)?;
    match verify_second_factor(&mut tx, &auth.keyring, me.user_id, &b.code).await? {
        Verdict::Accepted(method) => {
            attempt.success();
            if matches!(method, LoginMethod::RecoveryCode) {
                audit_recovery_code_used(&mut tx, ctx, me.user_id, &me.username, stage).await?;
            }
            Ok(tx)
        }
        refused => {
            tx.rollback().await?;
            let locked = attempt.failure();
            let mut extra = json!({ "stage": stage, "lockedForSeconds": locked.map(|d| d.as_secs().max(1)) });
            if let Some(reason) = refused.failure_reason() {
                extra["reason"] = reason.into();
            }
            let mut own = pool.begin().await?;
            events::mfa(&mut own, ctx, AuditAction::MfaFailure, me.user_id, &me.username, extra).await?;
            own.commit().await?;
            Err(AppError::field("code", "The code is wrong or was already used", "invalid_code"))
        }
    }
}

/// Turns one's own MFA off (or cancels an unfinished set-up).
async fn disable(
    pool: &PgPool,
    auth: &AuthState,
    ctx: &RequestContext,
    b: MfaReauthentication,
) -> Result<(), AppError> {
    let me = me(ctx)?;
    let attempt = confirm_current_password_attempt(pool, auth, me, &b.current_password).await?;
    let mut tx = pool.begin().await?;
    let Some(t) = data::get_totp(&mut tx, me.user_id, true).await? else { return Err(not_enabled()) };
    let mut tx = if t.confirmed {
        reauthenticate(pool, auth, ctx, tx, attempt, &b, "disable").await?
    } else {
        attempt.success();
        tx
    };
    if data::delete_mfa(&mut tx, me.user_id).await? {
        let extra = json!({ "reason": "self_service" });
        events::mfa(&mut tx, ctx, AuditAction::MfaDisable, me.user_id, &me.username, extra).await?;
        // Sessions that proved the old authenticator must not outlive it (GH#280).
        let ended = auth_data::delete_user_sessions(&mut tx, me.user_id, me.session_id()).await?;
        events::revoked(&mut tx, ctx, &ended, RevokeReason::MfaDisabled).await?;
        tracing::info!(user = %me.username, "two-factor authentication turned off");
    }
    tx.commit().await?;
    Ok(())
}

async fn regenerate(
    pool: &PgPool,
    auth: &AuthState,
    ctx: &RequestContext,
    b: MfaReauthentication,
) -> Result<RecoveryCodes, AppError> {
    let me = me(ctx)?;
    let attempt = confirm_current_password_attempt(pool, auth, me, &b.current_password).await?;
    let mut tx = pool.begin().await?;
    if !data::get_totp(&mut tx, me.user_id, true).await?.is_some_and(|t| t.confirmed) {
        return Err(not_enabled());
    }
    let mut tx = reauthenticate(pool, auth, ctx, tx, attempt, &b, "recovery_codes").await?;
    let codes = new_recovery_codes(&mut tx, me.user_id).await?;
    let extra = json!({ "recoveryCodes": codes.codes.len() });
    events::mfa(&mut tx, ctx, AuditAction::MfaRecoveryCodes, me.user_id, &me.username, extra).await?;
    tx.commit().await?;
    Ok(codes)
}

/// An administrator turns a user's MFA off (lost device and recovery codes)
/// and ends the user's sessions. If a profile requires MFA, the user sets it
/// up again at their next sign-in.
pub async fn reset(pool: &PgPool, ctx: &RequestContext, user_id: Uuid) -> Result<(), AppError> {
    let mut tx = pool.begin().await?;
    let user = auth_data::get_user(&mut tx, user_id, true).await?.ok_or_else(|| AppError::missing("User", user_id))?;
    users::must_cover_user(&mut tx, ctx, user_id).await?;
    if data::delete_mfa(&mut tx, user_id).await? {
        let extra = json!({ "reason": "admin_reset" });
        events::mfa(&mut tx, ctx, AuditAction::MfaDisable, user_id, &user.username, extra).await?;
        // Every session of the user ends, the caller's own excepted when they
        // reset themselves: none may carry over to a later enrolment (GH#280).
        let own = ctx.principal().filter(|p| p.user_id == user_id).and_then(|p| p.session_id());
        let ended = auth_data::delete_user_sessions(&mut tx, user_id, own).await?;
        events::revoked(&mut tx, ctx, &ended, RevokeReason::MfaReset).await?;
        tracing::info!(user = %user.username, "two-factor authentication reset by an administrator");
    }
    tx.commit().await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Routes
// ---------------------------------------------------------------------------

const TAG: &str = "Authentication";
const LOCK_NOTE: &str = "`currentPassword` is the local password, or for a directory (LDAP) account the directory password, checked against the account's own directory entry. 400 when it is wrong; 409 for an account of an OIDC provider (no password here) or while the account's directory is disabled; 503 IDENTITY_PROVIDER_UNAVAILABLE when the directory cannot be reached. Wrong passwords and codes count together: the first 4 cost nothing; from the 5th on, each one locks these routes for this user for 1 s, 2 s, 4 s, ... up to 15 min (429 RATE_LIMITED with Retry-After). Once MFA is set up, a right password alone does not reset that count; a right password together with a right code does.";

pub fn routes() -> Vec<Route> {
    vec![
        route(Method::GET, "/api/v1/auth/mfa", "getMfaStatus")
            .tag(TAG)
            .summary("Your two-factor authentication status")
            .session_only()
            .before_mfa_enrolment()
            .handle(|api, In(NoPath, NoQuery, NoBody): In<NoPath, NoQuery, NoBody>| async move {
                let me = me(&api.ctx)?;
                Ok(Json(status(&mut *api.pool.acquire().await?, me.user_id, me.session_id()).await?))
            }),
        route(Method::POST, "/api/v1/auth/mfa/totp", "startTotpEnrolment")
            .tag(TAG)
            .summary("Start setting up an authenticator app: returns a new secret to confirm")
            .description(format!(
                "Nothing changes at sign-in until the secret is confirmed (POST /api/v1/auth/mfa/totp/confirm); calling this again replaces an unconfirmed secret. 409 when an authenticator is already set up. {LOCK_NOTE}"
            ))
            .status(StatusCode::CREATED)
            .session_only()
            .before_mfa_enrolment()
            .errors(&[ErrorCode::Conflict, ErrorCode::RateLimited, ErrorCode::IdentityProviderUnavailable])
            .handle(|api, In(NoPath, NoQuery, Body(b)): In<NoPath, NoQuery, Body<PasswordConfirmation>>| async move {
                Ok(Json(enrol(&api.pool, &api.auth, &api.ctx, b).await?))
            }),
        route(Method::POST, "/api/v1/auth/mfa/totp/confirm", "confirmTotpEnrolment")
            .tag(TAG)
            .summary("Confirm the new authenticator with a code from it; returns 10 recovery codes (shown once)")
            .description(
                "From now on sign-in asks for a code after the password. This session counts as having proven the second factor; your other sessions that did not prove one end (audited as `session.revoke`, reason mfa_enrolled) and sign in again with a code. 400 (field `code`) when the code does not match; 409 without a started set-up or when one is already confirmed.",
            )
            .session_only()
            .before_mfa_enrolment()
            .errors(&[ErrorCode::Conflict])
            .handle(|api, In(NoPath, NoQuery, Body(b)): In<NoPath, NoQuery, Body<TotpConfirmation>>| async move {
                Ok(Json(confirm(&api.pool, &api.auth, &api.ctx, b).await?))
            }),
        route(Method::DELETE, "/api/v1/auth/mfa/totp", "disableTotp")
            .tag(TAG)
            .summary("Turn your two-factor authentication off (or cancel an unfinished set-up)")
            .description(format!(
                "Needs the password and a current code (authenticator or recovery code; not needed to cancel an unconfirmed set-up). Deletes the recovery codes too and ends your other sessions. If a profile you hold requires MFA, your session is then limited to setting it up again. 400 (field `code`) for a wrong code; 409 when nothing is set up. {LOCK_NOTE}"
            ))
            .session_only()
            .before_mfa_enrolment()
            .errors(&[ErrorCode::Conflict, ErrorCode::RateLimited, ErrorCode::IdentityProviderUnavailable])
            .handle(|api, In(NoPath, NoQuery, Body(b)): In<NoPath, NoQuery, Body<MfaReauthentication>>| async move {
                disable(&api.pool, &api.auth, &api.ctx, b).await?;
                Ok(NoContent)
            }),
        route(Method::POST, "/api/v1/auth/mfa/recovery-codes", "regenerateRecoveryCodes")
            .tag(TAG)
            .summary("Replace your recovery codes with 10 new ones (shown once)")
            .description(format!(
                "Needs the password and a current code. The old codes stop working. 409 when MFA is not set up. {LOCK_NOTE}"
            ))
            .session_only()
            .errors(&[ErrorCode::Conflict, ErrorCode::RateLimited, ErrorCode::IdentityProviderUnavailable])
            .handle(|api, In(NoPath, NoQuery, Body(b)): In<NoPath, NoQuery, Body<MfaReauthentication>>| async move {
                Ok(Json(regenerate(&api.pool, &api.auth, &api.ctx, b).await?))
            }),
        route(Method::DELETE, "/api/v1/admin/users/{id}/mfa", "resetUserMfa")
            .tag("Users")
            .summary("Turn a user's two-factor authentication off (lost authenticator and recovery codes)")
            .description(
                "Deletes the user's authenticator and recovery codes (audited as `mfa.disable`, reason admin_reset) and ends every session of the user (`session.revoke`, reason mfa_reset; your own is kept when you reset yourself). Does nothing if none is set up. If a profile they hold requires MFA, they set it up again after signing in with their password. A non-administrator can only reset users whose permissions they hold themselves (403).",
            )
            .requires(GlobalPermission::UsersManage)
            .session_only()
            .errors(&[ErrorCode::NotFound])
            .handle(|api, In(IdPath(id), NoQuery, NoBody): In<IdPath, NoQuery, NoBody>| async move {
                reset(&api.pool, &api.ctx, id).await?;
                Ok(NoContent)
            }),
    ]
}

#[cfg(test)]
pub(crate) mod tests {
    use axum::Router;
    use axum::http::{HeaderMap, header};
    use serde_json::{Value, json};

    use super::*;
    use crate::db::scratch;
    use crate::modules::api_tokens::tests::{Creds, app, call, code};

    const PASSWORD: &str = "correct horse battery";

    /// The `name=value` pairs of the response's Set-Cookie headers.
    fn set_cookies(headers: &HeaderMap) -> Vec<String> {
        headers
            .get_all(header::SET_COOKIE)
            .iter()
            .map(|v| v.to_str().unwrap().split(';').next().unwrap().to_owned())
            .collect()
    }

    fn session_of(me: &Value, headers: &HeaderMap) -> Creds {
        let cookie = set_cookies(headers).into_iter().filter(|c| !c.starts_with("shadoucmdb_mfa=")).collect::<Vec<_>>();
        Creds { cookie: Some(cookie.join("; ")), csrf: me["csrfToken"].as_str().map(str::to_owned), bearer: None }
    }

    pub(crate) async fn setup(app: &Router) -> (Creds, Value) {
        let body = json!({ "username": "owner", "displayName": "Owner", "password": PASSWORD, "setupToken": crate::auth::setup_token::TEST_TOKEN });
        let (status, me, headers) = call(app, "POST", "/api/v1/setup", &Creds::default(), Some(body)).await;
        assert_eq!(status, 201, "{me}");
        (session_of(&me, &headers), me)
    }

    /// A time step with a few seconds left, so codes for it and its
    /// neighbours stay valid while the test runs.
    async fn settled_step() -> i64 {
        let into =
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() % totp::STEP_SECONDS;
        if into > totp::STEP_SECONDS - 5 {
            tokio::time::sleep(std::time::Duration::from_secs(totp::STEP_SECONDS - into + 1)).await;
        }
        totp::current_step()
    }

    /// The seed of the only `user_totp` row, decrypted. GH#189: what is stored is
    /// not the seed but its ciphertext (48 bytes) under the test key.
    pub(crate) async fn stored_seed(pool: &PgPool) -> Vec<u8> {
        let (user_id, stored, key_id): (Uuid, Vec<u8>, Option<i32>) =
            sqlx::query_as("SELECT user_id, secret, key_id FROM user_totp").fetch_one(pool).await.unwrap();
        let ring = crate::secrets::Keyring::for_tests();
        assert_eq!((stored.len(), key_id), (48, Some(ring.active_id().0)), "sealed under the active key");
        let seed = sealed::open_totp_secret(&ring, user_id, key_id, &stored).expect("opens").to_vec();
        assert_eq!(seed.len(), 20);
        assert!(!stored.windows(seed.len()).any(|w| w == seed), "the seed is not stored in the clear");
        seed
    }

    /// Sets up an authenticator through the API; returns its secret and the recovery codes.
    async fn enrol(app: &Router, pool: &PgPool, session: &Creds, step: i64) -> (Vec<u8>, Vec<String>) {
        let (status, v, _) =
            call(app, "POST", "/api/v1/auth/mfa/totp", session, Some(json!({ "currentPassword": PASSWORD }))).await;
        assert_eq!(status, 201, "{v}");
        let secret = stored_seed(pool).await;
        assert_eq!(v["secret"].as_str(), Some(totp::base32(&secret).as_str()));
        let body = json!({ "code": totp::code_at(&secret, step - 1) });
        let (status, v, _) = call(app, "POST", "/api/v1/auth/mfa/totp/confirm", session, Some(body)).await;
        assert_eq!(status, 200, "{v}");
        let codes = v["codes"].as_array().unwrap().iter().map(|c| c.as_str().unwrap().to_owned()).collect();
        (secret, codes)
    }

    /// Password step: returns the challenge cookie.
    async fn password_step(app: &Router) -> Creds {
        let body = json!({ "username": "owner", "password": PASSWORD });
        let (status, v, headers) = call(app, "POST", "/api/v1/auth/login", &Creds::default(), Some(body)).await;
        assert_eq!((status, code(&v)), (401, "MFA_REQUIRED"), "{v}");
        let cookie = set_cookies(&headers).into_iter().find(|c| c.starts_with("shadoucmdb_mfa=")).expect("mfa cookie");
        Creds { cookie: Some(cookie), ..Creds::default() }
    }

    async fn second_step(app: &Router, challenge: &Creds, code_or_recovery: &str) -> (u16, Value, HeaderMap) {
        call(app, "POST", "/api/v1/auth/login/mfa", challenge, Some(json!({ "code": code_or_recovery }))).await
    }

    async fn mfa_rows(pool: &PgPool) -> Vec<(String, Value)> {
        sqlx::query_as("SELECT action, new_value FROM audit_log WHERE action LIKE 'mfa.%' ORDER BY id")
            .fetch_all(pool)
            .await
            .unwrap()
    }

    /// Enrolment, the second sign-in step, replay protection, one-time
    /// recovery codes, turning MFA off, and what the audit log keeps.
    #[tokio::test]
    async fn totp_sign_in_takes_a_second_step() {
        let Some(db) = scratch::database("totp_sign_in_takes_a_second_step").await else { return };
        let (app, pool) = (app(db.pool.clone()), &db.pool);
        let (session, _) = setup(&app).await;
        let step = settled_step().await;

        let (status, v, _) = call(&app, "GET", "/api/v1/auth/mfa", &session, None).await;
        assert_eq!((status, &v["totpEnabled"], &v["enrolmentRequired"]), (200, &json!(false), &json!(false)));
        let wrong = json!({ "currentPassword": "not the password" });
        let (status, v, _) = call(&app, "POST", "/api/v1/auth/mfa/totp", &session, Some(wrong)).await;
        assert_eq!((status, v["error"]["details"][0]["field"].as_str()), (400, Some("currentPassword")));

        // A code that does not match does not confirm.
        let (status, _, _) =
            call(&app, "POST", "/api/v1/auth/mfa/totp", &session, Some(json!({ "currentPassword": PASSWORD }))).await;
        assert_eq!(status, 201);
        let secret = stored_seed(pool).await;
        let far = json!({ "code": totp::code_at(&secret, step + 10) });
        let (status, v, _) = call(&app, "POST", "/api/v1/auth/mfa/totp/confirm", &session, Some(far)).await;
        assert_eq!((status, v["error"]["details"][0]["code"].as_str()), (400, Some("invalid_code")));
        let (secret, recovery) = enrol(&app, pool, &session, step).await;
        assert_eq!(recovery.len(), 10);
        let (status, _, _) =
            call(&app, "POST", "/api/v1/auth/mfa/totp", &session, Some(json!({ "currentPassword": PASSWORD }))).await;
        assert_eq!(status, 409, "already set up");

        // The password alone no longer signs in; the code step needs the cookie.
        let challenge = password_step(&app).await;
        let (status, v, _) = second_step(&app, &Creds::default(), &totp::code_at(&secret, step)).await;
        assert_eq!((status, code(&v)), (401, "UNAUTHENTICATED"));
        let (status, _, _) = second_step(&app, &challenge, &totp::code_at(&secret, step - 1)).await;
        assert_eq!(status, 401, "the confirming code was used already");
        let (status, me, headers) = second_step(&app, &challenge, &totp::code_at(&secret, step)).await;
        assert_eq!(status, 200, "{me}");
        assert_eq!((&me["mfa"]["totpEnabled"], &me["mfa"]["recoveryCodesRemaining"]), (&json!(true), &json!(10)));
        assert!(set_cookies(&headers).contains(&"shadoucmdb_mfa=".to_owned()), "challenge cookie cleared");
        let (status, _, _) = second_step(&app, &challenge, &totp::code_at(&secret, step + 1)).await;
        assert_eq!(status, 401, "a challenge signs in once");

        // Each code once; recovery codes in any case, once.
        let challenge = password_step(&app).await;
        let (status, _, _) = second_step(&app, &challenge, &totp::code_at(&secret, step)).await;
        assert_eq!(status, 401, "replayed code");
        let (status, me, _) = second_step(&app, &challenge, &recovery[0].to_uppercase()).await;
        assert_eq!((status, &me["mfa"]["recoveryCodesRemaining"]), (200, &json!(9)));
        let challenge = password_step(&app).await;
        let (status, _, _) = second_step(&app, &challenge, &recovery[0]).await;
        assert_eq!(status, 401, "used recovery code");
        let (status, users, _) = call(&app, "GET", "/api/v1/admin/users", &session, None).await;
        assert_eq!((status, &users["data"][0]["mfaEnabled"]), (200, &json!(true)));

        // Turning it off needs the password and a factor.
        let off = |c: &str| json!({ "currentPassword": PASSWORD, "code": c });
        let (status, v, _) = call(&app, "DELETE", "/api/v1/auth/mfa/totp", &session, Some(off("000000"))).await;
        assert_eq!((status, v["error"]["details"][0]["field"].as_str()), (400, Some("code")));
        let (status, _, _) = call(&app, "DELETE", "/api/v1/auth/mfa/totp", &session, Some(off(&recovery[1]))).await;
        assert_eq!(status, 204);
        let body = json!({ "username": "owner", "password": PASSWORD });
        let (status, _, _) = call(&app, "POST", "/api/v1/auth/login", &Creds::default(), Some(body)).await;
        assert_eq!(status, 200, "password-only again");
        let left: i64 = sqlx::query_scalar("SELECT count(*) FROM user_recovery_codes").fetch_one(pool).await.unwrap();
        assert_eq!(left, 0);

        let rows = mfa_rows(pool).await;
        let actions: Vec<&str> = rows.iter().map(|(a, _)| a.as_str()).collect();
        assert_eq!(
            actions,
            [
                "mfa.enrol",
                "mfa.failure",
                "mfa.failure",
                "mfa.recovery_code_used",
                "mfa.failure",
                "mfa.failure",
                "mfa.recovery_code_used",
                "mfa.disable"
            ]
        );
        assert_eq!((rows[1].1["stage"].as_str(), rows[1].1["username"].as_str()), (Some("login"), Some("owner")));
        assert_eq!((rows[3].1["stage"].as_str(), &rows[3].1["recoveryCodesRemaining"]), (Some("login"), &json!(9)));
        assert_eq!((rows[4].1["stage"].as_str(), rows[5].1["stage"].as_str()), (Some("login"), Some("disable")));
        assert_eq!(
            (rows[6].1["stage"].as_str(), rows[7].1["reason"].as_str()),
            (Some("disable"), Some("self_service"))
        );
        let methods: Vec<String> =
            sqlx::query_scalar("SELECT new_value->>'method' FROM audit_log WHERE action = 'login.success' ORDER BY id")
                .fetch_all(pool)
                .await
                .unwrap();
        assert_eq!(methods, ["setup", "totp", "recovery_code", "password"]);
        let all: Vec<String> =
            sqlx::query_scalar("SELECT new_value::text FROM audit_log").fetch_all(pool).await.unwrap();
        let hashes =
            recovery.iter().map(|c| hex::encode(totp::recovery_code_hash(&totp::normalise_recovery_code(c).unwrap())));
        let secrets: Vec<String> =
            [totp::base32(&secret), hex::encode(&secret)].into_iter().chain(recovery.clone()).chain(hashes).collect();
        for row in &all {
            for s in &secrets {
                assert!(!row.to_lowercase().contains(&s.to_lowercase()), "{row} contains secret material");
            }
        }
        db.drop().await;
    }

    /// Wrong codes are failed sign-ins for the username: the same lock as
    /// wrong passwords, and a challenge takes only so many.
    #[tokio::test]
    async fn wrong_codes_lock_the_username_like_wrong_passwords() {
        let Some(db) = scratch::database("wrong_codes_lock_the_username_like_wrong_passwords").await else { return };
        let (app, pool) = (app(db.pool.clone()), &db.pool);
        let (session, _) = setup(&app).await;
        let step = settled_step().await;
        let (secret, _) = enrol(&app, pool, &session, step).await;

        // Three wrong codes on one challenge, two on the next: the fifth locks the username.
        let first = password_step(&app).await;
        for _ in 0..3 {
            let (status, v, _) = second_step(&app, &first, "000000").await;
            assert_eq!((status, code(&v)), (401, "UNAUTHENTICATED"));
        }
        let challenge = password_step(&app).await;
        for _ in 3..crate::auth::throttle::FREE_FAILURES {
            let (status, v, _) = second_step(&app, &challenge, "000000").await;
            assert_eq!((status, code(&v)), (401, "UNAUTHENTICATED"));
        }
        let (status, v, _) = second_step(&app, &challenge, &totp::code_at(&secret, step)).await;
        assert_eq!((status, code(&v)), (429, "RATE_LIMITED"), "locked: not even the right code is checked");
        let body = json!({ "username": "owner", "password": PASSWORD });
        let (status, _, _) = call(&app, "POST", "/api/v1/auth/login", &Creds::default(), Some(body)).await;
        assert_eq!(status, 429, "the password step shares the lock");
        let locked: Vec<Value> = sqlx::query_scalar("SELECT new_value FROM audit_log WHERE action = 'login.locked'")
            .fetch_all(pool)
            .await
            .unwrap();
        assert_eq!((locked.len(), locked[0]["attemptedUsername"].as_str()), (1, Some("owner")));

        // Once the lock has passed the right code signs in; a challenge takes
        // at most five wrong codes, then the password is asked for again.
        tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
        let (status, _, _) = second_step(&app, &challenge, &totp::code_at(&secret, step)).await;
        assert_eq!(status, 200);
        let spent = password_step(&app).await;
        for _ in 0..5 {
            second_step(&app, &spent, "000000").await;
        }
        let (status, v, _) = second_step(&app, &spent, &totp::code_at(&secret, step + 1)).await;
        let expired = v["error"]["message"].as_str().is_some_and(|m| m.contains("expired"));
        assert_eq!((status, expired), (401, true), "the fifth wrong code drops the challenge: {v}");
        db.drop().await;
    }

    /// A second-factor step started before an administrator resets the
    /// password or disables the account is refused afterwards, even with the
    /// right code and the account enabled again (GH#191).
    #[tokio::test]
    async fn a_reset_or_disable_drops_pending_second_factor_steps() {
        let Some(db) = scratch::database("a_reset_or_disable_drops_pending_second_factor_steps").await else { return };
        let (app, pool) = (app(db.pool.clone()), &db.pool);
        let (session, _) = setup(&app).await;
        let step = settled_step().await;
        let (secret, _) = enrol(&app, pool, &session, step).await;
        let administrators: Uuid =
            sqlx::query_scalar("SELECT id FROM permission_profiles WHERE name = 'Administrator'")
                .fetch_one(pool)
                .await
                .unwrap();
        let body = json!({ "username": "second", "displayName": "Second", "password": PASSWORD,
            "profileIds": [administrators] });
        let (status, v, _) = call(&app, "POST", "/api/v1/admin/users", &session, Some(body)).await;
        assert_eq!(status, 201, "{v}");
        let body = json!({ "username": "second", "password": PASSWORD });
        let (status, me, headers) = call(&app, "POST", "/api/v1/auth/login", &Creds::default(), Some(body)).await;
        assert_eq!(status, 200, "{me}");
        let second = session_of(&me, &headers);
        let owner: Uuid =
            sqlx::query_scalar("SELECT id FROM users WHERE username = 'owner'").fetch_one(pool).await.unwrap();
        let user = format!("/api/v1/admin/users/{owner}");
        let pending = || {
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM mfa_challenges WHERE user_id = $1")
                .bind(owner)
                .fetch_one(pool)
        };

        // Disabled and enabled again while the second step is pending.
        let challenge = password_step(&app).await;
        assert_eq!(pending().await.unwrap(), 1);
        let (status, v, _) = call(&app, "PATCH", &user, &second, Some(json!({ "isActive": false }))).await;
        assert_eq!(status, 200, "{v}");
        assert_eq!(pending().await.unwrap(), 0);
        let (status, v, _) = call(&app, "PATCH", &user, &second, Some(json!({ "isActive": true }))).await;
        assert_eq!(status, 200, "{v}");
        let (status, v, _) = second_step(&app, &challenge, &totp::code_at(&secret, step)).await;
        assert_eq!((status, code(&v)), (401, "UNAUTHENTICATED"), "{v}");

        // Reset (to the same password, so the test can sign in again) while it is pending.
        let challenge = password_step(&app).await;
        let reset = json!({ "password": PASSWORD });
        let (status, v, _) = call(&app, "PUT", &format!("{user}/password"), &second, Some(reset)).await;
        assert_eq!(status, 200, "{v}");
        assert_eq!(pending().await.unwrap(), 0);
        let (status, v, _) = second_step(&app, &challenge, &totp::code_at(&secret, step)).await;
        assert_eq!((status, code(&v)), (401, "UNAUTHENTICATED"), "{v}");

        // A step started afterwards goes through with the same code.
        let challenge = password_step(&app).await;
        let (status, v, _) = second_step(&app, &challenge, &totp::code_at(&secret, step)).await;
        assert_eq!(status, 200, "{v}");
        db.drop().await;
    }

    /// With the session and the password, guessing the code to turn MFA off
    /// or get new recovery codes locks like guessing the password: a right
    /// password does not clear the count, not even on a password-only
    /// endpoint in between (GH#141).
    #[tokio::test]
    async fn right_password_wrong_code_locks_reauthentication() {
        let Some(db) = scratch::database("right_password_wrong_code_locks_reauthentication").await else { return };
        let (app, pool) = (app(db.pool.clone()), &db.pool);
        let (session, _) = setup(&app).await;
        let step = settled_step().await;
        let (secret, _) = enrol(&app, pool, &session, step).await;
        let with = |c: &str| json!({ "currentPassword": PASSWORD, "code": c });
        let password_only = json!({ "currentPassword": PASSWORD });

        for i in 0..crate::auth::throttle::FREE_FAILURES {
            let (method, path) = match i % 2 {
                0 => ("DELETE", "/api/v1/auth/mfa/totp"),
                _ => ("POST", "/api/v1/auth/mfa/recovery-codes"),
            };
            let (status, v, _) =
                call(&app, "POST", "/api/v1/auth/mfa/totp", &session, Some(password_only.clone())).await;
            assert_eq!((status, code(&v)), (409, "CONFLICT"), "{i}: already set up: {v}");
            let (status, v, _) = call(&app, method, path, &session, Some(with("000000"))).await;
            assert_eq!((status, v["error"]["details"][0]["field"].as_str()), (400, Some("code")), "{i}: {v}");
        }
        let right = totp::code_at(&secret, step);
        let (status, v, _) = call(&app, "DELETE", "/api/v1/auth/mfa/totp", &session, Some(with(&right))).await;
        assert_eq!((status, code(&v)), (429, "RATE_LIMITED"), "locked: not even the right code is checked");
        let (status, _, _) = call(&app, "POST", "/api/v1/auth/mfa/totp", &session, Some(password_only)).await;
        assert_eq!(status, 429, "the password-only endpoints share the lock");

        // Once the lock has passed, the right password and code go through.
        tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
        let (status, v, _) = call(&app, "DELETE", "/api/v1/auth/mfa/totp", &session, Some(with(&right))).await;
        assert_eq!(status, 204, "{v}");
        db.drop().await;
    }

    /// A profile that requires MFA limits its holders' sessions to setting it
    /// up; an administrator's reset puts them back there.
    #[tokio::test]
    async fn a_profile_can_require_mfa() {
        let Some(db) = scratch::database("a_profile_can_require_mfa").await else { return };
        let (app, pool) = (app(db.pool.clone()), &db.pool);
        let (session, me) = setup(&app).await;
        let owner = me["user"]["id"].as_str().unwrap().to_owned();
        let admin_profile = me["user"]["profiles"][0]["id"].as_str().unwrap().to_owned();
        let path = format!("/api/v1/admin/profiles/{admin_profile}");

        let (status, _, _) = call(&app, "PATCH", &path, &session, Some(json!({ "name": "Renamed" }))).await;
        assert_eq!(status, 409, "the built-in profile stays read-only");
        let (status, v, _) = call(&app, "PATCH", &path, &session, Some(json!({ "requireMfa": true }))).await;
        assert_eq!((status, &v["requireMfa"]), (200, &json!(true)), "{v}");

        let (status, v, _) = call(&app, "GET", "/api/v1/admin/users", &session, None).await;
        assert_eq!((status, code(&v)), (403, "MFA_ENROLMENT_REQUIRED"));
        let (status, v, _) = call(&app, "GET", "/api/v1/auth/me", &session, None).await;
        assert_eq!((status, &v["mfa"]["enrolmentRequired"]), (200, &json!(true)));
        let step = settled_step().await;
        enrol(&app, pool, &session, step).await;
        let (status, v, _) = call(&app, "GET", "/api/v1/admin/users", &session, None).await;
        assert_eq!((status, &v["data"][0]["mfaEnabled"]), (200, &json!(true)));

        // Reset by an administrator (here: themselves): back to enrolment.
        let reset = format!("/api/v1/admin/users/{owner}/mfa");
        let (status, _, _) = call(&app, "DELETE", &reset, &session, None).await;
        assert_eq!(status, 204);
        let (status, v, _) = call(&app, "GET", "/api/v1/admin/users", &session, None).await;
        assert_eq!((status, code(&v)), (403, "MFA_ENROLMENT_REQUIRED"));
        let rows = mfa_rows(pool).await;
        let last = rows.last().unwrap();
        assert_eq!((last.0.as_str(), last.1["reason"].as_str()), ("mfa.disable", Some("admin_reset")));
        let (status, _, _) = call(&app, "DELETE", &reset, &session, None).await;
        assert_eq!(status, 403, "the reset route is not an enrolment route");
        db.drop().await;
    }

    /// GH#405: a clone of the built-in profile spells out every permission but
    /// is not `administrator`, so its holder cannot change requireMfa on the
    /// built-in profile; an administrator can.
    #[tokio::test]
    async fn only_an_administrator_changes_the_builtin_require_mfa() {
        let Some(db) = scratch::database("only_an_administrator_changes_the_builtin_require_mfa").await else {
            return;
        };
        let app = app(db.pool.clone());
        let (session, me) = setup(&app).await;
        let admin_profile = me["user"]["profiles"][0]["id"].as_str().unwrap().to_owned();
        let path = format!("/api/v1/admin/profiles/{admin_profile}");
        let clone = json!({ "name": "Delegated admin" });
        let (status, v, _) = call(&app, "POST", &format!("{path}/clone"), &session, Some(clone)).await;
        assert_eq!(status, 201, "{v}");
        let body = json!({ "username": "delegate", "displayName": "Delegate", "password": PASSWORD,
            "profileIds": [v["id"]] });
        let (status, v, _) = call(&app, "POST", "/api/v1/admin/users", &session, Some(body)).await;
        assert_eq!(status, 201, "{v}");
        let body = json!({ "username": "delegate", "password": PASSWORD });
        let (status, me, headers) = call(&app, "POST", "/api/v1/auth/login", &Creds::default(), Some(body)).await;
        assert_eq!(status, 200, "{me}");
        let delegate = session_of(&me, &headers);

        for require in [true, false] {
            let body = Some(json!({ "requireMfa": require }));
            let (status, v, _) = call(&app, "PATCH", &path, &delegate, body).await;
            assert_eq!((status, code(&v)), (403, "FORBIDDEN"), "{v}");
        }
        let (status, v, _) = call(&app, "GET", &path, &session, None).await;
        assert_eq!((status, &v["requireMfa"]), (200, &json!(false)), "unchanged: {v}");
        let (status, v, _) = call(&app, "DELETE", &path, &delegate, None).await;
        assert_eq!(status, 409, "{v}");

        let (status, v, _) = call(&app, "PATCH", &path, &session, Some(json!({ "requireMfa": true }))).await;
        assert_eq!((status, &v["requireMfa"]), (200, &json!(true)), "{v}");
        db.drop().await;
    }

    /// GH#303: a password sign-in and the confirming of an authenticator
    /// serialize on the user's row, so no password-only session outlives the
    /// confirm, whichever of the two takes the row first.
    #[tokio::test]
    async fn a_password_sign_in_racing_a_confirm_gets_no_password_only_session() {
        let Some(db) = scratch::database("a_password_sign_in_racing_a_confirm_gets_no_password_only_session").await
        else {
            return;
        };
        let (app, pool) = (app(db.pool.clone()), &db.pool);
        let (session, me) = setup(&app).await;
        let owner: Uuid = me["user"]["id"].as_str().unwrap().parse().unwrap();
        let own: Uuid = sqlx::query_scalar("SELECT id FROM sessions").fetch_one(pool).await.unwrap();
        let unverified = async || -> i64 {
            sqlx::query_scalar("SELECT count(*) FROM sessions WHERE NOT mfa_verified").fetch_one(pool).await.unwrap()
        };
        let start = json!({ "currentPassword": PASSWORD });
        let step = settled_step().await;

        // The sign-in locks the row first (its session not yet committed):
        // the confirm waits for it, then ends the session.
        let (status, _, _) = call(&app, "POST", "/api/v1/auth/mfa/totp", &session, Some(start.clone())).await;
        assert_eq!(status, 201);
        let secret = stored_seed(pool).await;
        let mut signing_in = pool.begin().await.unwrap();
        auth_data::record_login(&mut signing_in, owner).await.unwrap().expect("the owner");
        let body = json!({ "code": totp::code_at(&secret, step) });
        let ((status, v, _), ()) =
            tokio::join!(call(&app, "POST", "/api/v1/auth/mfa/totp/confirm", &session, Some(body)), async move {
                let awaited = crate::modules::auth::tests::a_lock_is_awaited(pool).await;
                assert!(awaited, "the confirm waits for the sign-in's lock on the user's row");
                let hash = crate::auth::session::token_hash("racing sign-in");
                let max_age = std::time::Duration::from_secs(600);
                auth_data::create_session(&mut signing_in, owner, &hash, "csrf", max_age, None, None, false)
                    .await
                    .unwrap();
                signing_in.commit().await.unwrap();
            });
        assert_eq!(status, 200, "{v}");
        assert_eq!(unverified().await, 0, "the password-only session committed first is ended");
        let recovery = v["codes"][0].as_str().unwrap().to_owned();

        // The confirm locks the row first: a sign-in that found no
        // authenticator waits for it, then gets no session but a 401.
        let off = json!({ "currentPassword": PASSWORD, "code": recovery });
        let (status, v, _) = call(&app, "DELETE", "/api/v1/auth/mfa/totp", &session, Some(off)).await;
        assert_eq!(status, 204, "{v}");
        let (status, _, _) = call(&app, "POST", "/api/v1/auth/mfa/totp", &session, Some(start)).await;
        assert_eq!(status, 201);
        let mut confirming = pool.begin().await.unwrap();
        auth_data::lock_sign_in(&mut confirming, owner).await.unwrap().expect("the owner");
        let (password, anonymous) = (json!({ "username": "owner", "password": PASSWORD }), Creds::default());
        let ((status, v, _), ()) =
            tokio::join!(call(&app, "POST", "/api/v1/auth/login", &anonymous, Some(password)), async move {
                let awaited = crate::modules::auth::tests::a_lock_is_awaited(pool).await;
                assert!(awaited, "the sign-in waits for the confirm's lock on the user's row");
                data::confirm_totp(&mut confirming, owner, step).await.unwrap();
                auth_data::mark_session_mfa_verified(&mut confirming, own).await.unwrap();
                auth_data::delete_unverified_sessions(&mut confirming, owner, Some(own)).await.unwrap();
                confirming.commit().await.unwrap();
            });
        assert_eq!((status, code(&v)), (401, "UNAUTHENTICATED"), "{v}");
        assert!(v["error"]["message"].as_str().unwrap().contains("Two-factor authentication was set up"), "{v}");
        assert_eq!(unverified().await, 0, "no password-only session after the confirm");
        let reason: Option<String> = sqlx::query_scalar(
            "SELECT new_value->>'reason' FROM audit_log WHERE action = 'login.failure' ORDER BY id DESC LIMIT 1",
        )
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(reason.as_deref(), Some("mfa_enrolled"));
        // Signing in again asks for the code.
        password_step(&app).await;
        db.drop().await;
    }

    /// GH#280: under requireMfa the gate follows what the session proved, not
    /// the account's current state. A password-only session ends when the user
    /// sets up MFA in another one (GH#292); a reset or turning MFA off ends the
    /// user's other sessions, so none carries over to a later enrolment.
    #[tokio::test]
    async fn require_mfa_gates_by_what_the_session_proved() {
        let Some(db) = scratch::database("require_mfa_gates_by_what_the_session_proved").await else { return };
        let (app, pool) = (app(db.pool.clone()), &db.pool);
        let (session, me) = setup(&app).await;
        let owner = me["user"]["id"].as_str().unwrap().to_owned();
        let admin_profile = me["user"]["profiles"][0]["id"].as_str().unwrap().to_owned();
        let path = format!("/api/v1/admin/profiles/{admin_profile}");
        let (status, v, _) = call(&app, "PATCH", &path, &session, Some(json!({ "requireMfa": true }))).await;
        assert_eq!(status, 200, "{v}");
        let gated = |creds: Creds| {
            let app = app.clone();
            async move {
                // A CI read and an admin read get the same answer.
                let (status, v, _) = call(&app, "GET", "/api/v1/configuration-items", &creds, None).await;
                let (admin_status, admin_v, _) = call(&app, "GET", "/api/v1/admin/users", &creds, None).await;
                assert_eq!((status, code(&v)), (admin_status, code(&admin_v)), "{v} / {admin_v}");
                (status, code(&v).to_owned())
            }
        };
        let enrolment_required = (403, "MFA_ENROLMENT_REQUIRED".to_owned());
        let password = json!({ "username": "owner", "password": PASSWORD });

        // Session A: the password alone (an attacker's phished password).
        let (status, me_a, headers) = call(&app, "POST", "/api/v1/auth/login", &Creds::default(), Some(password)).await;
        assert_eq!(status, 200, "{me_a}");
        let a = session_of(&me_a, &headers);
        assert_eq!(gated(a.clone()).await, enrolment_required);

        // The real user enrols in their own session, which gets full access ...
        let step = settled_step().await;
        let (secret, _) = enrol(&app, pool, &session, step).await;
        assert_eq!(gated(session.clone()).await.0, 200, "the enrolling session");
        let (status, v, _) = call(&app, "GET", "/api/v1/auth/me", &session, None).await;
        assert_eq!((status, &v["mfa"]["enrolmentRequired"]), (200, &json!(false)));
        // ... and session A ends (GH#292): it can neither use the enrolment
        // routes nor change the password, which would end the user's session.
        let (status, v, _) = call(&app, "GET", "/api/v1/auth/me", &a, None).await;
        assert_eq!((status, code(&v)), (401, "UNAUTHENTICATED"), "session A after enrolment elsewhere");
        let change = json!({ "currentPassword": PASSWORD, "newPassword": "a password the attacker chose" });
        let (status, v, _) = call(&app, "PUT", "/api/v1/auth/password", &a, Some(change)).await;
        assert_eq!((status, code(&v)), (401, "UNAUTHENTICATED"), "password change from session A");
        assert_eq!(gated(session.clone()).await.0, 200, "the enrolling session after A's attempt");

        // A sign-in with a code gets full access.
        let challenge = password_step(&app).await;
        let (status, me_c, headers) = second_step(&app, &challenge, &totp::code_at(&secret, step)).await;
        assert_eq!(status, 200, "{me_c}");
        let c = session_of(&me_c, &headers);
        assert_eq!(gated(c.clone()).await.0, 200);
        // A session that did not prove the authenticator (none is left by the
        // API; e.g. one opened before an upgrade) stays limited.
        let latest = "(SELECT id FROM sessions ORDER BY created_at DESC LIMIT 1)";
        let unverify = format!("UPDATE sessions SET mfa_verified = false WHERE id = {latest}");
        sqlx::query(sqlx::AssertSqlSafe(unverify)).execute(pool).await.unwrap();
        assert_eq!(gated(c.clone()).await, enrolment_required, "an unproven session C");
        let (status, v, _) = call(&app, "GET", "/api/v1/auth/me", &c, None).await;
        assert_eq!(
            (status, &v["mfa"]["totpEnabled"], &v["mfa"]["enrolmentRequired"]),
            (200, &json!(true), &json!(true))
        );
        let (status, v, _) = call(&app, "GET", "/api/v1/admin/users", &c, None).await;
        assert!(v["error"]["message"].as_str().unwrap().contains("sign in again with a code"), "{status}: {v}");
        let verify = format!("UPDATE sessions SET mfa_verified = true WHERE id = {latest}");
        sqlx::query(sqlx::AssertSqlSafe(verify)).execute(pool).await.unwrap();

        // An administrator's reset (here: the owner themselves) ends every
        // other session; the caller's own is limited to enrolment again.
        let (status, _, _) = call(&app, "DELETE", &format!("/api/v1/admin/users/{owner}/mfa"), &session, None).await;
        assert_eq!(status, 204);
        let (status, v, _) = call(&app, "GET", "/api/v1/auth/me", &c, None).await;
        assert_eq!((status, code(&v)), (401, "UNAUTHENTICATED"), "session C after the reset");
        assert_eq!(gated(session.clone()).await, enrolment_required, "the caller's session after the reset");
        // Setting it up again brings no ended session back.
        let (secret, _) = enrol(&app, pool, &session, step).await;
        assert_eq!(gated(session.clone()).await.0, 200);
        let (status, _, _) = call(&app, "GET", "/api/v1/auth/me", &c, None).await;
        assert_eq!(status, 401, "session C after re-enrolment");

        // Turning MFA off ends the other sessions too; this one is limited to enrolment.
        let challenge = password_step(&app).await;
        let (status, me_d, headers) = second_step(&app, &challenge, &totp::code_at(&secret, step)).await;
        assert_eq!(status, 200, "{me_d}");
        let d = session_of(&me_d, &headers);
        let off = json!({ "currentPassword": PASSWORD, "code": totp::code_at(&secret, step + 1) });
        let (status, v, _) = call(&app, "DELETE", "/api/v1/auth/mfa/totp", &session, Some(off)).await;
        assert_eq!(status, 204, "{v}");
        let (status, _, _) = call(&app, "GET", "/api/v1/auth/me", &d, None).await;
        assert_eq!(status, 401, "session D after MFA was turned off");
        assert_eq!(gated(session.clone()).await, enrolment_required, "the disabling session");

        let reasons: Vec<String> = sqlx::query_scalar(
            "SELECT new_value->>'reason' FROM audit_log WHERE action = 'session.revoke' ORDER BY id",
        )
        .fetch_all(pool)
        .await
        .unwrap();
        assert_eq!(reasons, ["mfa_enrolled", "mfa_reset", "mfa_disabled"]);
        db.drop().await;
    }

    /// An identity provider row of `kind` (`ldap` or `oidc`). Nothing is
    /// ever contacted at `ldap_url`, except by tests that expect a refusal.
    pub(crate) async fn provider(pool: &PgPool, kind: &str, enabled: bool, ldap_url: &str) -> Uuid {
        let sql = if kind == "ldap" {
            "INSERT INTO identity_providers (kind, name, is_enabled, ldap_url, start_tls, user_base_dn, user_filter,
               username_attribute, display_name_attribute, email_attribute, group_attribute)
             VALUES ('ldap', 'Directory ' || gen_random_uuid(), $1, $2, false, 'DC=example,DC=test',
               '(uid={username})', 'uid', 'cn', 'mail', 'memberOf') RETURNING id"
        } else {
            "INSERT INTO identity_providers (kind, name, is_enabled, issuer_url, client_id, scopes, username_claim,
               groups_claim, mfa_assurance, required_acr)
             VALUES ('oidc', 'Company SSO ' || gen_random_uuid(), $1, 'https://sso.example.test', 'cmdb', 'openid',
               'preferred_username', 'groups', 'trust_provider', '{}') RETURNING id"
        };
        let q = sqlx::query_scalar(sql).bind(enabled);
        let q = if kind == "ldap" { q.bind(ldap_url) } else { q };
        q.fetch_one(pool).await.unwrap()
    }

    /// A user holding a profile that requires MFA, local (with PASSWORD) or
    /// linked to `linked`, with an open session; returns the credentials to
    /// call the API with.
    async fn mfa_required_user(pool: &PgPool, name: &str, linked: Option<Uuid>) -> Creds {
        let profile: Uuid =
            sqlx::query_scalar("INSERT INTO permission_profiles (name, require_mfa) VALUES ($1, true) RETURNING id")
                .bind(format!("MFA required for {name}"))
                .fetch_one(pool)
                .await
                .unwrap();
        let system = RequestContext::system("test", "test");
        let user_id = match linked {
            None => {
                let input = users::UserCreate {
                    username: name.into(),
                    display_name: name.into(),
                    email: None,
                    password: PASSWORD.into(),
                    is_active: Some(true),
                    profile_ids: vec![profile],
                };
                users::create(pool, &system, &input).await.unwrap().id
            }
            Some(provider_id) => {
                let mut tx = pool.begin().await.unwrap();
                let new = crate::data::identity_providers::NewLinkedUser {
                    provider_id,
                    external_id: &format!("entry-{name}"),
                    username: name,
                    display_name: name,
                    email: None,
                };
                let id = crate::data::identity_providers::insert_linked(&mut tx, &new).await.unwrap();
                auth_data::set_user_profiles(&mut tx, id, &[profile]).await.unwrap();
                tx.commit().await.unwrap();
                id
            }
        };
        let auth = AuthState::new(
            crate::config::AuthConfig {
                session_idle: std::time::Duration::from_secs(3600),
                session_max_age: std::time::Duration::from_secs(3600),
                cookie_secure: crate::config::CookieSecure::Never,
                public_url: None,
                oidc_allowed_hosts: None,
                setup_token: Some(crate::auth::setup_token::TEST_TOKEN.into()),
                setup_token_file: None,
                trusted_proxies: Default::default(),
                sign_in_failure_floor: std::time::Duration::ZERO,
            },
            crate::secrets::Keyring::for_tests(),
        );
        let (_, cookies) = super::super::auth::open_session(
            pool,
            &auth,
            &HeaderMap::new(),
            &system,
            user_id,
            name,
            LoginMethod::Ldap,
            None,
        )
        .await
        .unwrap();
        let token = crate::auth::session::cookie_value(&cookies[0], crate::auth::session::SESSION_COOKIE).unwrap();
        let csrf = crate::auth::session::cookie_value(&cookies[1], crate::auth::session::CSRF_COOKIE).unwrap();
        let cookie = format!("shadoucmdb_session={token}; shadoucmdb_csrf={csrf}");
        Creds { cookie: Some(cookie), csrf: Some(csrf), bearer: None }
    }

    /// GH#120: requireMfa covers local and directory (LDAP) accounts; OIDC
    /// accounts of a provider trusted to enforce MFA are left to it (GH#131
    /// covers verifying providers). The per-request gate and /auth/me agree
    /// for all three.
    #[tokio::test]
    async fn require_mfa_covers_directory_accounts_but_not_oidc() {
        let Some(db) = scratch::database("require_mfa_covers_directory_accounts_but_not_oidc").await else { return };
        let (app, pool) = (app(db.pool.clone()), &db.pool);
        let ldap = provider(pool, "ldap", true, "ldaps://dc.example.test").await;
        let oidc = provider(pool, "oidc", true, "").await;
        for (name, linked, gated) in [("lena", None, true), ("dirk", Some(ldap), true), ("olga", Some(oidc), false)] {
            let creds = mfa_required_user(pool, name, linked).await;
            let idle = std::time::Duration::from_secs(3600);
            // The stored hash of the session just opened for this user.
            let hash: Vec<u8> = sqlx::query_scalar(
                "SELECT s.token_hash FROM sessions s JOIN users u ON u.id = s.user_id WHERE u.username = $1",
            )
            .bind(name)
            .fetch_one(pool)
            .await
            .unwrap();
            let live = auth_data::resolve_session(pool, &hash, idle).await.unwrap().expect("live session");
            assert_eq!(live.mfa_enrolment_required, gated, "{name}: resolve_session");
            // The profile grants nothing: past the gate the answer is FORBIDDEN.
            let (status, v, _) = call(&app, "GET", "/api/v1/admin/users", &creds, None).await;
            let expected = if gated { "MFA_ENROLMENT_REQUIRED" } else { "FORBIDDEN" };
            assert_eq!((status, code(&v)), (403, expected), "{name}: {v}");
            let (status, v, _) = call(&app, "GET", "/api/v1/auth/me", &creds, None).await;
            assert_eq!(
                (status, &v["mfa"]["required"], &v["mfa"]["enrolmentRequired"]),
                (200, &json!(gated), &json!(gated)),
                "{name}: {v}"
            );
        }
        db.drop().await;
    }

    /// Setting up MFA confirms the password first. An OIDC account has none
    /// here: 409. A directory that cannot be reached: 503. A session left from
    /// a directory disabled since is no session at all (GH#250): 401. Nothing
    /// is set up in any case.
    #[tokio::test]
    async fn enrolment_needs_a_password_that_can_be_confirmed() {
        let Some(db) = scratch::database("enrolment_needs_a_password_that_can_be_confirmed").await else { return };
        let (app, pool) = (app(db.pool.clone()), &db.pool);
        let oidc = provider(pool, "oidc", true, "").await;
        let disabled = provider(pool, "ldap", true, "ldaps://dc.example.test").await;
        // Nothing listens on port 1: the connection is refused at once.
        let unreachable = provider(pool, "ldap", true, "ldaps://127.0.0.1:1").await;
        let body = json!({ "currentPassword": PASSWORD });
        for (name, linked, status_code, error, says) in [
            ("olga", oidc, 409, "CONFLICT", "identity provider"),
            ("dirk", disabled, 401, "UNAUTHENTICATED", "Sign in"),
            ("dana", unreachable, 503, "IDENTITY_PROVIDER_UNAVAILABLE", "could not be reached"),
        ] {
            let creds = mfa_required_user(pool, name, Some(linked)).await;
            if linked == disabled {
                // Its sessions are not ended here, as if one were left over.
                sqlx::query("UPDATE identity_providers SET is_enabled = false WHERE id = $1")
                    .bind(linked)
                    .execute(pool)
                    .await
                    .unwrap();
            }
            let (status, v, _) = call(&app, "POST", "/api/v1/auth/mfa/totp", &creds, Some(body.clone())).await;
            assert_eq!((status, code(&v)), (status_code, error), "{name}: {v}");
            assert!(v["error"]["message"].as_str().unwrap().contains(says), "{name}: {v}");
        }
        let set_up: i64 = sqlx::query_scalar("SELECT count(*) FROM user_totp").fetch_one(pool).await.unwrap();
        assert_eq!(set_up, 0);
        db.drop().await;
    }

    /// A profile granting only `audit.view`, the scope of the tokens below.
    async fn readers_profile(pool: &PgPool) -> Uuid {
        let id: Uuid = sqlx::query_scalar("INSERT INTO permission_profiles (name) VALUES ('Readers') RETURNING id")
            .fetch_one(pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO permission_profile_global_permissions (profile_id, permission) VALUES ($1, 'audit.view')",
        )
        .bind(id)
        .execute(pool)
        .await
        .unwrap();
        id
    }

    /// A profile that requires MFA and grants nothing.
    async fn strict_profile(pool: &PgPool) -> Uuid {
        sqlx::query_scalar("INSERT INTO permission_profiles (name, require_mfa) VALUES ('Strict', true) RETURNING id")
            .fetch_one(pool)
            .await
            .unwrap()
    }

    /// Creates a token through the API: (status, response).
    async fn mint(app: &Router, session: &Creds, owner: Option<&str>, scope: Uuid, name: &str) -> (u16, Value) {
        let expires = (chrono::Utc::now() + chrono::Duration::days(30)).to_rfc3339();
        let mut body = json!({ "name": name, "profileId": scope, "expiresAt": expires });
        if let Some(owner) = owner {
            body["userId"] = json!(owner);
        }
        let (status, v, _) = call(app, "POST", "/api/v1/admin/api-tokens", session, Some(body)).await;
        (status, v)
    }

    fn bearer(created: &Value) -> Creds {
        Creds { bearer: created["secret"].as_str().map(str::to_owned), ..Creds::default() }
    }

    async fn use_token(app: &Router, token: &Creds) -> (u16, Value) {
        let (status, v, _) = call(app, "GET", "/api/v1/audit-log?limit=1", token, None).await;
        (status, v)
    }

    async fn last_outcome(pool: &PgPool) -> Option<String> {
        sqlx::query_scalar(
            "SELECT new_value->>'outcome' FROM audit_log WHERE action = 'token.use' ORDER BY id DESC LIMIT 1",
        )
        .fetch_one(pool)
        .await
        .unwrap()
    }

    /// GH#200: a token follows its owner's requireMfa at every use. A token
    /// created without a second factor is refused while a profile of the
    /// owner requires MFA (even after the owner enrols), and works again when
    /// the requirement goes; tokens created from a session that proved a
    /// second factor (enrolment confirmed in it, or /auth/login/mfa) work.
    #[tokio::test]
    async fn api_tokens_follow_their_owners_require_mfa() {
        let Some(db) = scratch::database("api_tokens_follow_their_owners_require_mfa").await else { return };
        let (app, pool) = (app(db.pool.clone()), &db.pool);
        let (session, me) = setup(&app).await;
        let owner = me["user"]["id"].as_str().unwrap().to_owned();
        let readers = readers_profile(pool).await;

        // Password-only session, no policy yet: the token works.
        let (status, a) = mint(&app, &session, None, readers, "old script").await;
        assert_eq!(status, 201, "{a}");
        assert_eq!((&a["token"]["mfaVerified"], &a["token"]["refusedForMfa"]), (&json!(false), &json!(false)));
        let old = bearer(&a);
        assert_eq!(use_token(&app, &old).await.0, 200);
        let (status, b) = mint(&app, &session, None, readers, "revoked script").await;
        assert_eq!(status, 201, "{b}");
        let path = format!("/api/v1/admin/api-tokens/{}", b["token"]["id"].as_str().unwrap());
        assert_eq!(call(&app, "DELETE", &path, &session, None).await.0, 204);

        // A profile of the owner now requires MFA: refused, audited, not "used".
        let strict = strict_profile(pool).await;
        sqlx::query("INSERT INTO user_permission_profiles (user_id, profile_id) VALUES ($1::uuid, $2)")
            .bind(&owner)
            .bind(strict)
            .execute(pool)
            .await
            .unwrap();
        let used_before: Option<chrono::DateTime<chrono::Utc>> =
            sqlx::query_scalar("SELECT last_used_at FROM api_tokens WHERE name = 'old script'")
                .fetch_one(pool)
                .await
                .unwrap();
        let (status, v) = use_token(&app, &old).await;
        assert_eq!((status, code(&v)), (401, "UNAUTHENTICATED"));
        assert!(v["error"]["message"].as_str().unwrap().contains("must use two-factor authentication"), "{v}");
        assert_eq!(last_outcome(pool).await.as_deref(), Some("mfa_required"));
        let used_after: Option<chrono::DateTime<chrono::Utc>> =
            sqlx::query_scalar("SELECT last_used_at FROM api_tokens WHERE name = 'old script'")
                .fetch_one(pool)
                .await
                .unwrap();
        assert_eq!(used_before, used_after, "a refused use is not a use");
        // A revoked token reports that it is revoked, not the MFA requirement.
        let (status, v) = use_token(&app, &bearer(&b)).await;
        assert_eq!((status, v["error"]["message"].as_str()), (401, Some("This API token has been revoked")));
        assert_eq!(last_outcome(pool).await.as_deref(), Some("revoked"));

        // Enrolling does not revive the old token; confirming it in this
        // session proves the second factor, so a new token works.
        let step = settled_step().await;
        let (secret, _) = enrol(&app, pool, &session, step).await;
        assert_eq!(use_token(&app, &old).await.0, 401);
        let (status, c) = mint(&app, &session, None, readers, "after enrolment").await;
        assert_eq!(status, 201, "{c}");
        assert_eq!((&c["token"]["mfaVerified"], &c["token"]["refusedForMfa"]), (&json!(true), &json!(false)));
        assert_eq!(use_token(&app, &bearer(&c)).await.0, 200);

        // Administrators find the refused tokens; migrate counts the same ones.
        let (status, v, _) = call(&app, "GET", "/api/v1/admin/api-tokens?refusedForMfa=true", &session, None).await;
        assert_eq!((status, v["page"]["total"].as_i64()), (200, Some(1)), "{v}");
        assert_eq!((v["data"][0]["name"].as_str(), &v["data"][0]["refusedForMfa"]), (Some("old script"), &json!(true)));
        let (status, v, _) = call(&app, "GET", "/api/v1/admin/api-tokens?refusedForMfa=false", &session, None).await;
        assert_eq!((status, v["page"]["total"].as_i64()), (200, Some(2)), "{v}");
        let mut conn = pool.acquire().await.unwrap();
        assert_eq!(crate::data::api_tokens::count_second_factor_refusals(&mut conn).await.unwrap(), (1, 1));
        let notice = crate::data::api_tokens::second_factor_refusal_notice(&mut conn).await.unwrap().unwrap();
        assert!(notice.starts_with("1 API token of 1 account is refused"), "{notice}");
        assert!(notice.contains("Administration › API tokens"), "{notice}");
        assert!(notice.contains("/admin/api-tokens?refusedForMfa=true)"), "{notice}");
        assert!(notice.contains("GET /api/v1/admin/api-tokens?refusedForMfa=true"), "{notice}");
        drop(conn);

        // A sign-in through /auth/login/mfa is a verified session too.
        let challenge = password_step(&app).await;
        let (status, me, headers) = second_step(&app, &challenge, &totp::code_at(&secret, step)).await;
        assert_eq!(status, 200, "{me}");
        let (status, d) = mint(&app, &session_of(&me, &headers), None, readers, "after mfa sign-in").await;
        assert_eq!((status, &d["token"]["mfaVerified"]), (201, &json!(true)), "{d}");
        assert_eq!(use_token(&app, &bearer(&d)).await.0, 200);
        let created: Vec<Option<bool>> = sqlx::query_scalar(
            "SELECT (new_value->>'mfaVerified')::boolean FROM audit_log
             WHERE entity_type = 'api_tokens' AND action = 'create' ORDER BY id",
        )
        .fetch_all(pool)
        .await
        .unwrap();
        assert_eq!(created, vec![Some(false), Some(false), Some(true), Some(true)], "token.create keeps mfaVerified");

        // The rule is evaluated per request: relaxing the policy revives the token.
        sqlx::query("UPDATE permission_profiles SET require_mfa = false WHERE id = $1")
            .bind(strict)
            .execute(pool)
            .await
            .unwrap();
        assert_eq!(use_token(&app, &old).await.0, 200);
        db.drop().await;
    }

    /// GH#200, service accounts: the creating administrator's second factor
    /// counts for a token of an account under requireMfa that has none. An
    /// administrator whose session did not prove one gets 403 instead of a
    /// token that would be refused at its first use.
    #[tokio::test]
    async fn an_administrators_second_factor_vouches_for_a_service_token() {
        let Some(db) = scratch::database("an_administrators_second_factor_vouches_for_a_service_token").await else {
            return;
        };
        let (app, pool) = (app(db.pool.clone()), &db.pool);
        let (admin, _) = setup(&app).await;
        let readers = readers_profile(pool).await;
        let strict = strict_profile(pool).await;
        let svc = json!({ "username": "svc-backup", "displayName": "Backup", "password": "service account password",
            "profileIds": [strict, readers] });
        let (status, v, _) = call(&app, "POST", "/api/v1/admin/users", &admin, Some(svc)).await;
        assert_eq!(status, 201, "{v}");
        let svc = v["id"].as_str().unwrap().to_owned();

        let (status, v) = mint(&app, &admin, Some(&svc), readers, "backup").await;
        assert_eq!((status, code(&v)), (403, "MFA_REQUIRED_FOR_TOKEN"), "{v}");
        assert!(v["error"]["message"].as_str().unwrap().starts_with("svc-backup must use two-factor"), "{v}");
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM api_tokens").fetch_one(pool).await.unwrap();
        assert_eq!(count, 0);

        let step = settled_step().await;
        enrol(&app, pool, &admin, step).await;
        let (status, v) = mint(&app, &admin, Some(&svc), readers, "backup").await;
        assert_eq!(status, 201, "{v}");
        assert_eq!((&v["token"]["mfaVerified"], &v["token"]["refusedForMfa"]), (&json!(true), &json!(false)));
        assert_eq!(use_token(&app, &bearer(&v)).await.0, 200);
        db.drop().await;
    }

    /// GH#200 for OIDC accounts: under `verify` only a token created from a
    /// session whose sign-in proved MFA is accepted; `trust_provider`
    /// accepts both, and switching back refuses the unverified one again.
    #[tokio::test]
    async fn oidc_tokens_follow_the_providers_mfa_assurance() {
        let Some(db) = scratch::database("oidc_tokens_follow_the_providers_mfa_assurance").await else { return };
        let pool = &db.pool;
        let oidc = provider(pool, "oidc", true, "").await;
        mfa_required_user(pool, "olga", Some(oidc)).await;
        let (olga, scope): (Uuid, Uuid) = sqlx::query_as(
            "SELECT u.id, up.profile_id FROM users u JOIN user_permission_profiles up ON up.user_id = u.id
             WHERE u.username = 'olga'",
        )
        .fetch_one(pool)
        .await
        .unwrap();
        let mut conn = pool.acquire().await.unwrap();
        for (hash, mfa_verified) in [([1u8; 32], true), ([2u8; 32], false)] {
            let t = crate::data::api_tokens::NewToken {
                name: "sync",
                user_id: olga,
                profile_id: scope,
                token_hash: &hash,
                token_prefix: "scmdb_test",
                expires_at: chrono::Utc::now() + chrono::Duration::days(1),
                created_by: None,
                created_by_user_id: Some(olga),
                mfa_verified,
            };
            crate::data::api_tokens::insert(&mut conn, &t).await.unwrap();
        }
        drop(conn);
        let refused = |hash: [u8; 32]| async move {
            crate::data::api_tokens::find_by_hash(pool, &hash).await.unwrap().unwrap().mfa_required
        };
        for (assurance, verified_refused, unverified_refused) in [
            ("trust_provider", false, false),
            ("verify", false, true),
            ("trust_provider", false, false),
            ("verify", false, true),
        ] {
            sqlx::query("UPDATE identity_providers SET mfa_assurance = $1 WHERE id = $2")
                .bind(assurance)
                .bind(oidc)
                .execute(pool)
                .await
                .unwrap();
            assert_eq!(
                (refused([1; 32]).await, refused([2; 32]).await),
                (verified_refused, unverified_refused),
                "{assurance}"
            );
        }
        db.drop().await;
    }

    /// GH#257: disabling an identity provider stops the API tokens of its
    /// accounts as well as their sessions, until it is enabled again.
    #[tokio::test]
    async fn a_disabled_providers_accounts_tokens_are_refused() {
        let Some(db) = scratch::database("a_disabled_providers_accounts_tokens_are_refused").await else { return };
        let (app, pool) = (app(db.pool.clone()), &db.pool);
        let (admin, _) = setup(&app).await;
        let oidc = provider(pool, "oidc", true, "").await;
        let olga = mfa_required_user(pool, "olga", Some(oidc)).await;
        let readers = readers_profile(pool).await;
        let olga_id: Uuid = sqlx::query_scalar(
            "INSERT INTO user_permission_profiles (user_id, profile_id)
             SELECT id, $1 FROM users WHERE username = 'olga' RETURNING user_id",
        )
        .bind(readers)
        .fetch_one(pool)
        .await
        .unwrap();
        let (status, v) = mint(&app, &admin, Some(&olga_id.to_string()), readers, "sync").await;
        assert_eq!(status, 201, "{v}");
        let token = bearer(&v);
        assert_eq!(use_token(&app, &token).await.0, 200);

        let path = format!("/api/v1/admin/identity-providers/{oidc}");
        let (status, v, _) = call(&app, "PATCH", &path, &admin, Some(json!({ "isEnabled": false }))).await;
        assert_eq!(status, 200, "{v}");
        assert_eq!(call(&app, "GET", "/api/v1/auth/me", &olga, None).await.0, 401);
        let (status, v) = use_token(&app, &token).await;
        assert_eq!((status, code(&v)), (401, "UNAUTHENTICATED"), "{v}");
        assert_eq!(last_outcome(pool).await.as_deref(), Some("provider_disabled"));

        let (status, v, _) = call(&app, "PATCH", &path, &admin, Some(json!({ "isEnabled": true }))).await;
        assert_eq!(status, 200, "{v}");
        assert_eq!(use_token(&app, &token).await.0, 200);
        assert_eq!(last_outcome(pool).await.as_deref(), Some("accepted"));
        db.drop().await;
    }
}
