//! Test sends per user and minute (GH#899, GH#904): the workflow action test
//! and the mail test each go out at once, past the outbox, so a caller may
//! send [`MAX_PER_MINUTE`] of each a minute.
//!
//! A request takes its slot in `cmdb.test_send_slots` before it sends, in a
//! transaction holding a per-user advisory lock while it counts and inserts:
//! concurrent requests, also on other server processes, queue on the lock
//! for a moment instead of all passing the count.

use sqlx::{Acquire, PgConnection};
use uuid::Uuid;

use crate::http::error::{AppError, ErrorCode};

/// Test sends per caller, kind and minute.
pub const MAX_PER_MINUTE: i64 = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TestSend {
    /// `POST /api/v1/workflows/{id}/actions/{key}/test`
    WorkflowAction,
    /// `POST /api/v1/admin/mail/test`
    Mail,
}

impl TestSend {
    fn kind(self) -> &'static str {
        match self {
            Self::WorkflowAction => "workflow_action",
            Self::Mail => "mail",
        }
    }
}

/// Takes one of the caller's test sends for this minute, or refuses with 429 RATE_LIMITED and `Retry-After` the
/// seconds until the oldest one of the minute expires. Commits on its own: the slot stays taken whatever the send
/// comes to.
pub async fn take(conn: &mut PgConnection, user: Uuid, what: TestSend) -> Result<(), AppError> {
    let mut tx = conn.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext('test_send:' || $1 || ':' || $2::text))")
        .bind(what.kind())
        .bind(user)
        .execute(&mut *tx)
        .await?;
    // clock_timestamp(): this transaction may have waited for the lock.
    sqlx::query(
        "DELETE FROM cmdb.test_send_slots
         WHERE user_id = $1 AND kind = $2 AND taken_at <= clock_timestamp() - interval '1 minute'",
    )
    .bind(user)
    .bind(what.kind())
    .execute(&mut *tx)
    .await?;
    let (taken, wait): (i64, Option<i64>) = sqlx::query_as(
        "SELECT count(*), ceil(extract(epoch FROM min(taken_at) + interval '1 minute' - clock_timestamp()))::bigint
         FROM cmdb.test_send_slots WHERE user_id = $1 AND kind = $2",
    )
    .bind(user)
    .bind(what.kind())
    .fetch_one(&mut *tx)
    .await?;
    if taken >= MAX_PER_MINUTE {
        let mut err = AppError::new(
            ErrorCode::RateLimited,
            format!("You sent {MAX_PER_MINUTE} tests in the last minute; wait a moment before the next one"),
        );
        err.retry_after = Some(wait.and_then(|s| u64::try_from(s).ok()).unwrap_or(1).clamp(1, 60));
        return Err(err);
    }
    sqlx::query("INSERT INTO cmdb.test_send_slots (user_id, kind, taken_at) VALUES ($1, $2, clock_timestamp())")
        .bind(user)
        .bind(what.kind())
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}
