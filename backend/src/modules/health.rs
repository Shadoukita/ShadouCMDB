//! `/healthz` (liveness), `/readyz` (readiness) and `/api/v1/version`.

use axum::http::{Method, StatusCode};
use serde::Serialize;
use utoipa::ToSchema;

use crate::api::pg_error;
use crate::api::route::{In, Json, NoBody, NoPath, NoQuery, Route, WithStatus, route};
use crate::db;

#[derive(Serialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum LiveStatus {
    Ok,
}

/// The running build's version (Cargo package version, SemVer).
const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct Liveness {
    #[schema(inline)]
    status: LiveStatus,
    /// Version of the running server, e.g. 0.1.0
    version: &'static str,
}

#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VersionInfo {
    /// Version of the running server (SemVer), e.g. 0.1.0; compare it with security advisories
    version: &'static str,
    /// REST API major version this server speaks
    api_version: &'static str,
    /// Database migrations shipped with this build
    migrations: usize,
}

#[derive(Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReadyStatus {
    Ready,
    NotReady,
}

/// `ok` once the database answered both queries; otherwise why it did not.
#[derive(Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DatabaseState {
    Ok,
    /// No connection (network, TLS, timeout)
    Unreachable,
    /// The server refused the credentials (SQLSTATE class 28)
    AuthenticationFailed,
    /// Connected, but the role may not read the schema (SQLSTATE 42501)
    PermissionDenied,
    /// Connected, but a query failed otherwise; the log has the error
    Error,
}

impl DatabaseState {
    fn of(err: &sqlx::Error) -> Self {
        if pg_error::is_connection_error(err) {
            return DatabaseState::Unreachable;
        }
        match err.as_database_error().and_then(|e| e.code()).as_deref() {
            Some(c) if c.starts_with("28") => DatabaseState::AuthenticationFailed,
            Some("42501") => DatabaseState::PermissionDenied,
            _ => DatabaseState::Error,
        }
    }
}

#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Migrations {
    /// Absent unless the database state is `ok`
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    applied: Option<usize>,
    /// Migrations shipped with this build
    expected: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    up_to_date: Option<bool>,
}

#[derive(Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct Readiness {
    #[schema(inline)]
    status: ReadyStatus,
    #[schema(inline)]
    database: DatabaseState,
    #[schema(inline)]
    migrations: Migrations,
}

pub fn routes() -> Vec<Route> {
    vec![
        route(Method::GET, "/healthz", "getLiveness")
            .tag("Health")
            .summary("Liveness: the process is up (does not touch the database)")
            .unlimited()
            .handle(|_, In(NoPath, NoQuery, NoBody): In<NoPath, NoQuery, NoBody>| async {
                Ok(Json(Liveness { status: LiveStatus::Ok, version: VERSION }))
            }),
        route(Method::GET, "/api/v1/version", "getVersion")
            .tag("Health")
            .summary("Version of the running server")
            .unlimited()
            .description(
                "Public, like /healthz: monitoring and vulnerability scanners need it without a session. Does not touch the database.",
            )
            .handle(|_, In(NoPath, NoQuery, NoBody): In<NoPath, NoQuery, NoBody>| async {
                Ok(Json(VersionInfo { version: VERSION, api_version: "v1", migrations: db::expected_count() }))
            }),
        route(Method::GET, "/readyz", "getReadiness")
            .tag("Health")
            .summary("Readiness: database reachable and all migrations applied")
            .unlimited()
            .description(
                "Returns 200 with status \"ready\" only when the database answers and every migration in this build is applied; otherwise 503 with the same body shape.",
            )
            .also_returns(
                StatusCode::SERVICE_UNAVAILABLE,
                "Not ready: database unreachable, credentials or privileges refused, or migrations pending",
            )
            .handle(|api, In(NoPath, NoQuery, NoBody): In<NoPath, NoQuery, NoBody>| async move {
                let expected = db::expected_count();
                let probe = async {
                    sqlx::query("SELECT 1").execute(&api.pool).await?;
                    db::applied_count(&api.pool).await
                };
                Ok(match probe.await {
                    Ok(applied) => {
                        let ready = applied == expected;
                        let body = Readiness {
                            status: if ready { ReadyStatus::Ready } else { ReadyStatus::NotReady },
                            database: DatabaseState::Ok,
                            migrations: Migrations { applied: Some(applied), expected, up_to_date: Some(ready) },
                        };
                        WithStatus(if ready { StatusCode::OK } else { StatusCode::SERVICE_UNAVAILABLE }, body)
                    }
                    Err(err) => {
                        tracing::warn!(error = %err, "readiness check failed");
                        let body = Readiness {
                            status: ReadyStatus::NotReady,
                            database: DatabaseState::of(&err),
                            migrations: Migrations { applied: None, expected, up_to_date: None },
                        };
                        WithStatus(StatusCode::SERVICE_UNAVAILABLE, body)
                    }
                })
            }),
    ]
}
