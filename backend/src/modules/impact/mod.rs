//! Impact analysis: which CIs are affected if a CI fails (downstream), and
//! which CIs it depends on (upstream).
//!
//! Which relationship types carry impact, and which way, is data-model
//! configuration: `relationship_types.impact_direction` (migration 0029). The
//! traversal ([`engine`]) runs on the server, bounded by depth, nodes, edges
//! and time (`IMPACT_*`, [`crate::config::ImpactConfig`]), and never walks
//! through, returns or counts a CI the caller may not view.

pub mod engine;
#[cfg(test)]
mod perf;
pub mod schemas;
pub mod service;
#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::http::header::{self, HeaderValue};
use axum::http::{Method, StatusCode};
use serde::{Deserialize, Serialize};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::api::context::{Caller, RequestContext};
use crate::api::route::{Csv, IdPath, In, Json, NoBody, NoPath, NoQuery, Query, Route, WithHeaders, route};
use crate::config::ImpactConfig;
use crate::http::error::{AppError, ErrorCode};
use schemas::ImpactQuery;

/// How impact flows across an edge `source -forward_label-> target`
/// (`relationship_types.impact_direction`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "text", rename_all = "snake_case")]
pub enum ImpactDirection {
    /// The type does not propagate impact (connected_to, documentation links)
    #[default]
    None,
    /// When the target fails, the source is affected (application runs_on server, depends_on, located_in)
    TargetToSource,
    /// When the source fails, the target is affected (hosts, supplies_power_to)
    SourceToTarget,
    /// Impact flows both ways (clustered peers); the only value besides none for a non-directional type
    Both,
}

impl ImpactDirection {
    pub fn as_str(self) -> &'static str {
        match self {
            ImpactDirection::None => "none",
            ImpactDirection::TargetToSource => "target_to_source",
            ImpactDirection::SourceToTarget => "source_to_target",
            ImpactDirection::Both => "both",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "none" => ImpactDirection::None,
            "target_to_source" => ImpactDirection::TargetToSource,
            "source_to_target" => ImpactDirection::SourceToTarget,
            "both" => ImpactDirection::Both,
            _ => return None,
        })
    }

    /// A non-directional type has no source or target side.
    pub fn allowed_without_direction(self) -> bool {
        matches!(self, ImpactDirection::None | ImpactDirection::Both)
    }
}

/// The limits and the analyses in progress in this process. The caps are per
/// process: with several replicas, each has its own.
pub struct ImpactState {
    pub config: ImpactConfig,
    global: Arc<Semaphore>,
    per_user: Mutex<HashMap<Uuid, usize>>,
}

impl Default for ImpactState {
    fn default() -> Self {
        ImpactState::new(ImpactConfig::default())
    }
}

/// A running analysis; gives its places back when dropped.
pub struct RunPermit<'a> {
    _global: OwnedSemaphorePermit,
    _user: Option<UserSlot<'a>>,
}

/// One of a user's places; given back when dropped.
struct UserSlot<'a> {
    state: &'a ImpactState,
    user: Uuid,
}

impl Drop for UserSlot<'_> {
    fn drop(&mut self) {
        if let Ok(mut m) = self.state.per_user.lock()
            && let Some(n) = m.get_mut(&self.user)
        {
            *n -= 1;
            if *n == 0 {
                m.remove(&self.user);
            }
        }
    }
}

impl ImpactState {
    pub fn new(config: ImpactConfig) -> Self {
        ImpactState {
            config,
            global: Arc::new(Semaphore::new(config.max_concurrent)),
            per_user: Mutex::new(HashMap::new()),
        }
    }

    /// A place for one analysis: 429 RATE_LIMITED when the caller (a user and
    /// their API tokens) already runs `max_concurrent_per_user`, 503
    /// SERVER_BUSY when the process runs `max_concurrent`. Never waits.
    pub fn acquire(&self, ctx: &RequestContext) -> Result<RunPermit<'_>, AppError> {
        let user = match &ctx.caller {
            Caller::User(p) => Some(p.user_id),
            _ => None,
        };
        let mut slot = None;
        if let Some(user) = user {
            let mut m = self.per_user.lock().map_err(|_| AppError::internal())?;
            let n = m.entry(user).or_insert(0);
            if *n >= self.config.max_concurrent_per_user {
                let mut err = AppError::new(
                    ErrorCode::RateLimited,
                    format!(
                        "You already have {} impact analyses in progress; retry when one has finished",
                        self.config.max_concurrent_per_user
                    ),
                );
                err.retry_after = Some(1);
                return Err(err);
            }
            *n += 1;
            slot = Some(UserSlot { state: self, user });
        }
        // Refused here, the user's place goes back with `slot`.
        let global = self.global.clone().try_acquire_owned().map_err(|_| busy())?;
        Ok(RunPermit { _global: global, _user: slot })
    }
}

