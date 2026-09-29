//! `/healthz` (liveness), `/readyz` (readiness) and `/api/v1/version`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::http::{Method, StatusCode};
use serde::Serialize;
use sqlx::PgPool;
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
#[derive(Clone, Copy, Serialize, ToSchema)]
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

/// How long a readiness result is reused (GH#242).
pub const READINESS_TTL: Duration = Duration::from_secs(1);

/// A readiness result and when it was taken.
type Checked = Option<(Instant, Result<usize, DatabaseState>)>;

/// The last readiness check, shared by every `/readyz` request (GH#242).
///
/// `/readyz` is public and takes no request permit (`RouteBuilder::unlimited`), so
/// an anonymous flood must not turn into one database round trip per request on
/// the pool signed-in users need. Concurrent probes wait for the one check in
/// flight, and its result is reused for [`READINESS_TTL`]: a flood costs at most
/// about one check per second, and the answer is never older than that.
///
/// Handlers run inside the request future, which hyper drops when the client
/// disconnects. The check itself therefore runs in a detached task that owns the
/// lock, so a client that hangs up mid-check cannot cancel it and let the next
/// waiter start another one (GH#255).
pub struct ReadinessCache {
    ttl: Duration,
    /// Held while a check runs, so concurrent probes queue here, not on the pool.
    last: Arc<tokio::sync::Mutex<Checked>>,
    /// Checks that reached the database.
    #[cfg(test)]
    checks: std::sync::atomic::AtomicUsize,
}

impl Default for ReadinessCache {
    fn default() -> Self {
        ReadinessCache::with_ttl(READINESS_TTL)
    }
}

impl ReadinessCache {
    pub fn with_ttl(ttl: Duration) -> Self {
        ReadinessCache {
            ttl,
            last: Arc::default(),
            #[cfg(test)]
            checks: Default::default(),
        }
    }

    /// The applied migration count, or why the database did not answer.
    async fn check(&self, pool: &PgPool) -> Result<usize, DatabaseState> {
        let mut last = self.last.clone().lock_owned().await;
        if let Some((at, result)) = *last
            && at.elapsed() < self.ttl
        {
            return result;
        }
        #[cfg(test)]
        self.checks.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let pool = pool.clone();
        // Detached: dropping the handle when the client disconnects does not abort it.
        let check = tokio::spawn(async move {
            let probe = async {
                sqlx::query("SELECT 1").execute(&pool).await?;
                db::applied_count(&pool).await
            };
            let result = probe.await.map_err(|err| {
                tracing::warn!(error = %err, "readiness check failed");
                DatabaseState::of(&err)
            });
            *last = Some((Instant::now(), result));
            result
        });
        check.await.unwrap_or_else(|err| {
            tracing::error!(error = %err, "readiness check task failed");
            Err(DatabaseState::Unreachable)
        })
    }
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
                "Returns 200 with status \"ready\" only when the database answers and every migration in this build is applied; otherwise 503 with the same body shape. The result is reused for up to 1 s, and concurrent requests share one database check.",
            )
            .also_returns(
                StatusCode::SERVICE_UNAVAILABLE,
                "Not ready: database unreachable, credentials or privileges refused, or migrations pending",
            )
            .handle(|api, In(NoPath, NoQuery, NoBody): In<NoPath, NoQuery, NoBody>| async move {
                let expected = db::expected_count();
                Ok(match api.readiness.check(&api.pool).await {
                    Ok(applied) => {
                        let ready = applied == expected;
                        let body = Readiness {
                            status: if ready { ReadyStatus::Ready } else { ReadyStatus::NotReady },
                            database: DatabaseState::Ok,
                            migrations: Migrations { applied: Some(applied), expected, up_to_date: Some(ready) },
                        };
                        WithStatus(if ready { StatusCode::OK } else { StatusCode::SERVICE_UNAVAILABLE }, body)
                    }
                    Err(database) => {
                        let body = Readiness {
                            status: ReadyStatus::NotReady,
                            database,
                            migrations: Migrations { applied: None, expected, up_to_date: None },
                        };
                        WithStatus(StatusCode::SERVICE_UNAVAILABLE, body)
                    }
                })
            }),
    ]
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::Ordering;
    use std::time::Duration;

    use super::{DatabaseState, ReadinessCache};
    use crate::db::{self, scratch};

    /// GH#242: a flood of concurrent probes costs one database check, and the
    /// cached answer expires, so a lost database still turns `/readyz` red.
    #[tokio::test]
    async fn concurrent_probes_share_one_check_that_expires() {
        let Some(db) = scratch::database("readiness_single_flight").await else { return };
        let cache = Arc::new(ReadinessCache::with_ttl(Duration::from_millis(300)));

        let mut probes = tokio::task::JoinSet::new();
        for _ in 0..200 {
            let (cache, pool) = (cache.clone(), db.pool.clone());
            probes.spawn(async move { cache.check(&pool).await });
        }
        while let Some(result) = probes.join_next().await {
            assert_eq!(result.unwrap().ok(), Some(db::expected_count()));
        }
        assert_eq!(cache.checks.load(Ordering::SeqCst), 1, "concurrent probes each reached the database");

        // The database goes away: the cached answer holds for the window, then expires.
        db.pool.close().await;
        assert!(cache.check(&db.pool).await.is_ok());
        assert_eq!(cache.checks.load(Ordering::SeqCst), 1);
        tokio::time::sleep(Duration::from_millis(350)).await;
        assert!(matches!(cache.check(&db.pool).await, Err(DatabaseState::Unreachable)));
        assert_eq!(cache.checks.load(Ordering::SeqCst), 2);
        // The failure is cached too, so a flood against a dead database stays one check a window.
        assert!(cache.check(&db.pool).await.is_err());
        assert_eq!(cache.checks.load(Ordering::SeqCst), 2);

        db.drop().await;
    }

    /// GH#255: a client that disconnects mid-check does not cancel the check, so
    /// the next probe inside the window reuses its answer instead of starting another.
    #[tokio::test]
    async fn disconnected_caller_does_not_cancel_the_check() {
        let Some(db) = scratch::database("readiness_disconnect").await else { return };
        let cache = Arc::new(ReadinessCache::with_ttl(Duration::from_secs(30)));

        // Hold every pool connection, so the check blocks waiting for one.
        let mut held = Vec::new();
        for _ in 0..db.pool.options().get_max_connections() {
            held.push(db.pool.acquire().await.unwrap());
        }

        let first = tokio::spawn({
            let (cache, pool) = (cache.clone(), db.pool.clone());
            async move { cache.check(&pool).await }
        });
        while cache.checks.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
        // The client hangs up: hyper drops the request future mid-check.
        first.abort();
        assert!(first.await.is_err_and(|err| err.is_cancelled()));

        drop(held);
        assert_eq!(cache.check(&db.pool).await.ok(), Some(db::expected_count()));
        assert_eq!(cache.checks.load(Ordering::SeqCst), 1, "the cancelled caller's check was restarted");

        db.drop().await;
    }
}
