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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    ValidationError,
    NotFound,
    Conflict,
    InUse,
    VersionConflict,
    UnsupportedMediaType,
    PayloadTooLarge,
    DatabaseUnavailable,
    InternalError,
}

impl ErrorCode {
    pub fn status(self) -> StatusCode {
        match self {
            ErrorCode::ValidationError => StatusCode::BAD_REQUEST,
            ErrorCode::NotFound => StatusCode::NOT_FOUND,
            ErrorCode::Conflict | ErrorCode::InUse | ErrorCode::VersionConflict => StatusCode::CONFLICT,
            ErrorCode::UnsupportedMediaType => StatusCode::UNSUPPORTED_MEDIA_TYPE,
            ErrorCode::PayloadTooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            ErrorCode::DatabaseUnavailable => StatusCode::SERVICE_UNAVAILABLE,
            ErrorCode::InternalError => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum FieldLocation {
    Body,
    Query,
    Params,
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
}

impl AppError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        AppError { code, message: message.into(), details: None }
    }

    pub fn validation(details: Vec<FieldError>) -> Self {
        AppError {
            code: ErrorCode::ValidationError,
            message: "Request validation failed".into(),
            details: Some(details),
        }
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::NotFound, message)
    }

    pub fn internal() -> Self {
        Self::new(ErrorCode::InternalError, "An unexpected error occurred")
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
        (self.code.status(), Json(body)).into_response()
    }
}

impl From<sqlx::Error> for AppError {
    /// Connection-level failures become 503 DATABASE_UNAVAILABLE; anything else
    /// is logged and hidden behind INTERNAL_ERROR. Constraint violations are
    /// mapped per resource by the API modules, not here.
    fn from(err: sqlx::Error) -> Self {
        match err {
            sqlx::Error::Io(_) | sqlx::Error::Tls(_) | sqlx::Error::PoolTimedOut | sqlx::Error::PoolClosed => {
                tracing::error!(error = %err, "database unavailable");
                AppError::new(ErrorCode::DatabaseUnavailable, "The database is unreachable; try again shortly")
            }
            _ => {
                tracing::error!(error = %err, "unhandled database error");
                AppError::internal()
            }
        }
    }
}
