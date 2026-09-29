//! Administration > Sign-in: OIDC providers and LDAP/AD directories, and the
//! mapping from their groups to permission profiles.
//!
//! Only holders of the built-in Administrator profile may read or change these
//! (403 otherwise, even with users.manage): a provider decides who gets which
//! profile, the Administrator profile included, so it is as powerful as the
//! Administrator profile itself, and its settings and group mappings map out
//! the path to that profile.
//!
//! Secrets (the OIDC client secret, the directory's bind password) are
//! write-only: responses say whether one is set, never what it is, and the
//! audit rows carry the same representation.
//!
//! Disabling or deleting a provider ends the sessions of its accounts. A
//! provider that still has accounts cannot be deleted: disable it (its
//! accounts then cannot sign in) or delete the accounts first.

use std::collections::HashSet;

use axum::http::{Method, StatusCode};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{PgConnection, PgPool};
use utoipa::ToSchema;
use utoipa::openapi::schema::{ArrayBuilder, ObjectBuilder, Schema, Type};
use uuid::Uuid;

use super::sso::{self, LDAP, OIDC};
use crate::api::context::{RequestContext, forbidden};
use crate::api::route::{Body, Check, IdPath, In, Json, NoBody, NoContent, NoPath, NoQuery, Route, route};
use crate::api::schemas::{self, name_schema, sort_order_schema, trimmed, ts};
use crate::auth::AuthState;
use crate::auth::events::{self, RevokeReason};
use crate::auth::permissions::GlobalPermission;
use crate::auth::sso::{ldap, oidc, tls};
use crate::data::auth as auth_data;
use crate::data::crud::{self, AuditAction, AuditEntry};
use crate::data::identity_providers::{self as data, ProviderRow, TABLE};
use crate::http::error::{AppError, ErrorCode, FieldError, FieldLocation};

const MAX_MAPPINGS: usize = 500;

// ---------------------------------------------------------------------------
// Schemas
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum ProviderKind {
    /// OpenID Connect (authorization code + PKCE): a "Sign in with ..." button
    Oidc,
    /// LDAP / Active Directory: the username/password form
    Ldap,
}

impl ProviderKind {
    fn as_str(self) -> &'static str {
        match self {
            ProviderKind::Oidc => OIDC,
            ProviderKind::Ldap => LDAP,
        }
    }
}

/// Whether the exemption of OIDC accounts from `requireMfa` needs proof in the ID token
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub enum MfaAssurance {
    /// The ID token must prove a second factor (`amr`, or `acr` in `requiredAcr`); otherwise users whose profiles require MFA are refused
    Verify,
    /// The provider is trusted to enforce MFA for this client; the ID token is not checked
    TrustProvider,
}

impl MfaAssurance {
    fn as_db(self) -> &'static str {
        match self {
            MfaAssurance::Verify => oidc::MfaPolicy::VERIFY,
            MfaAssurance::TrustProvider => oidc::MfaPolicy::TRUST_PROVIDER,
        }
    }

    fn from_db(v: Option<&str>) -> MfaAssurance {
        if v == Some(oidc::MfaPolicy::TRUST_PROVIDER) { MfaAssurance::TrustProvider } else { MfaAssurance::Verify }
    }
}

fn mfa_assurance_schema() -> Schema {
    ObjectBuilder::new()
        .schema_type(Type::String)
        .enum_values(Some(["verify", "trustProvider"]))
        .description(Some(
            "verify: the ID token must prove a second factor, or users whose profiles require MFA are refused. \
             trustProvider: the provider is trusted to enforce MFA; the token is not checked.",
        ))
        .into()
}

const MAX_REQUIRED_ACR: usize = 10;

