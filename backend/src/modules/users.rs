//! Administration > Users: local accounts, their passwords and profiles, and
//! the accounts identity providers created (see [`super::sso`]).
//!
//! Disabling (`isActive: false`) is the normal way to remove access; it ends
//! the user's sessions, drops their pending second-factor steps and revokes
//! their API tokens and the tokens they created for others at once. Deleting
//! is allowed too (the audit log keeps the user's id and name as text).
//! Nobody can disable or delete themselves, the database refuses any change
//! that leaves no active Administrator, and a non-administrator user manager
//! can only act on accounts, and assign profiles, whose permissions they hold
//! themselves.
//!
//! Every account has a unique e-mail (ignoring case) and is linked to a CI of
//! the built-in Person type with that e-mail ([`super::people`], SHAA-1505):
//! creating an account or changing its e-mail links or creates the Person in
//! the same transaction. Accounts from before 0044 without an e-mail sign in
//! once more and must enter one (`signInStatus: email_required`).

use std::collections::HashSet;

use axum::http::{Method, StatusCode};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{PgConnection, PgPool};
use utoipa::openapi::schema::{ArrayBuilder, KnownFormat, ObjectBuilder, Schema, SchemaFormat, Type};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use super::groups::OwnerRemoval;
use super::{api_tokens, auth, people, profiles};
use crate::api::context::{Count, RequestContext, forbidden};
use crate::api::route::{Body, Check, IdPath, In, Json, NoBody, NoPath, NoQuery, Query, Route, route};
use crate::api::schemas::{
    self, Page, Paged, QueryBool, Sort, USERNAME_PATTERN, UuidList, like_pattern, name_schema, trimmed, ts, ts_opt,
};
use crate::auth::events::{self, RevokeReason};
use crate::auth::password;
use crate::auth::permissions::GlobalPermission;
use crate::auth::secret::Secret;
use crate::data::auth::{self as data, UserRow};
use crate::data::crud::{self, AuditAction, AuditEntry, ColumnSet, Where};
use crate::data::mfa;
use crate::data::people as people_data;
use crate::data::service_owners::{self, OwnerCleanup, Principal};
use crate::http::error::{AppError, ErrorCode, FieldError, FieldLocation};
use crate::modules::simple_resource::non_empty;
use crate::paged;

const TABLE: &str = "users";

// ---------------------------------------------------------------------------
// Schemas
// ---------------------------------------------------------------------------

/// A profile a user holds
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProfileRef {
    pub id: Uuid,
    pub name: String,
    pub is_builtin: bool,
}

/// The Person CI an account is linked to
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PersonRef {
    pub id: Uuid,
    /// The Person's label (its Name)
    pub label: String,
}

/// Whether the account can sign in as it is (independent of `isActive`)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SignInStatus {
    /// Has an e-mail and a linked Person
    Ready,
    /// Created before e-mails were required: must enter one at the next sign-in
    /// before anything else (403 EMAIL_REQUIRED until then)
    EmailRequired,
    /// Has an e-mail but no linked Person ("account incomplete"): sign-in is
    /// refused. Saving the account's e-mail again links it.
    PersonMissing,
}

impl SignInStatus {
    fn of(email: Option<&str>, person: Option<Uuid>) -> Self {
        match (email, person) {
            (None, _) => SignInStatus::EmailRequired,
            (Some(_), None) => SignInStatus::PersonMissing,
            (Some(_), Some(_)) => SignInStatus::Ready,
        }
    }

    /// The SQL condition on `users`.
    fn sql(self) -> &'static str {
        match self {
            SignInStatus::Ready => "email IS NOT NULL AND person_ci_id IS NOT NULL",
            SignInStatus::EmailRequired => "email IS NULL",
            SignInStatus::PersonMissing => "email IS NOT NULL AND person_ci_id IS NULL",
        }
    }
}