fn busy() -> AppError {
    tracing::warn!("impact analysis refused: IMPACT_MAX_CONCURRENT reached");
    let mut err = AppError::new(ErrorCode::ServerBusy, "The server is running too many impact analyses; retry shortly");
    err.retry_after = Some(1);
    err
}

const TAG: &str = "Impact analysis";

pub fn routes() -> Vec<Route> {
    vec![
        route(Method::GET, "/api/v1/configuration-items/{id}/impact", "getConfigurationItemImpact")
            .tag(TAG)
            .summary("Impact analysis: the CIs affected by this CI (downstream) or that it depends on (upstream)")
            .description(
                "Breadth-first traversal over live relationships whose type propagates impact \
                 (`impactDirection` on the relationship type). Downstream follows impact the way it flows, upstream \
                 against it; `both` runs the two walks separately. Each CI appears once, at its shortest hop \
                 distance, with the last hop of that path (`via`); `via` chains resolve inside `items` plus the \
                 root. Bounded by `depth`, `maxNodes`, an edge budget of 5 × maxNodes and IMPACT_TIMEOUT_MS: an \
                 analysis stopped by a bound answers 200 with `truncated` and `truncatedReason`. Not paginated: the \
                 result is bounded (at most IMPACT_MAX_NODES items). Needs view on the CI's class (404 otherwise, as \
                 for a missing CI). CIs of classes the caller may not view are neither returned, counted nor \
                 traversed: a CI reachable only through one is left out, and `visibility` says `restricted` whenever \
                 the caller's profile limits the classes they may view. 429 RATE_LIMITED when the caller already \
                 runs IMPACT_MAX_CONCURRENT_PER_USER analyses, 503 SERVER_BUSY when the server runs \
                 IMPACT_MAX_CONCURRENT.",
            )
            .errors(&[ErrorCode::NotFound, ErrorCode::RateLimited, ErrorCode::ServerBusy])
            .class_checked()
            .handle(|api, In(IdPath(id), Query(q), NoBody): In<IdPath, Query<ImpactQuery>, NoBody>| async move {
                let a = service::analyse(&api.pool, &api.ctx, &api.impact, id, &q).await?;
                Ok(Json(a.result))
            }),
        route(Method::GET, "/api/v1/configuration-items/{id}/impact/export", "exportConfigurationItemImpact")
            .tag(TAG)
            .summary("Impact analysis as CSV")
            .description(
                "The same analysis as getConfigurationItemImpact (same parameters, limits and visibility) as a CSV \
                 file (`Content-Disposition: attachment`). Every field is quoted; a value starting with =, +, -, @, \
                 a tab or a line break is prefixed with ' so spreadsheets do not run it as a formula. The first row \
                 is a comment with the root, the parameters, whether the result was truncated and the visibility \
                 note; then the columns ci_id, ident, name, class, criticality, direction, hops, via_relationship, \
                 via_ci_ident, path_idents, active, status. Each export is recorded in the audit log (action \
                 `export` on the CI, with the parameters and the row count, never the rows). Needs view on the CI's \
                 class.",
            )
            .errors(&[ErrorCode::NotFound, ErrorCode::RateLimited, ErrorCode::ServerBusy])
            .class_checked()
            .handle(|api, In(IdPath(id), Query(q), NoBody): In<IdPath, Query<ImpactQuery>, NoBody>| async move {
                let (name, body) = service::export(&api.pool, &api.ctx, &api.impact, id, &q).await?;
                let disposition = HeaderValue::from_str(&format!("attachment; filename=\"{name}\""))
                    .map_err(|_| AppError::internal())?;
                Ok(WithHeaders(
                    Csv(body),
                    vec![
                        (header::CONTENT_DISPOSITION, disposition),
                        (header::CACHE_CONTROL, HeaderValue::from_static("no-store")),
                    ],
                ))
            }),
        route(Method::GET, "/api/v1/settings/impact", "getImpactSettings")
            .tag(TAG)
            .summary("Impact analysis limits and whether any relationship type propagates impact")
            .description(
                "The bounds a request may use (IMPACT_MAX_DEPTH, IMPACT_MAX_NODES), the defaults and the deadline, \
                 and `anyTypePropagates`: false while every relationship type has impactDirection none, so every \
                 analysis would be empty.",
            )
            .status(StatusCode::OK)
            .handle(|api, In(NoPath, NoQuery, NoBody): In<NoPath, NoQuery, NoBody>| async move {
                Ok(Json(service::settings(&api.pool, &api.impact).await?))
            }),
    ]
}
