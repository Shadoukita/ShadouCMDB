//! Every failure leaves the API in one envelope:
//!
//! ```json
//! { "error": { "code": "VALIDATION_ERROR", "message": "...", "details": [...], "requestId": "..." } }
//! ```
//!
//! `code` is machine-readable and stable; `message` is for humans; `details`
//! carries per-field problems (`in` says where the field lives). Same contract
//! as the `ErrorEnvelope` schema in backend/openapi.json.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;

use super::request_id;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    ValidationError,
    /// No valid session (or wrong credentials on login)
    Unauthenticated,
    /// Signed in, but a permission is missing
    Forbidden,
    /// State-changing request without the session's X-CSRF-Token
    CsrfTokenInvalid,
    NotFound,
    Conflict,
    InUse,
    VersionConflict,
    /// The operation was removed; the message names its replacement (410)
    Gone,
    /// A technical name (area, type or field) is malformed, reserved or already taken (422)
    InvalidName,
    /// A data model change was refused because it would lose or break stored data (422)
    SchemaChangeRefused,
    /// A stored identity provider secret must be entered again: the patch changes where it would be sent (422)
    SecretRequired,
    /// An `Idempotency-Key` sent again for another operation or another target (422)
    IdempotencyKeyReused,
    /// The change would leave no active user holding the Administrator profile
    LastAdministrator,
    /// Too many failed password attempts, or too many impact analyses of one user in progress; retry after the
    /// Retry-After header
    RateLimited,
    /// The password was right; send the authenticator or recovery code to POST /api/v1/auth/login/mfa (401)
    MfaRequired,
    /// A profile the user holds requires MFA; until it is set up only the MFA set-up routes answer (403)
    MfaEnrolmentRequired,
    /// The token's owner must use two-factor authentication, and the session creating it did not sign in with a
    /// second factor (403)
    MfaRequiredForToken,
    /// The LDAP directory (or OIDC provider) could not be reached; local accounts still sign in (503)
    IdentityProviderUnavailable,
    UnsupportedMediaType,
    PayloadTooLarge,
    /// The request was not completed within HTTP_REQUEST_TIMEOUT_SECS
    RequestTimeout,
    DatabaseUnavailable,
    /// HTTP_MAX_CONCURRENT_REQUESTS requests (or the anonymous body budget) are in use; retry after the Retry-After header (503)
    ServerBusy,
    /// The database has migrations pending; run `shadoucmdb migrate` (503)
    SchemaNotMigrated,
    InternalError,
}

impl ErrorCode {
    pub fn status(self) -> StatusCode {
        match self {
            ErrorCode::ValidationError => StatusCode::BAD_REQUEST,
            ErrorCode::Unauthenticated | ErrorCode::MfaRequired => StatusCode::UNAUTHORIZED,
            ErrorCode::Forbidden
            | ErrorCode::CsrfTokenInvalid
            | ErrorCode::MfaEnrolmentRequired
            | ErrorCode::MfaRequiredForToken => StatusCode::FORBIDDEN,
            ErrorCode::NotFound => StatusCode::NOT_FOUND,
            ErrorCode::Gone => StatusCode::GONE,
            ErrorCode::Conflict | ErrorCode::InUse | ErrorCode::VersionConflict | ErrorCode::LastAdministrator => {
                StatusCode::CONFLICT
            }
            ErrorCode::InvalidName
            | ErrorCode::SchemaChangeRefused
            | ErrorCode::SecretRequired
            | ErrorCode::IdempotencyKeyReused => StatusCode::UNPROCESSABLE_ENTITY,
            ErrorCode::RateLimited => StatusCode::TOO_MANY_REQUESTS,
            ErrorCode::UnsupportedMediaType => StatusCode::UNSUPPORTED_MEDIA_TYPE,
            ErrorCode::PayloadTooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            ErrorCode::RequestTimeout => StatusCode::REQUEST_TIMEOUT,
            ErrorCode::DatabaseUnavailable
            | ErrorCode::ServerBusy
            | ErrorCode::SchemaNotMigrated
            | ErrorCode::IdentityProviderUnavailable => StatusCode::SERVICE_UNAVAILABLE,
            ErrorCode::InternalError => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum FieldLocation {
    Body,
    Query,
    Params,
    // Part of the published contract; used once headers are validated (auth).
    #[allow(dead_code)]
    Header,
}

#[derive(Debug, Clone, Serialize)]
pub struct FieldError {
    #[serde(rename = "in")]
    pub location: FieldLocation,
    /// Dotted path, e.g. "attributes.cpu_cores" or "limit".
    pub field: String,
    pub message: String,
    /// Machine-readable reason, e.g. "invalid_type", "required", "unique".
    pub code: String,
}

#[derive(Debug, Clone, thiserror::Error)]
#[error("{code:?}: {message}")]
pub struct AppError {
    pub code: ErrorCode,
    pub message: String,
    pub details: Option<Vec<FieldError>>,
    /// Seconds, sent as Retry-After (RATE_LIMITED, SERVER_BUSY).
    pub retry_after: Option<u64>,
    /// Answer no earlier than this (a refused sign-in, GH#216). `api::route`
    /// waits after the handler returned and gave back its capacity permit.
    pub hold_until: Option<tokio::time::Instant>,
}

impl AppError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        AppError { code, message: message.into(), details: None, retry_after: None, hold_until: None }
    }

    pub fn validation(details: Vec<FieldError>) -> Self {
        AppError {
            code: ErrorCode::ValidationError,
            message: "Request validation failed".into(),
            details: Some(details),
            retry_after: None,
            hold_until: None,
        }
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::NotFound, message)
    }

