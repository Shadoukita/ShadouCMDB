//! `/healthz` (liveness) and `/readyz` (readiness), same contract as the
//! `getLiveness` / `getReadiness` operations in backend/openapi.json.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use serde::Serialize;

use super::AppState;
use crate::db;

#[derive(Serialize)]
pub struct Liveness {
    status: &'static str,
}

/// The process is up; does not touch the database.
pub async fn liveness() -> Json<Liveness> {
    Json(Liveness { status: "ok" })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Migrations {
    /// Absent when the database is unreachable.
    #[serde(skip_serializing_if = "Option::is_none")]
    applied: Option<usize>,
    /// Migrations shipped with this build.
    expected: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    up_to_date: Option<bool>,
}

#[derive(Serialize)]
pub struct Readiness {
    status: &'static str,
    database: &'static str,
    migrations: Migrations,
}

/// 200 "ready" only when the database answers and every migration in this
/// build is applied; otherwise 503 with the same body shape.
pub async fn readiness(State(state): State<AppState>) -> (StatusCode, Json<Readiness>) {
    let expected = db::expected_count();
    let probe = async {
        sqlx::query("SELECT 1").execute(&state.pool).await?;
        db::applied_count(&state.pool).await
    };
    match probe.await {
        Ok(applied) => {
            let ready = applied == expected;
            let body = Readiness {
                status: if ready { "ready" } else { "not_ready" },
                database: "ok",
                migrations: Migrations { applied: Some(applied), expected, up_to_date: Some(ready) },
            };
            (if ready { StatusCode::OK } else { StatusCode::SERVICE_UNAVAILABLE }, Json(body))
        }
        Err(err) => {
            tracing::warn!(error = %err, "readiness check failed");
            let body = Readiness {
                status: "not_ready",
                database: "unreachable",
                migrations: Migrations { applied: None, expected, up_to_date: None },
            };
            (StatusCode::SERVICE_UNAVAILABLE, Json(body))
        }
    }
}