fn required_acr_schema() -> Schema {
    ArrayBuilder::new()
        .items(ObjectBuilder::new().schema_type(Type::String).pattern(Some(r"^[!-~]{1,200}$")))
        .max_items(Some(MAX_REQUIRED_ACR))
        .description(Some(
            "Only with mfaAssurance verify: the ID token's acr must be one of these (case-sensitive), and they are \
             sent as acr_values. Empty: amr decides. Printable ASCII without spaces, 1 to 200 characters each.",
        ))
        .into()
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OidcConfig {
    pub issuer_url: String,
    pub client_id: String,
    /// A client secret is stored (it is never returned)
    pub client_secret_set: bool,
    /// Requested besides openid
    pub scopes: String,
    /// ID token claim used as the username (dots descend into objects)
    pub username_claim: String,
    /// ID token claim listing the user's groups (dots descend into objects)
    pub groups_claim: String,
    /// Register this at the provider; null until PUBLIC_URL is set
    #[schema(required = true)]
    pub redirect_uri: Option<String>,
    #[schema(inline)]
    pub mfa_assurance: MfaAssurance,
    /// With verify: acr values that prove MFA (empty: amr decides)
    pub required_acr: Vec<String>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LdapConfig {
    /// ldaps://host[:port] or ldap://host[:port] (then with StartTLS)
    pub url: String,
    pub start_tls: bool,
    /// Service account the user search binds as; null searches anonymously
    #[schema(required = true)]
    pub bind_dn: Option<String>,
    /// A bind password is stored (it is never returned)
    pub bind_password_set: bool,
    pub user_base_dn: String,
    /// {username} is replaced by the escaped sign-in name
    pub user_filter: String,
    pub username_attribute: String,
    pub display_name_attribute: String,
    pub email_attribute: String,
    /// Holds the DNs of the user's groups (memberOf)
    pub group_attribute: String,
}

/// Users in this group get this profile
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GroupMapping {
    /// As the provider reports it (a group name or id, or a group DN); compared case-insensitively
    pub group: String,
    pub profile_id: Uuid,
    pub profile_name: String,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IdentityProvider {
    pub id: Uuid,
    pub kind: ProviderKind,
    /// Shown on the sign-in button and in the audit trail
    pub name: String,
    /// Disabled: nobody signs in through it and its accounts' sessions ended
    pub is_enabled: bool,
    /// Button order (OIDC); the order directories are asked in (LDAP)
    pub sort_order: i32,
    /// Extra CA certificates (PEM) trusted for this provider
    #[schema(required = true)]
    pub ca_certificate: Option<String>,
    /// Set for kind oidc
    #[schema(required = true)]
    pub oidc: Option<OidcConfig>,
    /// Set for kind ldap
    #[schema(required = true)]
    pub ldap: Option<LdapConfig>,
    pub group_mappings: Vec<GroupMapping>,
    /// Accounts that sign in through this provider
    pub user_count: i64,
    #[serde(serialize_with = "ts::serialize")]
    pub created_at: DateTime<Utc>,
    #[serde(serialize_with = "ts::serialize")]
    pub updated_at: DateTime<Utc>,
}

fn text(max: usize) -> Schema {
    ObjectBuilder::new().schema_type(Type::String).min_length(Some(1)).max_length(Some(max)).pattern(Some(r"\S")).into()
}

fn nullable_text(max: usize) -> Schema {
    schemas::nullable_trimmed_schema(max)
}

fn short_text() -> Schema {
    text(256)
}

fn long_text() -> Schema {
    text(1024)
}

fn url_schema() -> Schema {
    text(2048)
}

fn secret_schema() -> Schema {
    schemas::nullable_string_schema(4096)
}

fn claim_schema() -> Schema {
    ObjectBuilder::new().schema_type(Type::String).pattern(Some(r"^[A-Za-z0-9_:/.\-]{1,128}$")).into()
}

fn attribute_schema() -> Schema {
    ObjectBuilder::new().schema_type(Type::String).pattern(Some(r"^[A-Za-z][A-Za-z0-9-]{0,127}$")).into()
}

fn scopes_schema() -> Schema {
    ObjectBuilder::new()
        .schema_type(Type::String)
        .max_length(Some(1024))
        .pattern(Some(r"^[\x21\x23-\x5B\x5D-\x7E ]*$"))
        .description(Some("Space-separated; openid is always added. Default: profile email"))
        .into()
}

fn pem_schema() -> Schema {
    schemas::nullable_string_schema(65536)
}

fn mappings_schema() -> Schema {
    ArrayBuilder::new()
        .items(<GroupMappingInput as utoipa::PartialSchema>::schema())
        .max_items(Some(MAX_MAPPINGS))
        .description(Some("Replaces all mappings of the provider"))
        .into()
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GroupMappingInput {
    #[schema(schema_with = long_text)]
    #[serde(deserialize_with = "trimmed")]
    group: String,
    profile_id: Uuid,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OidcInput {
    /// https://... (http only for a loopback test issuer)
    #[schema(schema_with = url_schema)]
    #[serde(deserialize_with = "trimmed")]
    issuer_url: String,
    #[schema(schema_with = short_text)]
    #[serde(deserialize_with = "trimmed")]
    client_id: String,
    /// Null or left out for a public client
    #[schema(schema_with = secret_schema)]
    #[serde(default)]
    client_secret: Option<String>,
    #[schema(schema_with = scopes_schema)]
    #[serde(default)]
    scopes: Option<String>,
    /// Default preferred_username
    #[schema(schema_with = claim_schema)]
    #[serde(default)]
    username_claim: Option<String>,
    /// Default groups
    #[schema(schema_with = claim_schema)]
    #[serde(default)]
    groups_claim: Option<String>,
    /// Default verify
    #[schema(schema_with = mfa_assurance_schema)]
    #[serde(default)]
    mfa_assurance: Option<MfaAssurance>,
    #[schema(schema_with = required_acr_schema)]
    #[serde(default)]
    required_acr: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LdapInput {
    #[schema(schema_with = url_schema)]
    #[serde(deserialize_with = "trimmed")]
    url: String,
    /// Default: true for ldap://, false for ldaps://
    #[schema(nullable = false)]
    #[serde(default)]
    start_tls: Option<bool>,
    #[schema(schema_with = nullable_text_1024)]
    #[serde(default, deserialize_with = "schemas::trimmed_opt")]
    bind_dn: Option<String>,
    #[schema(schema_with = secret_schema)]
    #[serde(default)]
    bind_password: Option<String>,
    #[schema(schema_with = long_text)]
    #[serde(deserialize_with = "trimmed")]
    user_base_dn: String,
    /// Default (&(objectClass=user)(sAMAccountName={username})) (Active Directory)
    #[schema(schema_with = long_text)]
    #[serde(default, deserialize_with = "schemas::trimmed_opt")]
    user_filter: Option<String>,
    /// Default sAMAccountName
    #[schema(schema_with = attribute_schema)]
    #[serde(default)]
    username_attribute: Option<String>,
    /// Default displayName
    #[schema(schema_with = attribute_schema)]
    #[serde(default)]
    display_name_attribute: Option<String>,
    /// Default mail
    #[schema(schema_with = attribute_schema)]
    #[serde(default)]
    email_attribute: Option<String>,
    /// Default memberOf
    #[schema(schema_with = attribute_schema)]
    #[serde(default)]
    group_attribute: Option<String>,
}

fn nullable_text_1024() -> Schema {
    nullable_text(1024)
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IdentityProviderCreate {
    #[schema(inline)]
    kind: ProviderKind,
    #[schema(schema_with = name_schema)]
    #[serde(deserialize_with = "trimmed")]
    name: String,
    /// Default true
    #[schema(nullable = false)]
    #[serde(default)]
    is_enabled: Option<bool>,
    #[schema(schema_with = sort_order_schema)]
    #[serde(default)]
    sort_order: Option<i32>,
    #[schema(schema_with = pem_schema)]
    #[serde(default)]
    ca_certificate: Option<String>,
    /// Required for kind oidc
    #[schema(inline)]
    #[serde(default)]
    oidc: Option<OidcInput>,
    /// Required for kind ldap
    #[schema(inline)]
    #[serde(default)]
    ldap: Option<LdapInput>,
    #[schema(schema_with = mappings_schema)]
    #[serde(default)]
    group_mappings: Vec<GroupMappingInput>,
}

fn body_error(field: &str, message: impl Into<String>, code: &str) -> FieldError {
    FieldError { location: FieldLocation::Body, field: field.into(), message: message.into(), code: code.into() }
}

impl Check for IdentityProviderCreate {
    fn check(&self) -> Vec<FieldError> {
        let mut errors = Vec::new();
        match self.kind {
            ProviderKind::Oidc => {
                if self.oidc.is_none() {
                    errors.push(body_error("oidc", "Required for an OIDC provider", "required"));
                }
                if self.ldap.is_some() {
                    errors.push(body_error("ldap", "Only for kind ldap", "not_allowed"));
                }
            }
            ProviderKind::Ldap => {
                if self.ldap.is_none() {
                    errors.push(body_error("ldap", "Required for an LDAP directory", "required"));
                }
                if self.oidc.is_some() {
                    errors.push(body_error("oidc", "Only for kind oidc", "not_allowed"));
                }
            }
        }
        errors
    }
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OidcPatch {
    #[schema(schema_with = url_schema)]
    #[serde(default, deserialize_with = "schemas::trimmed_opt")]
    issuer_url: Option<String>,
    #[schema(schema_with = short_text)]
    #[serde(default, deserialize_with = "schemas::trimmed_opt")]
    client_id: Option<String>,
    /// A string replaces the secret, null removes it, left out keeps it
    #[schema(schema_with = secret_schema)]
    #[serde(default, deserialize_with = "schemas::patch")]
    client_secret: Option<Option<String>>,
    #[schema(schema_with = scopes_schema)]
    #[serde(default)]
    scopes: Option<String>,
    #[schema(schema_with = claim_schema)]
    #[serde(default)]
    username_claim: Option<String>,
    #[schema(schema_with = claim_schema)]
    #[serde(default)]
    groups_claim: Option<String>,
    #[schema(schema_with = mfa_assurance_schema)]
    #[serde(default)]
    mfa_assurance: Option<MfaAssurance>,
    #[schema(schema_with = required_acr_schema)]
    #[serde(default)]
    required_acr: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LdapPatch {
    #[schema(schema_with = url_schema)]
    #[serde(default, deserialize_with = "schemas::trimmed_opt")]
    url: Option<String>,
    #[schema(nullable = false)]
    #[serde(default)]
    start_tls: Option<bool>,
    /// null: search anonymously (the bind password is removed too)
    #[schema(schema_with = nullable_text_1024)]
    #[serde(default, deserialize_with = "schemas::patch_trimmed")]
    bind_dn: Option<Option<String>>,
    /// A string replaces the password, null removes it, left out keeps it
    #[schema(schema_with = secret_schema)]
    #[serde(default, deserialize_with = "schemas::patch")]
    bind_password: Option<Option<String>>,
    #[schema(schema_with = long_text)]
    #[serde(default, deserialize_with = "schemas::trimmed_opt")]
    user_base_dn: Option<String>,
    #[schema(schema_with = long_text)]
    #[serde(default, deserialize_with = "schemas::trimmed_opt")]
    user_filter: Option<String>,
    #[schema(schema_with = attribute_schema)]
    #[serde(default)]
    username_attribute: Option<String>,
    #[schema(schema_with = attribute_schema)]
    #[serde(default)]
    display_name_attribute: Option<String>,
    #[schema(schema_with = attribute_schema)]
    #[serde(default)]
    email_attribute: Option<String>,
    #[schema(schema_with = attribute_schema)]
    #[serde(default)]
    group_attribute: Option<String>,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IdentityProviderUpdate {
    #[schema(schema_with = name_schema)]
    #[serde(default, deserialize_with = "schemas::trimmed_opt")]
    name: Option<String>,
    /// false stops sign-ins through it and ends its accounts' sessions
    #[schema(nullable = false)]
    #[serde(default)]
    is_enabled: Option<bool>,
    #[schema(schema_with = sort_order_schema)]
    #[serde(default)]
    sort_order: Option<i32>,
    #[schema(schema_with = pem_schema)]
    #[serde(default, deserialize_with = "schemas::patch")]
    ca_certificate: Option<Option<String>>,
    /// Only for kind oidc; fields left out are kept
    #[schema(inline)]
    #[serde(default)]
    oidc: Option<OidcPatch>,
    /// Only for kind ldap; fields left out are kept
    #[schema(inline)]
    #[serde(default)]
    ldap: Option<LdapPatch>,
    #[schema(schema_with = mappings_schema)]
    #[serde(default)]
    group_mappings: Option<Vec<GroupMappingInput>>,
}

impl Check for IdentityProviderUpdate {}

/// A connection test; for a directory, optionally with a name to look up (no password)
#[derive(Debug, Default, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConnectionTestInput {
    /// LDAP only: look this sign-in name up and show what the directory says
    #[schema(schema_with = nullable_text_256)]
    #[serde(default, deserialize_with = "schemas::trimmed_opt")]
    username: Option<String>,
}

fn nullable_text_256() -> Schema {
    nullable_text(256)
}

impl Check for ConnectionTestInput {}

/// What a directory says about a user (connection test)
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DirectoryUserPreview {
    pub dn: String,
    #[schema(required = true)]
    pub username: Option<String>,
    #[schema(required = true)]
    pub display_name: Option<String>,
    #[schema(required = true)]
    pub email: Option<String>,
    pub groups: Vec<String>,
    /// The profiles these groups map to; empty means the user would be refused
    pub profiles: Vec<String>,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConnectionTest {
    /// The provider answered as expected
    pub ok: bool,
    /// What was checked, or what went wrong (for the administrator)
    pub message: String,
    /// Findings, one per line
    pub details: Vec<String>,
    /// LDAP with a username: the entry found (null when none or several matched)
    #[schema(required = true)]
    pub user: Option<DirectoryUserPreview>,
}

// ---------------------------------------------------------------------------
// Validation of a complete provider
// ---------------------------------------------------------------------------

/// A provider row as it will be written.
struct Draft {
    kind: String,
    name: String,
    is_enabled: bool,
    sort_order: i32,
    ca_certificate: Option<String>,
    issuer_url: Option<String>,
    client_id: Option<String>,
    client_secret: Option<String>,
    scopes: Option<String>,
    username_claim: Option<String>,
    groups_claim: Option<String>,
    mfa_assurance: Option<String>,
    required_acr: Option<Vec<String>>,
    ldap_url: Option<String>,
    start_tls: Option<bool>,
    bind_dn: Option<String>,
    bind_password: Option<String>,
    user_base_dn: Option<String>,
    user_filter: Option<String>,
    username_attribute: Option<String>,
    display_name_attribute: Option<String>,
    email_attribute: Option<String>,
    group_attribute: Option<String>,
}

impl Draft {
    fn from_row(r: &ProviderRow) -> Draft {
        Draft {
            kind: r.kind.clone(),
            name: r.name.clone(),
            is_enabled: r.is_enabled,
            sort_order: r.sort_order,
            ca_certificate: r.ca_certificate.clone(),
            issuer_url: r.issuer_url.clone(),
            client_id: r.client_id.clone(),
            client_secret: r.client_secret.clone(),
            scopes: r.scopes.clone(),
            username_claim: r.username_claim.clone(),
            groups_claim: r.groups_claim.clone(),
            mfa_assurance: r.mfa_assurance.clone(),
            required_acr: r.required_acr.clone(),
            ldap_url: r.ldap_url.clone(),
            start_tls: r.start_tls,
            bind_dn: r.bind_dn.clone(),
            bind_password: r.bind_password.clone(),
            user_base_dn: r.user_base_dn.clone(),
            user_filter: r.user_filter.clone(),
            username_attribute: r.username_attribute.clone(),
            display_name_attribute: r.display_name_attribute.clone(),
            email_attribute: r.email_attribute.clone(),
            group_attribute: r.group_attribute.clone(),
        }
    }

    fn from_create(b: &IdentityProviderCreate) -> Draft {
        let mut d = Draft {
            kind: b.kind.as_str().to_owned(),
            name: b.name.clone(),
            is_enabled: b.is_enabled.unwrap_or(true),
            sort_order: b.sort_order.unwrap_or(0),
            ca_certificate: b.ca_certificate.clone().filter(|p| !p.trim().is_empty()),
            issuer_url: None,
            client_id: None,
            client_secret: None,
            scopes: None,
            username_claim: None,
            groups_claim: None,
            mfa_assurance: None,
            required_acr: None,
            ldap_url: None,
            start_tls: None,
            bind_dn: None,
            bind_password: None,
            user_base_dn: None,
            user_filter: None,
            username_attribute: None,
            display_name_attribute: None,
            email_attribute: None,
            group_attribute: None,
        };
        if let Some(o) = &b.oidc {
            d.issuer_url = Some(o.issuer_url.clone());
            d.client_id = Some(o.client_id.clone());
            d.client_secret = o.client_secret.clone().filter(|s| !s.is_empty());
            d.scopes = Some(o.scopes.clone().unwrap_or_else(|| "profile email".into()));
            d.username_claim = Some(o.username_claim.clone().unwrap_or_else(|| "preferred_username".into()));
            d.groups_claim = Some(o.groups_claim.clone().unwrap_or_else(|| "groups".into()));
            // Secure default: the token must prove MFA.
            d.mfa_assurance = Some(o.mfa_assurance.unwrap_or(MfaAssurance::Verify).as_db().to_owned());
            d.required_acr = Some(distinct(o.required_acr.as_deref().unwrap_or_default()));
        }
        if let Some(l) = &b.ldap {
            d.start_tls = Some(l.start_tls.unwrap_or_else(|| !l.url.to_ascii_lowercase().starts_with("ldaps://")));
            d.ldap_url = Some(l.url.clone());
            d.bind_dn = l.bind_dn.clone().filter(|s| !s.is_empty());
            d.bind_password = l.bind_password.clone().filter(|s| !s.is_empty());
            d.user_base_dn = Some(l.user_base_dn.clone());
            d.user_filter = Some(
                l.user_filter.clone().unwrap_or_else(|| "(&(objectClass=user)(sAMAccountName={username}))".into()),
            );
            d.username_attribute = Some(l.username_attribute.clone().unwrap_or_else(|| "sAMAccountName".into()));
            d.display_name_attribute = Some(l.display_name_attribute.clone().unwrap_or_else(|| "displayName".into()));
            d.email_attribute = Some(l.email_attribute.clone().unwrap_or_else(|| "mail".into()));
            d.group_attribute = Some(l.group_attribute.clone().unwrap_or_else(|| "memberOf".into()));
        }
        d
    }

    fn apply(&mut self, b: &IdentityProviderUpdate) -> Vec<FieldError> {
        let mut errors = Vec::new();
        if let Some(v) = &b.name {
            self.name = v.clone();
        }
        if let Some(v) = b.is_enabled {
            self.is_enabled = v;
        }
        if let Some(v) = b.sort_order {
            self.sort_order = v;
        }
        if let Some(v) = &b.ca_certificate {
            self.ca_certificate = v.clone().filter(|p| !p.trim().is_empty());
        }
        if let Some(o) = &b.oidc {
            if self.kind != OIDC {
                errors.push(body_error("oidc", "Only for kind oidc", "not_allowed"));
            } else {
                set(&mut self.issuer_url, &o.issuer_url);
                set(&mut self.client_id, &o.client_id);
                if let Some(secret) = &o.client_secret {
                    self.client_secret = secret.clone().filter(|s| !s.is_empty());
                }
                set(&mut self.scopes, &o.scopes);
                set(&mut self.username_claim, &o.username_claim);
                set(&mut self.groups_claim, &o.groups_claim);
                if let Some(m) = o.mfa_assurance {
                    self.mfa_assurance = Some(m.as_db().to_owned());
                    // Trust takes no acr values: switching to it drops them unless they are sent too.
                    if m == MfaAssurance::TrustProvider && o.required_acr.is_none() {
                        self.required_acr = Some(Vec::new());
                    }
                }
                if let Some(acr) = &o.required_acr {
                    self.required_acr = Some(distinct(acr));
                }
            }
        }
        if let Some(l) = &b.ldap {
            if self.kind != LDAP {
                errors.push(body_error("ldap", "Only for kind ldap", "not_allowed"));
            } else {
                if let Some(url) = &l.url {
                    // A changed scheme brings its own default, unless startTls is given too.
                    if l.start_tls.is_none() {
                        self.start_tls = Some(!url.to_ascii_lowercase().starts_with("ldaps://"));
                    }
                    self.ldap_url = Some(url.clone());
                }
                if let Some(v) = l.start_tls {
                    self.start_tls = Some(v);
                }
                if let Some(dn) = &l.bind_dn {
                    self.bind_dn = dn.clone().filter(|s| !s.is_empty());
                    if self.bind_dn.is_none() {
                        self.bind_password = None;
                    }
                }
                if let Some(pw) = &l.bind_password {
                    self.bind_password = pw.clone().filter(|s| !s.is_empty());
                }
                set(&mut self.user_base_dn, &l.user_base_dn);
                set(&mut self.user_filter, &l.user_filter);
                set(&mut self.username_attribute, &l.username_attribute);
                set(&mut self.display_name_attribute, &l.display_name_attribute);
                set(&mut self.email_attribute, &l.email_attribute);
                set(&mut self.group_attribute, &l.group_attribute);
            }
        }
        errors
    }

    /// The rules the check constraints also enforce, as field errors.
    fn problems(&self) -> Vec<FieldError> {
        let mut errors = Vec::new();
        if let Some(pem) = &self.ca_certificate
            && let Err(e) = tls::parse_ca_pem(pem)
        {
            errors.push(body_error("caCertificate", format!("The CA certificate {e}"), "invalid_certificate"));
        }
        if let Some(issuer) = &self.issuer_url
            && let Some(message) = issuer_problem(issuer)
        {
            errors.push(body_error("oidc.issuerUrl", message, "invalid_url"));
        }
        if let Some(url) = &self.ldap_url
            && let Some((field, message)) = ldap_url_problem(url, self.start_tls.unwrap_or(true))
        {
            errors.push(body_error(field, message, "invalid_url"));
        }
        if self.bind_dn.is_some() != self.bind_password.is_some() {
            let message =
                if self.bind_dn.is_some() { "A bind DN needs its password" } else { "A bind password needs a bind DN" };
            errors.push(body_error("ldap.bindPassword", message, "required"));
        }
        if self.mfa_assurance.as_deref() == Some(oidc::MfaPolicy::TRUST_PROVIDER)
            && self.required_acr.as_ref().is_some_and(|a| !a.is_empty())
        {
            errors.push(body_error(
                "oidc.requiredAcr",
                "Only with mfaAssurance verify: a trusted provider's token is not checked",
                "not_allowed",
            ));
        }
        if let Some(filter) = &self.user_filter
            && (!filter.contains("{username}") || !filter.starts_with('(') || !filter.ends_with(')'))
        {
            errors.push(body_error(
                "ldap.userFilter",
                "Must be an LDAP filter in parentheses containing {username}",
                "invalid_filter",
            ));
        }
        errors
    }
}

/// The values in order, each once.
fn distinct(values: &[String]) -> Vec<String> {
    let mut seen = HashSet::new();
    values.iter().filter(|v| seen.insert(v.as_str())).cloned().collect()
}

fn set(target: &mut Option<String>, value: &Option<String>) {
    if let Some(v) = value {
        *target = Some(v.clone());
    }
}

/// What the discovery document suggests about the MFA check (warnings only:
/// discovery need not list every claim or acr value the provider sends).
fn mfa_warnings(policy: &oidc::MfaPolicy, d: &oidc::Discovery) -> Vec<String> {
    let oidc::MfaPolicy::Verify { required_acr } = policy else {
        return vec![
            "MFA: trusted, not verified. ShadouCMDB does not check that the provider used a second factor".into(),
        ];
    };
    let mut out = Vec::new();
    if required_acr.is_empty() {
        if let Some(claims) = &d.claims_supported
            && !claims.iter().any(|c| c == "amr")
        {
            out.push(
                "Warning: the provider does not list amr in claims_supported. Without amr in the ID token, users \
                 whose profiles require MFA are refused; set requiredAcr, or check the provider's token settings"
                    .into(),
            );
        }
    } else if let Some(supported) = &d.acr_values_supported {
        for acr in required_acr.iter().filter(|a| !supported.contains(a)) {
            out.push(format!(
                "Warning: requiredAcr value {acr:?} is not in the provider's acr_values_supported ({})",
                supported.join(", ")
            ));
        }
    }
    out
}

/// https, or http to a loopback address; no credentials, query or fragment.
fn issuer_problem(issuer: &str) -> Option<&'static str> {
    let Ok(url) = url::Url::parse(issuer) else { return Some("Not a URL") };
    if !url.username().is_empty() || url.password().is_some() || url.query().is_some() || url.fragment().is_some() {
        return Some("Must not contain credentials, a query or a fragment");
    }
    match url.scheme() {
        "https" if url.host().is_some() => None,
        "http" if oidc::is_loopback(&url) => None,
        "http" => Some("Must be https (plain http only for a test issuer on this host)"),
        _ => Some("Must be an https URL"),
    }
}

/// ldaps://host[:port], or ldap://host[:port] with StartTLS; returns the field and message.
fn ldap_url_problem(url: &str, start_tls: bool) -> Option<(&'static str, &'static str)> {
    let lower = url.to_ascii_lowercase();
    let secure = lower.starts_with("ldaps://");
    if !secure && !lower.starts_with("ldap://") {
        return Some(("ldap.url", "Must start with ldaps:// or ldap://"));
    }
    let rest = &url[if secure { 8 } else { 7 }..];
    let host = rest.strip_suffix('/').unwrap_or(rest);
    if host.is_empty() || host.contains(['/', '?', '#', '@', ' ']) {
        return Some(("ldap.url", "Must be ldaps://host[:port] or ldap://host[:port] with no path"));
    }
    match (secure, start_tls) {
        (true, true) => Some(("ldap.startTls", "ldaps:// is already encrypted; set startTls to false")),
        (false, false) => Some(("ldap.startTls", "Plain LDAP is not allowed: use ldaps:// or set startTls to true")),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Service
// ---------------------------------------------------------------------------

fn administrator_only(ctx: &RequestContext) -> Result<(), AppError> {
    match ctx.principal() {
        Some(p) if !p.permissions.administrator => {
            Err(forbidden("Only holders of the Administrator profile can manage identity providers"))
        }
        _ => Ok(()),
    }
}

fn dto(auth: &AuthState, r: ProviderRow, mappings: &[data::MappingRow]) -> IdentityProvider {
    let kind = if r.kind == LDAP { ProviderKind::Ldap } else { ProviderKind::Oidc };
    let oidc = (kind == ProviderKind::Oidc).then(|| OidcConfig {
        issuer_url: r.issuer_url.clone().unwrap_or_default(),
        client_id: r.client_id.clone().unwrap_or_default(),
        client_secret_set: r.client_secret.is_some(),
        scopes: r.scopes.clone().unwrap_or_default(),
        username_claim: r.username_claim.clone().unwrap_or_default(),
        groups_claim: r.groups_claim.clone().unwrap_or_default(),
        redirect_uri: sso::redirect_uri(auth),
        mfa_assurance: MfaAssurance::from_db(r.mfa_assurance.as_deref()),
        required_acr: r.required_acr.clone().unwrap_or_default(),
    });
    let ldap = (kind == ProviderKind::Ldap).then(|| LdapConfig {
        url: r.ldap_url.clone().unwrap_or_default(),
        start_tls: r.start_tls.unwrap_or(true),
        bind_dn: r.bind_dn.clone(),
        bind_password_set: r.bind_password.is_some(),
        user_base_dn: r.user_base_dn.clone().unwrap_or_default(),
        user_filter: r.user_filter.clone().unwrap_or_default(),
        username_attribute: r.username_attribute.clone().unwrap_or_default(),
        display_name_attribute: r.display_name_attribute.clone().unwrap_or_default(),
        email_attribute: r.email_attribute.clone().unwrap_or_default(),
        group_attribute: r.group_attribute.clone().unwrap_or_default(),
    });
    IdentityProvider {
        group_mappings: mappings
            .iter()
            .filter(|m| m.provider_id == r.id)
            .map(|m| GroupMapping {
                group: m.group_name.clone(),
                profile_id: m.profile_id,
                profile_name: m.profile_name.clone(),
            })
            .collect(),
        id: r.id,
        kind,
        name: r.name,
        is_enabled: r.is_enabled,
        sort_order: r.sort_order,
        ca_certificate: r.ca_certificate,
        oidc,
        ldap,
        user_count: r.user_count,
        created_at: r.created_at,
        updated_at: r.updated_at,
    }
}

async fn load(
    conn: &mut PgConnection,
    auth: &AuthState,
    id: Uuid,
    for_update: bool,
) -> Result<IdentityProvider, AppError> {
    let row = data::get(conn, id, for_update).await?.ok_or_else(|| AppError::missing("Identity provider", id))?;
    let mappings = data::mappings(conn, &[id]).await?;
    Ok(dto(auth, row, &mappings))
}

pub async fn list(pool: &PgPool, auth: &AuthState, ctx: &RequestContext) -> Result<Vec<IdentityProvider>, AppError> {
    administrator_only(ctx)?;
    let mut conn = pool.acquire().await?;
    let rows = data::list(&mut conn).await?;
    let ids: Vec<Uuid> = rows.iter().map(|r| r.id).collect();
    let mappings = data::mappings(&mut conn, &ids).await?;
    Ok(rows.into_iter().map(|r| dto(auth, r, &mappings)).collect())
}

async fn check_mappings(
    conn: &mut PgConnection,
    mappings: &[GroupMappingInput],
) -> Result<Vec<(String, Uuid)>, AppError> {
    let ids: Vec<Uuid> = mappings.iter().map(|m| m.profile_id).collect();
    let found: HashSet<Uuid> = auth_data::existing_profiles(conn, &ids).await?.into_iter().collect();
    let mut errors = Vec::new();
    let mut seen = HashSet::new();
    let mut pairs = Vec::new();
    for (i, m) in mappings.iter().enumerate() {
        if !found.contains(&m.profile_id) {
            errors.push(body_error(
                &format!("groupMappings.{i}.profileId"),
                "Permission profile does not exist",
                "not_found",
            ));
        }
        if seen.insert((m.group.to_lowercase(), m.profile_id)) {
            pairs.push((m.group.clone(), m.profile_id));
        }
    }
    if errors.is_empty() { Ok(pairs) } else { Err(AppError::validation(errors)) }
}

async fn write(conn: &mut PgConnection, id: Option<Uuid>, d: &Draft) -> Result<Uuid, AppError> {
    let sql = match id {
        None => {
            "INSERT INTO identity_providers (kind, name, is_enabled, sort_order, ca_certificate,
               issuer_url, client_id, client_secret, scopes, username_claim, groups_claim,
               ldap_url, start_tls, bind_dn, bind_password, user_base_dn, user_filter,
               username_attribute, display_name_attribute, email_attribute, group_attribute,
               mfa_assurance, required_acr)
             VALUES ($2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19, $20, $21, $22,
               $23, $24)
             RETURNING id"
        }
        Some(_) => {
            "UPDATE identity_providers SET kind = $2, name = $3, is_enabled = $4, sort_order = $5, ca_certificate = $6,
               issuer_url = $7, client_id = $8, client_secret = $9, scopes = $10, username_claim = $11,
               groups_claim = $12, ldap_url = $13, start_tls = $14, bind_dn = $15, bind_password = $16,
               user_base_dn = $17, user_filter = $18, username_attribute = $19, display_name_attribute = $20,
               email_attribute = $21, group_attribute = $22, mfa_assurance = $23, required_acr = $24
             WHERE id = $1 RETURNING id"
        }
    };
    Ok(sqlx::query_scalar(sql)
        .bind(id)
        .bind(&d.kind)
        .bind(&d.name)
        .bind(d.is_enabled)
        .bind(d.sort_order)
        .bind(&d.ca_certificate)
        .bind(&d.issuer_url)
        .bind(&d.client_id)
        .bind(&d.client_secret)
        .bind(&d.scopes)
        .bind(&d.username_claim)
        .bind(&d.groups_claim)
        .bind(&d.ldap_url)
        .bind(d.start_tls)
        .bind(&d.bind_dn)
        .bind(&d.bind_password)
        .bind(&d.user_base_dn)
        .bind(&d.user_filter)
        .bind(&d.username_attribute)
        .bind(&d.display_name_attribute)
        .bind(&d.email_attribute)
        .bind(&d.group_attribute)
        .bind(&d.mfa_assurance)
        .bind(&d.required_acr)
        .fetch_one(conn)
        .await?)
}

fn audit(action: AuditAction, id: Uuid, old: Option<&IdentityProvider>, new: Option<&IdentityProvider>) -> AuditEntry {
    AuditEntry {
        action,
        entity_type: TABLE,
        entity_id: id,
        old_value: old.map(crud::json),
        new_value: new.map(crud::json),
    }
}

/// Ends the sessions of the provider's accounts (disabled or deleted provider).
async fn end_sessions(conn: &mut PgConnection, ctx: &RequestContext, provider_id: Uuid) -> Result<usize, AppError> {
    let mut ended = 0;
    for user_id in data::user_ids(conn, provider_id).await? {
        let sessions = auth_data::delete_user_sessions(conn, user_id, None).await?;
        events::revoked(conn, ctx, &sessions, RevokeReason::ProviderDisabled).await?;
        ended += sessions.len();
    }
    Ok(ended)
}

pub async fn create(
    pool: &PgPool,
    auth: &AuthState,
    ctx: &RequestContext,
    b: &IdentityProviderCreate,
) -> Result<IdentityProvider, AppError> {
    administrator_only(ctx)?;
    let draft = Draft::from_create(b);
    let problems = draft.problems();
    if !problems.is_empty() {
        return Err(AppError::validation(problems));
    }
    let mut tx = pool.begin().await?;
    let pairs = check_mappings(&mut tx, &b.group_mappings).await?;
    let id = write(&mut tx, None, &draft).await?;
    data::set_mappings(&mut tx, id, &pairs).await?;
    let created = load(&mut tx, auth, id, false).await?;
    crud::write_audit(&mut tx, ctx, vec![audit(AuditAction::Create, id, None, Some(&created))]).await?;
    tx.commit().await?;
    Ok(created)
}

pub async fn update(
    pool: &PgPool,
    auth: &AuthState,
    ctx: &RequestContext,
    id: Uuid,
    b: &IdentityProviderUpdate,
) -> Result<IdentityProvider, AppError> {
    administrator_only(ctx)?;
    let mut tx = pool.begin().await?;
    let row = data::get(&mut tx, id, true).await?.ok_or_else(|| AppError::missing("Identity provider", id))?;
    let before = load(&mut tx, auth, id, false).await?;
    let mut draft = Draft::from_row(&row);
    let mut problems = draft.apply(b);
    problems.extend(draft.problems());
    if !problems.is_empty() {
        return Err(AppError::validation(problems));
    }
    write(&mut tx, Some(id), &draft).await?;
    if let Some(mappings) = &b.group_mappings {
        let pairs = check_mappings(&mut tx, mappings).await?;
        data::set_mappings(&mut tx, id, &pairs).await?;
    }
    if row.is_enabled && !draft.is_enabled {
        let ended = end_sessions(&mut tx, ctx, id).await?;
        tracing::info!(provider = %row.name, ended_sessions = ended, "identity provider disabled");
    }
    let after = load(&mut tx, auth, id, false).await?;
    crud::write_audit(&mut tx, ctx, vec![audit(AuditAction::Update, id, Some(&before), Some(&after))]).await?;
    tx.commit().await?;
    auth.oidc.forget(id);
    Ok(after)
}

pub async fn remove(pool: &PgPool, auth: &AuthState, ctx: &RequestContext, id: Uuid) -> Result<(), AppError> {
    administrator_only(ctx)?;
    let mut tx = pool.begin().await?;
    let before = load(&mut tx, auth, id, true).await?;
    if before.user_count > 0 {
        return Err(AppError::new(
            ErrorCode::InUse,
            format!(
                "{} account(s) sign in through this provider. Disable the provider instead, or delete those accounts first.",
                before.user_count
            ),
        ));
    }
    data::delete(&mut tx, id).await?;
    crud::write_audit(&mut tx, ctx, vec![audit(AuditAction::Delete, id, Some(&before), None)]).await?;
    tx.commit().await?;
    auth.oidc.forget(id);
    Ok(())
}

async fn test(
    pool: &PgPool,
    auth: &AuthState,
    ctx: &RequestContext,
    id: Uuid,
    b: &ConnectionTestInput,
) -> Result<ConnectionTest, AppError> {
    administrator_only(ctx)?;
    let mut conn = pool.acquire().await?;
    let row = data::get(&mut conn, id, false).await?.ok_or_else(|| AppError::missing("Identity provider", id))?;
    drop(conn);
    if row.kind == OIDC {
        auth.oidc.forget(id);
        let settings = sso::oidc_settings(&row);
        return Ok(match auth.oidc.provider(id, &row.updated_at.to_rfc3339(), &settings).await {
            Ok(p) => {
                let mut details = vec![
                    format!("Issuer: {}", p.discovery.issuer),
                    format!("Authorization endpoint: {}", p.discovery.authorization_endpoint),
                    format!("Token endpoint: {}", p.discovery.token_endpoint),
                    format!("Key set: {}", p.discovery.jwks_uri),
                ];
                if let Some(algs) = &p.discovery.id_token_signing_alg_values_supported {
                    details.push(format!("ID token algorithms: {}", algs.join(", ")));
                }
                details.extend(mfa_warnings(&settings.mfa, &p.discovery));
                match sso::redirect_uri(auth) {
                    Some(uri) => details.push(format!("Redirect URI to register at the provider: {uri}")),
                    None => {
                        details.push("PUBLIC_URL is not set: sign-in through this provider is not possible yet".into())
                    }
                }
                let ok = auth.config.public_url.is_some();
                let message = if ok {
                    "The provider's discovery document and key set were read"
                } else {
                    "The provider answered, but PUBLIC_URL is not set on the server"
                };
                ConnectionTest { ok, message: message.into(), details, user: None }
            }
            Err(e) => {
                tracing::warn!(provider = %row.name, error = %e, "identity provider connection test failed");
                ConnectionTest { ok: false, message: e.summary().into(), details: Vec::new(), user: None }
            }
        });
    }
    let settings = sso::ldap_settings(&row);
    Ok(match ldap::probe(&settings, b.username.as_deref()).await {
        Ok(None) => ConnectionTest {
            ok: true,
            message: "Connected over TLS and the service account bind succeeded".into(),
            details: Vec::new(),
            user: None,
        },
        Ok(Some(Ok(u))) => {
            let profiles = {
                let mut conn = pool.acquire().await?;
                let ids = data::profiles_for_groups(&mut conn, id, &u.groups).await?;
                data::mappings(&mut conn, &[id])
                    .await?
                    .into_iter()
                    .filter(|m| ids.contains(&m.profile_id))
                    .map(|m| m.profile_name)
                    .collect::<std::collections::BTreeSet<_>>()
                    .into_iter()
                    .collect::<Vec<_>>()
            };
            let ok = !profiles.is_empty();
            let message = if ok {
                "The user was found and would get the profiles listed"
            } else {
                "The user was found, but none of their groups maps to a profile: their sign-in would be refused"
            };
            ConnectionTest {
                ok,
                message: message.into(),
                details: Vec::new(),
                user: Some(DirectoryUserPreview {
                    dn: u.dn,
                    username: u.username,
                    display_name: u.display_name,
                    email: u.email,
                    groups: u.groups,
                    profiles,
                }),
            }
        }
        Ok(Some(Err(n))) => ConnectionTest {
            ok: false,
            message: if n == 0 {
                "Connected, but the user filter found no entry for this name".into()
            } else {
                format!("Connected, but the user filter found {n} entries for this name; it must find exactly one")
            },
            details: Vec::new(),
            user: None,
        },
        Err(e) => {
            tracing::warn!(provider = %row.name, error = %e, "identity provider connection test failed");
            ConnectionTest { ok: false, message: e.summary().into(), details: Vec::new(), user: None }
        }
    })
}

// ---------------------------------------------------------------------------
// Routes
// ---------------------------------------------------------------------------

const ROUTE_TAG: &str = "Identity providers";
const BASE: &str = "/api/v1/admin/identity-providers";
const BY_ID: &str = "/api/v1/admin/identity-providers/{id}";
const ADMIN_ONLY: &str = "Only for holders of the built-in Administrator profile (403 otherwise).";

pub fn routes() -> Vec<Route> {
    let manage = GlobalPermission::UsersManage;
    vec![
        route(Method::GET, BASE, "listIdentityProviders")
            .tag(ROUTE_TAG)
            .summary("List OIDC providers and LDAP/AD directories with their group mappings (secrets are never returned)")
            .description(ADMIN_ONLY)
            .requires(manage)
            .handle(|api, In(NoPath, NoQuery, NoBody): In<NoPath, NoQuery, NoBody>| async move {
                Ok(Json(list(&api.pool, &api.auth, &api.ctx).await?))
            }),
        route(Method::GET, BY_ID, "getIdentityProvider")
            .tag(ROUTE_TAG)
            .summary("Get one identity provider")
            .description(ADMIN_ONLY)
            .requires(manage)
            .errors(&[ErrorCode::NotFound])
            .handle(|api, In(IdPath(id), NoQuery, NoBody): In<IdPath, NoQuery, NoBody>| async move {
                administrator_only(&api.ctx)?;
                Ok(Json(load(&mut *api.pool.acquire().await?, &api.auth, id, false).await?))
            }),
        route(Method::POST, BASE, "createIdentityProvider")
            .tag(ROUTE_TAG)
            .summary("Add an OIDC provider or an LDAP/AD directory")
            .description(format!(
                "{ADMIN_ONLY} OIDC: the issuer must be https (http only for a test issuer on this host); register `oidc.redirectUri` of the response at the provider. `oidc.mfaAssurance` defaults to `verify`: users whose profiles require MFA must then prove a second factor in the ID token (`amr`, or `acr` in `requiredAcr`) or are refused. LDAP: ldaps://, or ldap:// with StartTLS; certificates are always verified (add a private CA with `caCertificate`). Users signing in get the profiles their groups map to; with no matching mapping they are refused."
            ))
            .status(StatusCode::CREATED)
            .requires(manage)
            .session_only()
            .errors(&[ErrorCode::Conflict])
            .handle(|api, In(NoPath, NoQuery, Body(b)): In<NoPath, NoQuery, Body<IdentityProviderCreate>>| async move {
                Ok(Json(create(&api.pool, &api.auth, &api.ctx, &b).await?))
            }),
        route(Method::PATCH, BY_ID, "updateIdentityProvider")
            .tag(ROUTE_TAG)
            .summary("Change an identity provider (partial); groupMappings replaces all mappings")
            .description(format!(
                "{ADMIN_ONLY} The kind cannot change. Secrets: a string replaces, null removes, left out keeps. `isEnabled: false` stops sign-ins through the provider and ends the sessions of its accounts. `oidc.mfaAssurance: trustProvider` without `oidc.requiredAcr` also empties `requiredAcr` (400 when both are sent with values). Switching to `verify` ends, on their next request, the sessions whose sign-in did not prove MFA for users whose profiles require it."
            ))
            .requires(manage)
            .session_only()
            .errors(&[ErrorCode::NotFound, ErrorCode::Conflict])
            .handle(|api, In(IdPath(id), NoQuery, Body(b)): In<IdPath, NoQuery, Body<IdentityProviderUpdate>>| async move {
                Ok(Json(update(&api.pool, &api.auth, &api.ctx, id, &b).await?))
            }),
        route(Method::DELETE, BY_ID, "deleteIdentityProvider")
            .tag(ROUTE_TAG)
            .summary("Delete an identity provider that no account signs in through")
            .description(format!("{ADMIN_ONLY} 409 IN_USE while accounts belong to it: disable it instead."))
            .requires(manage)
            .session_only()
            .errors(&[ErrorCode::NotFound, ErrorCode::InUse])
            .handle(|api, In(IdPath(id), NoQuery, NoBody): In<IdPath, NoQuery, NoBody>| async move {
                remove(&api.pool, &api.auth, &api.ctx, id).await?;
                Ok(NoContent)
            }),
        route(Method::POST, "/api/v1/admin/identity-providers/{id}/test", "testIdentityProvider")
            .tag(ROUTE_TAG)
            .summary("Check the saved settings against the provider (OIDC discovery and keys; LDAP TLS, bind and a user lookup)")
            .description(format!(
                "{ADMIN_ONLY} Answers 200 with `ok: false` and the reason when the provider cannot be used; nothing is changed. When no answer came back over verified TLS (connection, TLS or StartTLS failed), the message is the same whatever the cause and the details go to the server log only. For a directory, `username` looks a user up with the service account (no password) and shows the groups and the profiles they map to. For OIDC, `details` also warns when the MFA check is unlikely to work: `verify` without `requiredAcr` while the discovery document's `claims_supported` omits `amr`, or a `requiredAcr` value missing from `acr_values_supported`."
            ))
            .requires(manage)
            .errors(&[ErrorCode::NotFound])
            .handle(|api, In(IdPath(id), NoQuery, Body(b)): In<IdPath, NoQuery, Body<ConnectionTestInput>>| async move {
                Ok(Json(test(&api.pool, &api.auth, &api.ctx, id, &b).await?))
            }),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issuers_are_https_or_loopback_http() {
        assert_eq!(issuer_problem("https://login.example.com/tenant/v2.0"), None);
        assert_eq!(issuer_problem("http://127.0.0.1:8080/realms/test"), None);
        assert_eq!(issuer_problem("http://localhost:8080"), None);
        assert!(issuer_problem("http://idp.example.com").is_some());
        assert!(issuer_problem("https://u:p@idp.example.com").is_some());
        assert!(issuer_problem("https://idp.example.com/?x=1").is_some());
        assert!(issuer_problem("ftp://idp.example.com").is_some());
        assert!(issuer_problem("idp.example.com").is_some());
    }

    #[test]
    fn mfa_warnings_come_from_discovery() {
        let discovery = |claims: Option<&[&str]>, acr: Option<&[&str]>| oidc::Discovery {
            issuer: "https://idp.example.test".into(),
            authorization_endpoint: "https://idp.example.test/authorize".into(),
            token_endpoint: "https://idp.example.test/token".into(),
            jwks_uri: "https://idp.example.test/jwks".into(),
            token_endpoint_auth_methods_supported: None,
            id_token_signing_alg_values_supported: None,
            claims_supported: claims.map(|c| c.iter().map(|s| s.to_string()).collect()),
            acr_values_supported: acr.map(|c| c.iter().map(|s| s.to_string()).collect()),
        };
        let amr = oidc::MfaPolicy::Verify { required_acr: Vec::new() };
        assert_eq!(mfa_warnings(&amr, &discovery(Some(&["sub", "amr"]), None)), Vec::<String>::new());
        assert_eq!(mfa_warnings(&amr, &discovery(None, None)), Vec::<String>::new(), "no list, no guess");
        let w = mfa_warnings(&amr, &discovery(Some(&["sub", "email"]), None));
        assert_eq!(w.len(), 1);
        assert!(w[0].contains("amr"));

        let acr = oidc::MfaPolicy::Verify { required_acr: vec!["gold".into(), "silver".into()] };
        let w = mfa_warnings(&acr, &discovery(Some(&["sub"]), Some(&["gold", "bronze"])));
        assert_eq!(w.len(), 1);
        assert!(w[0].contains("\"silver\""));
        assert_eq!(mfa_warnings(&acr, &discovery(None, None)), Vec::<String>::new());
        assert!(mfa_warnings(&oidc::MfaPolicy::TrustProvider, &discovery(None, None))[0].contains("not verified"));
    }

    #[test]
    fn directories_are_ldaps_or_starttls() {
        assert_eq!(ldap_url_problem("ldaps://dc1.example.com:636", false), None);
        assert_eq!(ldap_url_problem("ldap://dc1.example.com", true), None);
        assert_eq!(ldap_url_problem("LDAPS://dc1.example.com/", false), None);
        assert_eq!(ldap_url_problem("ldap://dc1.example.com", false).map(|p| p.0), Some("ldap.startTls"));
        assert_eq!(ldap_url_problem("ldaps://dc1.example.com", true).map(|p| p.0), Some("ldap.startTls"));
        assert_eq!(ldap_url_problem("http://dc1.example.com", true).map(|p| p.0), Some("ldap.url"));
        assert_eq!(ldap_url_problem("ldaps://dc1.example.com/dc=x", false).map(|p| p.0), Some("ldap.url"));
        assert_eq!(ldap_url_problem("ldaps://", false).map(|p| p.0), Some("ldap.url"));
    }
}
