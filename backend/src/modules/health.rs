//! `/healthz` (liveness) and `/readyz` (readiness).

use axum::http::{Method, StatusCode};
use serde::Serialize;
use utoipa::ToSchema;

use crate::api::route::{In, Json, NoBody, NoPath, NoQuery, Route, WithStatus, route};
use crate::db;

#[derive(Serialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum LiveStatus {
    Ok,
}

#[derive(Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct Liveness {
    #[schema(inline)]
    status: LiveStatus,
}

#[derive(Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReadyStatus {
    Ready,
    NotReady,
}

#[derive(Serialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum DatabaseState {
    Ok,
    Unreachable,
}

#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Migrations {
    /// Absent when the database is unreachable
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
            .handle(|_, In(NoPath, NoQuery, NoBody): In<NoPath, NoQuery, NoBody>| async {
                Ok(Json(Liveness { status: LiveStatus::Ok }))
            }),
        route(Method::GET, "/readyz", "getReadiness")
            .tag("Health")
            .summary("Readiness: database reachable and all migrations applied")
            .description(
                "Returns 200 with status \"ready\" only when the database answers and every migration in this build is applied; otherwise 503 with the same body shape.",
            )
            .also_returns(StatusCode::SERVICE_UNAVAILABLE, "Not ready: database unreachable or migrations pending")
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
                            database: DatabaseState::Unreachable,
                            migrations: Migrations { applied: None, expected, up_to_date: None },
                        };
                        WithStatus(StatusCode::SERVICE_UNAVAILABLE, body)
                    }
                })
            }),
    ]
}