/// The language of what the server writes to a user (e-mail); the web UI
/// keeps its own choice in the browser and stores it here
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema, sqlx::Type)]
#[serde(rename_all = "lowercase")]
#[sqlx(type_name = "text", rename_all = "lowercase")]
pub enum Locale {
    En,
    De,
}

/// The identity provider an account signs in through
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IdentityProviderRef {
    pub id: Uuid,
    pub name: String,
    /// oidc or ldap
    pub kind: String,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct User {
    pub id: Uuid,
    /// Sign-in name, unique regardless of case
    pub username: String,
    pub display_name: String,
    /// Unique regardless of case and Unicode form (stored in NFKC). Null only
    /// for accounts created before e-mails were required
    /// (`signInStatus: email_required`).
    #[schema(required = true)]
    pub email: Option<String>,
    /// The Person CI linked to the account (same e-mail); null while
    /// `signInStatus` is not `ready`
    #[schema(required = true)]
    pub person: Option<PersonRef>,
    #[schema(inline)]
    pub sign_in_status: SignInStatus,
    /// Disabled users cannot sign in and their sessions end
    pub is_active: bool,
    /// Holds the built-in Administrator profile
    pub is_administrator: bool,
    /// Has set up two-factor authentication (an authenticator app)
    pub mfa_enabled: bool,
    /// Null for a local account (username and password). Otherwise the account
    /// was created by this provider and signs in only through it: it has no
    /// password here, and its name, e-mail and profiles are set from the
    /// provider at every sign-in.
    #[schema(required = true)]
    pub identity_provider: Option<IdentityProviderRef>,
    pub profiles: Vec<ProfileRef>,
    #[serde(serialize_with = "ts::serialize")]
    pub password_changed_at: DateTime<Utc>,
    #[serde(serialize_with = "ts_opt::serialize")]
    #[schema(required = true)]
    pub last_login_at: Option<DateTime<Utc>>,
    #[serde(serialize_with = "ts::serialize")]
    pub created_at: DateTime<Utc>,
    #[serde(serialize_with = "ts::serialize")]
    pub updated_at: DateTime<Utc>,
}

pub fn username_schema() -> Schema {
    ObjectBuilder::new().schema_type(Type::String).pattern(Some(USERNAME_PATTERN)).into()
}

/// An account's e-mail: required, never null (SHAA-1505). At most 254
/// characters (RFC 5321), the limit of the Person's Email.
pub fn required_email_schema() -> Schema {
    ObjectBuilder::new()
        .schema_type(Type::String)
        .format(Some(SchemaFormat::KnownFormat(KnownFormat::Email)))
        .max_length(Some(254))
        .into()
}

pub fn password_schema() -> Schema {
    ObjectBuilder::new()
        .schema_type(Type::String)
        // The request validator applies the password policy to this format.
        .format(Some(SchemaFormat::KnownFormat(KnownFormat::Password)))
        .min_length(Some(password::MIN_LENGTH))
        .max_length(Some(password::MAX_LENGTH))
        .description(Some("At least 12 characters"))
        .into()
}

fn profile_ids_schema() -> Schema {
    ArrayBuilder::new()
        .items(schemas::uuid_builder())
        .max_items(Some(100))
        .unique_items(true)
        .description(Some("Permission profiles the user holds (replaces the current set)"))
        .into()
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UserCreate {
    #[schema(schema_with = username_schema)]
    pub username: String,
    #[schema(schema_with = name_schema)]
    #[serde(deserialize_with = "trimmed")]
    pub display_name: String,
    /// Unique regardless of case and Unicode form (stored in NFKC). The account
    /// is linked to the Person with this e-mail, which is created when there is
    /// none (Name = the display name).
    #[schema(schema_with = required_email_schema)]
    #[serde(deserialize_with = "schemas::email")]
    pub email: String,
    #[schema(schema_with = password_schema)]
    pub password: Secret,
    /// Default true
    #[schema(nullable = false)]
    pub is_active: Option<bool>,
    #[schema(schema_with = profile_ids_schema)]
    #[serde(default)]
    pub profile_ids: Vec<Uuid>,
}

impl Check for UserCreate {
    fn check(&self) -> Vec<FieldError> {
        password_problem("password", &self.password)
    }
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UserUpdate {
    #[schema(schema_with = username_schema)]
    username: Option<String>,
    #[schema(schema_with = name_schema)]
    #[serde(default, deserialize_with = "schemas::trimmed_opt")]
    display_name: Option<String>,
    /// Cannot be cleared. Changes the linked Person's Email too (409 CONFLICT
    /// when another account or another Person has the address); an account
    /// without a Person is linked to the Person with the address, or one is created.
    #[schema(schema_with = required_email_schema)]
    #[serde(default, deserialize_with = "schemas::email_opt")]
    email: Option<String>,
    /// false disables the account and ends its sessions
    #[schema(nullable = false)]
    is_active: Option<bool>,
    #[schema(schema_with = profile_ids_schema)]
    profile_ids: Option<Vec<Uuid>>,
}

impl UserUpdate {
    fn columns(&self) -> ColumnSet {
        let mut c = ColumnSet::default();
        c.opt("username", self.username.clone())
            .opt("display_name", self.display_name.clone())
            .opt("email", self.email.clone().map(Some))
            .opt("is_active", self.is_active);
        c
    }
}

impl Check for UserUpdate {
    fn check(&self) -> Vec<FieldError> {
        if self.profile_ids.is_some() { Vec::new() } else { non_empty(&self.columns()) }
    }
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PasswordReset {
    /// The new password
    #[schema(schema_with = password_schema)]
    password: Secret,
}

impl Check for PasswordReset {
    fn check(&self) -> Vec<FieldError> {
        password_problem("password", &self.password)
    }
}

pub fn password_problem(field: &str, password: &str) -> Vec<FieldError> {
    password::policy_error(password)
        .map(|message| {
            vec![FieldError {
                location: FieldLocation::Body,
                field: field.into(),
                message,
                code: "password_policy".into(),
            }]
        })
        .unwrap_or_default()
}

fn sort_schema() -> Schema {
    schemas::sort_schema(&["username", "displayName", "createdAt", "lastLoginAt"], "username")
}

#[derive(Debug, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct UserList {
    /// Page size (1-200)
    #[param(required = false, default = 50, minimum = 1, maximum = 200)]
    limit: i64,
    /// Rows to skip
    #[param(required = false, default = 0, minimum = 0, maximum = 1_000_000)]
    offset: i64,
    /// Matches username, display name and email
    #[param(schema_with = schemas::search_schema)]
    q: Option<String>,
    #[param(required = false, schema_with = sort_schema)]
    sort: Sort,
    #[param(inline)]
    is_active: Option<QueryBool>,
    /// Users holding any of these profiles
    #[param(schema_with = schemas::uuid_list_schema)]
    profile_id: Option<UuidList>,
    /// Accounts in this state, e.g. `email_required` for the accounts that must
    /// still enter an e-mail, `person_missing` for incomplete ones
    #[param(inline)]
    sign_in_status: Option<SignInStatus>,
    /// The account linked to this Person CI
    person_ci_id: Option<Uuid>,
}
paged!(UserList);

// ---------------------------------------------------------------------------
// Service
// ---------------------------------------------------------------------------

async fn dtos(conn: &mut PgConnection, rows: Vec<UserRow>) -> Result<Vec<User>, AppError> {
    let ids: Vec<Uuid> = rows.iter().map(|r| r.id).collect();
    let held = data::profiles_of_users(conn, &ids).await?;
    let mfa = crate::data::mfa::enabled_among(conn, &ids).await?;
    let person_ids: Vec<Uuid> = rows.iter().filter_map(|r| r.person_ci_id).collect();
    let persons = if person_ids.is_empty() { Vec::new() } else { people_data::labels(conn, &person_ids).await? };
    let provider_ids: Vec<Uuid> = rows.iter().filter_map(|r| r.identity_provider_id).collect();
    let providers: Vec<(Uuid, String, String)> = if provider_ids.is_empty() {
        Vec::new()
    } else {
        sqlx::query_as("SELECT id, name, kind FROM identity_providers WHERE id = ANY($1)")
            .bind(&provider_ids)
            .fetch_all(&mut *conn)
            .await?
    };
    Ok(rows
        .into_iter()
        .map(|r| {
            let profiles: Vec<ProfileRef> = held
                .iter()
                .filter(|p| p.user_id == r.id)
                .map(|p| ProfileRef { id: p.id, name: p.name.clone(), is_builtin: p.is_builtin })
                .collect();
            User {
                id: r.id,
                username: r.username,
                display_name: r.display_name,
                sign_in_status: SignInStatus::of(r.email.as_deref(), r.person_ci_id),
                email: r.email,
                person: r.person_ci_id.and_then(|id| {
                    persons.iter().find(|p| p.0 == id).map(|(id, label)| PersonRef { id: *id, label: label.clone() })
                }),
                is_active: r.is_active,
                is_administrator: profiles.iter().any(|p| p.is_builtin),
                mfa_enabled: mfa.contains(&r.id),
                identity_provider: r.identity_provider_id.and_then(|id| {
                    providers.iter().find(|p| p.0 == id).map(|(id, name, kind)| IdentityProviderRef {
                        id: *id,
                        name: name.clone(),
                        kind: kind.clone(),
                    })
                }),
                profiles,
                password_changed_at: r.password_changed_at,
                last_login_at: r.last_login_at,
                created_at: r.created_at,
                updated_at: r.updated_at,
            }
        })
        .collect())
}

pub async fn load(conn: &mut PgConnection, id: Uuid) -> Result<User, AppError> {
    let row = data::get_user(conn, id, false).await?.ok_or_else(|| AppError::missing("User", id))?;
    Ok(dtos(conn, vec![row]).await?.remove(0))
}

/// The user, their row locked until the transaction ends.
pub async fn lock_for_update(conn: &mut PgConnection, id: Uuid) -> Result<User, AppError> {
    lock(conn, id).await
}

async fn lock(conn: &mut PgConnection, id: Uuid) -> Result<User, AppError> {
    let row = data::get_user(conn, id, true).await?.ok_or_else(|| AppError::missing("User", id))?;
    Ok(dtos(conn, vec![row]).await?.remove(0))
}

/// A user manager can only act on accounts whose permissions they hold themselves.
pub(crate) async fn must_cover_user(
    conn: &mut PgConnection,
    ctx: &RequestContext,
    user_id: Uuid,
) -> Result<(), AppError> {
    let Some(me) = ctx.principal() else { return Ok(()) };
    if me.permissions.administrator {
        return Ok(());
    }
    let theirs = data::load_permissions(conn, user_id).await?;
    if me.permissions.covers(&theirs) {
        Ok(())
    } else {
        Err(forbidden("This user has permissions you do not hold yourself"))
    }
}

/// Checks that the profiles exist and that the caller may hand them out.
async fn check_profiles(conn: &mut PgConnection, ctx: &RequestContext, ids: &[Uuid]) -> Result<(), AppError> {
    let found: HashSet<Uuid> = data::existing_profiles(conn, ids).await?.into_iter().collect();
    let missing: Vec<FieldError> = ids
        .iter()
        .enumerate()
        .filter(|(_, id)| !found.contains(id))
        .map(|(i, _)| FieldError {
            location: FieldLocation::Body,
            field: format!("profileIds.{i}"),
            message: "Permission profile does not exist".into(),
            code: "not_found".into(),
        })
        .collect();
    if !missing.is_empty() {
        return Err(AppError::validation(missing));
    }
    if let Some(me) = ctx.principal()
        && !me.permissions.covers(&profiles::union_of(conn, ids).await?)
    {
        return Err(forbidden("You can only assign profiles whose permissions you hold yourself"));
    }
    Ok(())
}

fn not_yourself(ctx: &RequestContext, id: Uuid, what: &str) -> Result<(), AppError> {
    if ctx.principal().is_some_and(|me| me.user_id == id) {
        return Err(AppError::conflict(format!("You cannot {what} your own account")));
    }
    Ok(())
}

/// GH#413: the administration routes ask for no current password, so a
/// session alone (a stolen one, say) must not use them to take over its own
/// account for good. The caller is sent to the self-service route, which
/// asks for the current password (and the code, once MFA is set up).
pub(crate) fn not_your_own(ctx: &RequestContext, id: Uuid, what: &str, instead: &str) -> Result<(), AppError> {
    if ctx.principal().is_some_and(|me| me.user_id == id) {
        return Err(AppError::conflict(format!(
            "You cannot {what} of your own account here. {instead}, which asks for your current password."
        )));
    }
    Ok(())
}

pub async fn list(pool: &PgPool, q: &UserList) -> Result<Page<User>, AppError> {
    let filter = |w: &mut Where<'_>| {
        if let Some(text) = &q.q {
            let p = like_pattern(text);
            w.and()
                .push("(username ILIKE ")
                .push_bind(p.clone())
                .push(" OR display_name ILIKE ")
                .push_bind(p.clone())
                .push(" OR email ILIKE ")
                .push_bind(p)
                .push(")");
        }
        if let Some(active) = q.is_active {
            w.and().push("is_active = ").push_bind(bool::from(active));
        }
        if let Some(ids) = &q.profile_id {
            w.and()
                .push("id IN (SELECT user_id FROM user_permission_profiles WHERE profile_id = ANY(")
                .push_bind(ids.0.clone())
                .push("))");
        }
        if let Some(status) = q.sign_in_status {
            w.and().push("(").push(status.sql()).push(")");
        }
        if let Some(id) = q.person_ci_id {
            w.and().push("person_ci_id = ").push_bind(id);
        }
    };
    let column = match q.sort.field.as_str() {
        "displayName" => "lower(display_name)",
        "createdAt" => "created_at",
        "lastLoginAt" => "last_login_at",
        _ => "lower(username)",
    };
    let order = format!("{column} {} NULLS LAST, id", q.sort.dir());
    let (rows, total) = crud::select_page::<UserRow>(
        &mut *pool.acquire().await?,
        "users",
        data::USER_COLUMNS,
        &filter,
        &order,
        q.limit,
        q.offset,
    )
    .await?;
    let data = dtos(&mut *pool.acquire().await?, rows).await?;
    Ok(Page { data, page: q.page_meta(total) })
}

pub async fn get(pool: &PgPool, id: Uuid) -> Result<User, AppError> {
    load(&mut *pool.acquire().await?, id).await
}

/// Inserts a user in the caller's transaction (shared by the API, first-run setup and the CLI).
pub async fn create_in(conn: &mut PgConnection, ctx: &RequestContext, b: &UserCreate) -> Result<User, AppError> {
    check_profiles(conn, ctx, &b.profile_ids).await?;
    let hash = password::hash(&b.password).await?;
    // The CLI and first-run setup build the request in code.
    let email = schemas::normalize_email(&b.email);
    let id = data::insert_user(
        conn,
        &data::NewUser {
            username: &b.username,
            display_name: &b.display_name,
            email: Some(&email),
            password_hash: &hash,
            is_active: b.is_active.unwrap_or(true),
        },
    )
    .await?;
    data::set_user_profiles(conn, id, &b.profile_ids).await?;
    people::link_user(conn, ctx, id).await?;
    let dto = load(conn, id).await?;
    let entry = AuditEntry {
        action: AuditAction::Create,
        entity_type: TABLE,
        entity_id: id,
        old_value: None,
        new_value: Some(crud::json(&dto)),
    };
    crud::write_audit(conn, ctx, vec![entry]).await?;
    Ok(dto)
}

pub async fn create(pool: &PgPool, ctx: &RequestContext, b: &UserCreate) -> Result<User, AppError> {
    let mut tx = pool.begin().await?;
    let dto = create_in(&mut tx, ctx, b).await?;
    tx.commit().await?;
    Ok(dto)
}

pub async fn update(pool: &PgPool, ctx: &RequestContext, id: Uuid, b: &UserUpdate) -> Result<User, AppError> {
    if b.is_active == Some(false) {
        not_yourself(ctx, id, "disable")?;
    }
    let mut tx = pool.begin().await?;
    let before = lock(&mut tx, id).await?;
    must_cover_user(&mut tx, ctx, id).await?;
    let disabling = before.is_active && b.is_active == Some(false);
    if disabling {
        // Tokens first: their rows must be locked before the first audit
        // insert takes the chain head (GH#166). A leaver's tokens for service
        // accounts go too (GH#183).
        api_tokens::revoke_all_of_user(&mut tx, ctx, id, true, "account disabled").await?;
        mfa::delete_challenges_of_user(&mut tx, id).await?;
    }
    if let Some(ids) = &b.profile_ids {
        check_profiles(&mut tx, ctx, ids).await?;
        data::set_user_profiles(&mut tx, id, ids).await?;
    }
    let columns = b.columns();
    if !columns.is_empty() {
        crud::update_row::<UserRow>(&mut tx, TABLE, data::USER_COLUMNS, id, columns).await?;
    }
    // The Person follows the e-mail; saving it again links an account without one.
    if b.email.is_some() {
        people::link_user(&mut tx, ctx, id).await?;
    }
    if disabling {
        let ended = data::delete_user_sessions(&mut tx, id, None).await?;
        events::revoked(&mut tx, ctx, &ended, RevokeReason::UserDisabled).await?;
    }
    let dto = load(&mut tx, id).await?;
    let entry = AuditEntry {
        action: AuditAction::Update,
        entity_type: TABLE,
        entity_id: id,
        old_value: Some(crud::json(&before)),
        new_value: Some(crud::json(&dto)),
    };
    crud::write_audit(&mut tx, ctx, vec![entry]).await?;
    // The last-Administrator check is deferred to here (LAST_ADMINISTRATOR).
    tx.commit().await?;
    Ok(dto)
}

/// An administrator's reset of another user's password (GH#413: never the
/// caller's own, see [`not_your_own`]).
pub async fn reset_password(
    pool: &PgPool,
    ctx: &RequestContext,
    id: Uuid,
    new_password: &str,
) -> Result<User, AppError> {
    not_your_own(ctx, id, "reset the password", "Change it under PUT /api/v1/auth/password")?;
    Ok(set_password(pool, ctx, id, new_password).await?.0)
}

/// Sets a new password, ends the user's sessions, drops their pending
/// second-factor steps (GH#191) and revokes their API tokens. An
/// administrator's reset (not the user's own change) also revokes the tokens
/// the user created for other owners: the account may have been compromised
/// (GH#145). The caller's own session is not ended but replaced by a new one,
/// returned for its cookies (GH#510).
pub async fn set_password(
    pool: &PgPool,
    ctx: &RequestContext,
    id: Uuid,
    new_password: &str,
) -> Result<(User, Option<auth::Rotated>), AppError> {
    let hash = password::hash(new_password).await?;
    let mut tx = pool.begin().await?;
    let before = lock(&mut tx, id).await?;
    if let Some(provider) = &before.identity_provider {
        return Err(AppError::conflict(format!(
            "This account signs in through \"{}\" and has no password here",
            provider.name
        )));
    }
    must_cover_user(&mut tx, ctx, id).await?;
    data::set_password(&mut tx, id, &hash).await?;
    let own = ctx.principal().filter(|p| p.user_id == id);
    // Tokens first: their rows must be locked before the first audit insert
    // takes the chain head (GH#166).
    let because = if own.is_some() { "password change" } else { "password reset" };
    api_tokens::revoke_all_of_user(&mut tx, ctx, id, own.is_none(), because).await?;
    mfa::delete_challenges_of_user(&mut tx, id).await?;
    let ended = data::delete_user_sessions(&mut tx, id, own.and_then(|p| p.session_id())).await?;
    let reason = if own.is_some() { RevokeReason::PasswordChanged } else { RevokeReason::PasswordReset };
    events::revoked(&mut tx, ctx, &ended, reason).await?;
    let rotated = match own.and_then(|p| p.session_id()) {
        Some(session_id) => Some(auth::rotate_own_session(&mut tx, ctx, session_id, false).await?),
        None => None,
    };
    let dto = load(&mut tx, id).await?;
    let entry = AuditEntry {
        action: AuditAction::Update,
        entity_type: TABLE,
        entity_id: id,
        old_value: Some(crud::json(&before)),
        new_value: Some(crud::json(&dto)),
    };
    crud::write_audit(&mut tx, ctx, vec![entry]).await?;
    tx.commit().await?;
    Ok((dto, rotated))
}

/// Deletes the account. The business services it owns lose it as owner, each
/// with an `update` row in its history (SHAA-927 §1.4); the result counts them.
pub async fn remove(pool: &PgPool, ctx: &RequestContext, id: Uuid) -> Result<OwnerRemoval, AppError> {
    not_yourself(ctx, id, "delete")?;
    let mut tx = pool.begin().await?;
    let before = lock(&mut tx, id).await?;
    must_cover_user(&mut tx, ctx, id).await?;
    // The services are locked before the first audit row takes the chain head (GH#166).
    let cleanup = OwnerCleanup::prepare(&mut tx, Principal::User(id)).await?;
    // Tokens first (GH#166): the ones they created for other owners are
    // revoked, as for disabling (GH#183); their own are then deleted with them.
    api_tokens::revoke_all_of_user(&mut tx, ctx, id, true, "account deletion").await?;
    // Ended explicitly (not by the foreign key's cascade) so each gets an audit row.
    let ended = data::delete_user_sessions(&mut tx, id, None).await?;
    events::revoked(&mut tx, ctx, &ended, RevokeReason::UserDeleted).await?;
    // Their API tokens go with them; each gets a delete row.
    api_tokens::audit_deleted_with_owner(&mut tx, ctx, id).await?;
    data::delete_user(&mut tx, id).await?;
    let entry = AuditEntry {
        action: AuditAction::Delete,
        entity_type: TABLE,
        entity_id: id,
        old_value: Some(crud::json(&before)),
        new_value: None,
    };
    crud::write_audit(&mut tx, ctx, vec![entry]).await?;
    let affected = cleanup.finish(&mut tx, ctx).await?;
    let class = service_owners::service_class_id(&mut tx).await?;
    tx.commit().await?;
    Ok(OwnerRemoval { affected_services: Count::scoped(ctx, &[class], affected) })
}

// ---------------------------------------------------------------------------
// Routes
// ---------------------------------------------------------------------------

const TAG: &str = "Users";
const BASE: &str = "/api/v1/admin/users";
const BY_ID: &str = "/api/v1/admin/users/{id}";

pub fn routes() -> Vec<Route> {
    let manage = GlobalPermission::UsersManage;
    vec![
        route(Method::GET, BASE, "listUsers")
            .tag(TAG)
            .summary("List users (paginated, searchable, filterable by active flag and profile)")
            .requires(manage)
            .handle(|api, In(NoPath, Query(q), NoBody): In<NoPath, Query<UserList>, NoBody>| async move {
                Ok(Json(list(&api.pool, &q).await?))
            }),
        route(Method::GET, BY_ID, "getUser")
            .tag(TAG)
            .summary("Get one user with the profiles they hold")
            .requires(manage)
            .errors(&[ErrorCode::NotFound])
            .handle(|api, In(IdPath(id), NoQuery, NoBody): In<IdPath, NoQuery, NoBody>| async move {
                Ok(Json(get(&api.pool, id).await?))
            }),
        route(Method::POST, BASE, "createUser")
            .tag(TAG)
            .summary("Create a local user with a password and permission profiles")
            .description("403 when assigning a profile that grants permissions the caller does not hold.")
            .status(StatusCode::CREATED)
            .requires(manage)
            .recent_reauthentication()
            .errors(&[ErrorCode::Conflict])
            .handle(|api, In(NoPath, NoQuery, Body(b)): In<NoPath, NoQuery, Body<UserCreate>>| async move {
                Ok(Json(create(&api.pool, &api.ctx, &b).await?))
            }),
        route(Method::PATCH, BY_ID, "updateUser")
            .tag(TAG)
            .summary("Update a user (partial): rename, disable/enable, assign profiles")
            .description(
                "`isActive: false` disables the account and ends its sessions. `profileIds` replaces the profiles the user holds. For an account of an identity provider, the name, e-mail and profiles are set again from the provider at its next sign-in (change the group mappings instead); disabling it holds whatever the provider says. 409 LAST_ADMINISTRATOR when the change would leave no active user with the Administrator profile; 409 CONFLICT when disabling yourself.",
            )
            .requires(manage)
            .recent_reauthentication()
            .errors(&[ErrorCode::NotFound, ErrorCode::Conflict, ErrorCode::LastAdministrator])
            .handle(|api, In(IdPath(id), NoQuery, Body(b)): In<IdPath, NoQuery, Body<UserUpdate>>| async move {
                Ok(Json(update(&api.pool, &api.ctx, id, &b).await?))
            }),
        route(Method::DELETE, BY_ID, "deleteUser")
            .tag(TAG)
            .summary("Delete a user (prefer disabling; the audit log keeps their id and name)")
            .description("409 when deleting yourself or the last active Administrator. The business services the user owns lose them as owner, each with an `update` row in its history; `affectedServices` counts them (null when the caller may not view the business service class). Disabling keeps the user as owner, marked as disabled.")
            .requires(manage)
            .session_only()
            .errors(&[ErrorCode::NotFound, ErrorCode::Conflict, ErrorCode::LastAdministrator])
            .handle(|api, In(IdPath(id), NoQuery, NoBody): In<IdPath, NoQuery, NoBody>| async move {
                Ok(Json(remove(&api.pool, &api.ctx, id).await?))
            }),
        route(Method::PUT, "/api/v1/admin/users/{id}/password", "resetUserPassword")
            .tag(TAG)
            .summary("Set a new password for a user, end their sessions and revoke their API tokens")
            .description("Every API token of the user that still works is revoked (`revokedBy` is the caller), so a token minted with a stolen password does not outlive the reset. So is every working token the user created for another owner (`createdByUserId`), since the account may have been compromised. 409 for an account that signs in through an identity provider (it has no password here), and for your own account: change your own password with `changeOwnPassword` (PUT /api/v1/auth/password), which asks for your current password.")
            .requires(manage)
            .recent_reauthentication()
            .errors(&[ErrorCode::NotFound, ErrorCode::Conflict])
            .handle(|api, In(IdPath(id), NoQuery, Body(b)): In<IdPath, NoQuery, Body<PasswordReset>>| async move {
                Ok(Json(reset_password(&api.pool, &api.ctx, id, &b.password).await?))
            }),
    ]
}
