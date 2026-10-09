//! The approval SLA sweep (approvals design SHAA-1869 §6.2, §7.2; slice A4,
//! SHAA-2643).
//!
//! Each server process runs one sweep task unless `WORKFLOW_APPROVAL_SWEEP`
//! is `off`. Every `WORKFLOW_APPROVAL_SWEEP_INTERVAL_SECS` it:
//!
//! 1. reads up to [`BATCH`] active steps past their due date, without locks,
//!    and marks each overdue in its own transaction under the runtime locks
//!    (CI, instance, request), re-checking first: a step another process
//!    already marked is skipped, so several processes on one database write
//!    exactly one `approval_overdue` event and audit row per step, without a
//!    lease. A `flag` step gains its escalation approvers; a `reject` step
//!    closes the request as rejected, reason `overdue`;
//! 2. re-resolves up to [`BATCH`] active steps that are understaffed or were
//!    resolved before their workflow last changed (a staffing change whose
//!    own re-resolution was cut short), least recently resolved first.
//!
//! A sweep never moves an instance forward: there is no auto-approve. Times
//! are the database's clock, never the API host's.

use std::time::Duration;

use sqlx::PgPool;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use uuid::Uuid;

use super::approvals::{self, Overdue};
use crate::config::ApprovalSweepConfig;
use crate::http::error::AppError;

/// Steps handled per tick and kind (overdue, re-resolution).
pub const BATCH: i64 = 100;
/// Steps a change of a workflow's approvers re-resolves at once; the sweep finishes the rest.
pub const AFTER_CHANGE: i64 = 200;

/// What one tick did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Report {
    /// Marked overdue (`flag`).
    pub flagged: u64,
    /// Marked overdue and closed as rejected (`reject`).
    pub rejected: u64,
    /// Re-resolved, and of those whose approvers changed.
    pub reresolved: u64,
    pub changed: u64,
    /// Raised as understaffed.
    pub understaffed: u64,
    /// Steps a tick could not handle (logged; retried on the next tick).
    pub failed: u64,
}

/// The active steps of pending requests that need resolving again: never
/// resolved, resolved before the workflow last changed, or understaffed.
/// `definition`: only that workflow's, and only the stale ones.
const STALE: &str = "SELECT st.request_id, st.step_no FROM cmdb.workflow_approval_request_steps st
     JOIN cmdb.workflow_approval_requests r ON r.id = st.request_id AND r.status = 'pending'
                                          AND r.current_step_no = st.step_no
     JOIN cmdb.workflow_instances wi ON wi.id = r.instance_id
     JOIN cmdb.workflow_definitions d ON d.id = wi.definition_id
     WHERE st.status = 'active' AND ($1::uuid IS NULL OR d.id = $1)
       AND (st.resolved_at IS NULL OR st.resolved_at < d.updated_at
            OR ($1::uuid IS NULL AND st.eligible_count < st.required_approvals - (
                  SELECT count(*) FROM cmdb.workflow_approval_decisions dc
                  WHERE dc.request_id = st.request_id AND dc.step_no = st.step_no AND dc.decision = 'approve')))
     ORDER BY st.resolved_at NULLS FIRST, st.request_id LIMIT $2";

/// One pass of the sweep. Each step is its own transaction; a step that
/// fails is logged and left for the next tick.
pub async fn tick(pool: &PgPool) -> Result<Report, AppError> {
    let mut report = Report::default();
    let due: Vec<(Uuid, i16)> = sqlx::query_as(
        "SELECT request_id, step_no FROM cmdb.workflow_approval_request_steps
         WHERE status = 'active' AND overdue_at IS NULL AND due_at IS NOT NULL AND due_at <= now()
         ORDER BY due_at LIMIT $1",
    )
    .bind(BATCH)
    .fetch_all(pool)
    .await?;
    for (request, step) in due {
        match approvals::mark_overdue(pool, request, step).await {
            Ok(Overdue::Flagged) => report.flagged += 1,
            Ok(Overdue::Rejected) => report.rejected += 1,
            Ok(Overdue::Skipped) => {}
            Err(e) => {
                report.failed += 1;
                tracing::warn!(%request, step, error = %e, "approval sweep: marking a step overdue failed");
            }
        }
    }
    let stale: Vec<(Uuid, i16)> = sqlx::query_as(STALE).bind(None::<Uuid>).bind(BATCH).fetch_all(pool).await?;
    reresolve_all(pool, stale, &mut report).await;
    Ok(report)
}

/// After a change of workflow `definition`'s approvers committed: re-resolves
/// the active steps of its pending requests, up to [`AFTER_CHANGE`], each in
/// its own transaction. The sweep finishes any left over.
pub async fn after_approvers_change(pool: &PgPool, definition: Uuid) -> Result<Report, AppError> {
    let mut report = Report::default();
    let stale: Vec<(Uuid, i16)> =
        sqlx::query_as(STALE).bind(Some(definition)).bind(AFTER_CHANGE).fetch_all(pool).await?;
    reresolve_all(pool, stale, &mut report).await;
    Ok(report)
}

async fn reresolve_all(pool: &PgPool, steps: Vec<(Uuid, i16)>, report: &mut Report) {
    for (request, step) in steps {
        match approvals::reresolve(pool, request, step).await {
            Ok(r) => {
                report.reresolved += u64::from(r.resolved);
                report.changed += u64::from(r.changed);
                report.understaffed += u64::from(r.understaffed);
            }
            Err(e) => {
                report.failed += 1;
                tracing::warn!(%request, step, error = %e, "approval sweep: re-resolving a step failed");
            }
        }
    }
}

/// The sweep task of this server process.
pub struct Sweep {
    stop: watch::Sender<bool>,
    task: JoinHandle<()>,
}

impl Sweep {
    /// None when `WORKFLOW_APPROVAL_SWEEP=off`: another process sweeps.
    pub fn spawn(pool: PgPool, cfg: ApprovalSweepConfig) -> Option<Self> {
        if !cfg.enabled {
            tracing::info!("approval SLA sweep is off in this process (WORKFLOW_APPROVAL_SWEEP=off)");
            return None;
        }
        let (stop, rx) = watch::channel(false);
        Some(Sweep { stop, task: tokio::spawn(sweep_loop(pool, cfg.interval, rx)) })
    }

    pub async fn stop(self) {
        let _ = self.stop.send(true);
        let _ = tokio::time::timeout(Duration::from_secs(10), self.task).await;
    }
}

async fn sweep_loop(pool: PgPool, every: Duration, mut stop: watch::Receiver<bool>) {
    loop {
        tokio::select! {
            _ = tokio::time::sleep(every) => {}
            _ = stop.changed() => return,
        }
        match tick(&pool).await {
            Ok(r) if r != Report::default() => tracing::info!(
                flagged = r.flagged,
                rejected = r.rejected,
                reresolved = r.reresolved,
                changed = r.changed,
                understaffed = r.understaffed,
                failed = r.failed,
                "approval SLA sweep"
            ),
            Ok(_) => {}
            Err(e) => tracing::warn!(error = %e, "approval SLA sweep failed; retried on the next tick"),
        }
    }
}