    pub fn internal() -> Self {
        Self::new(ErrorCode::InternalError, "An unexpected error occurred")
    }

    /// One problem with one body field; the message doubles as the envelope message.
    pub fn field(field: impl Into<String>, message: impl Into<String>, code: impl Into<String>) -> Self {
        let message = message.into();
        AppError {
            code: ErrorCode::ValidationError,
            message: message.clone(),
            details: Some(vec![FieldError {
                location: FieldLocation::Body,
                field: field.into(),
                message,
                code: code.into(),
            }]),
            retry_after: None,
            hold_until: None,
        }
    }

    /// "<Entity> <id> not found".
    pub fn missing(entity: &str, id: impl std::fmt::Display) -> Self {
        Self::not_found(format!("{entity} {id} not found"))
    }

    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Conflict, message)
    }

    pub fn with_details(mut self, details: Vec<FieldError>) -> Self {
        self.details = Some(details);
        self
    }
}

#[derive(Serialize)]
struct Envelope<'a> {
    error: Body<'a>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Body<'a> {
    code: ErrorCode,
    message: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    details: Option<&'a [FieldError]>,
    request_id: String,
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let body = Envelope {
            error: Body {
                code: self.code,
                message: &self.message,
                details: self.details.as_deref(),
                request_id: request_id::current(),
            },
        };
        let mut res = (self.code.status(), Json(body)).into_response();
        if let Some(secs) = self.retry_after {
            res.headers_mut().insert(axum::http::header::RETRY_AFTER, secs.into());
        }
        res
    }
}

impl From<sqlx::Error> for AppError {
    /// Client-caused constraint violations become 400/409 with field details
    /// (see [`crate::api::pg_error`]); connection-level failures become 503
    /// DATABASE_UNAVAILABLE; deadlocks and serialization failures 503
    /// SERVER_BUSY with Retry-After; anything else is logged and hidden behind
    /// INTERNAL_ERROR.
    fn from(err: sqlx::Error) -> Self {
        if let Some(mapped) = crate::api::pg_error::map(&err, None) {
            return mapped;
        }
        if crate::api::pg_error::is_connection_error(&err) {
            tracing::error!(error = %err, "database unavailable");
            return AppError::new(ErrorCode::DatabaseUnavailable, "The database is unreachable; try again shortly");
        }
        if crate::api::pg_error::is_retryable(&err) {
            tracing::warn!(error = %err, "transaction lost a race with another one");
            let mut e = AppError::new(ErrorCode::ServerBusy, "The request conflicted with another one; retry it");
            e.retry_after = Some(1);
            return e;
        }
        tracing::error!(error = %err, "unhandled database error");
        AppError::internal()
    }
}
