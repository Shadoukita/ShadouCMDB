//! In-app notifications (SHAA-2356): per user, read in the web UI only.
//!
//! Nothing leaves the server: no e-mail, webhook or push; the bell polls
//! `unread-count`. The rows are written by database triggers in the
//! transaction of the event they report (migration 0072), so every code path
//! notifies and a rolled-back action notifies no one. Every route needs a
//! browser session: a notification belongs to a person, and an API token has
//! no inbox.

pub mod service;
#[cfg(test)]
mod tests;

use axum::http::Method;

use crate::api::route::{Body, IdPath, In, Json, NoBody, NoContent, NoPath, NoQuery, Query, Route, route};
use crate::http::error::ErrorCode;
use service::{ListNotificationsQuery, MarkNotificationsRead, UpdateNotification};

pub const TAG: &str = "Notifications";

const NOTIFICATIONS: &str = "/api/v1/notifications";
const NOTIFICATION: &str = "/api/v1/notifications/{id}";

pub fn routes() -> Vec<Route> {
    vec![
        route(Method::GET, NOTIFICATIONS, "listNotifications")
            .tag(TAG)
            .summary("The caller's notifications, newest first")
            .description(
                "Session only. Only the caller's own; a notification about a CI is listed only while the caller may \
                 view its class (judged now, not when it was sent). Kept for `NOTIFICATION_RETENTION_DAYS` (default \
                 90) and at most the 500 newest per user.",
            )
            .session_only()
            .handle(
                |api, In(NoPath, Query(q), NoBody): In<NoPath, Query<ListNotificationsQuery>, NoBody>| async move {
                    Ok(Json(service::list(&api.pool, &api.ctx, &q).await?))
                },
            ),
        route(Method::GET, "/api/v1/notifications/unread-count", "getNotificationUnreadCount")
            .tag(TAG)
            .summary("How many of the caller's notifications are unread (the bell)")
            .description("Session only. Counts what `listNotifications?unread=true` lists; cheap enough to poll.")
            .session_only()
            .handle(|api, In(NoPath, NoQuery, NoBody): In<NoPath, NoQuery, NoBody>| async move {
                Ok(Json(service::unread_count(&api.pool, &api.ctx).await?))
            }),
        route(Method::POST, "/api/v1/notifications/mark-read", "markNotificationsRead")
            .tag(TAG)
            .summary("Mark the caller's notifications read")
            .description(
                "Session only. Every unread notification the caller may see, or only those created up to `upTo`. \
                 Not audited.",
            )
            .session_only()
            .handle(|api, In(NoPath, NoQuery, Body(b)): In<NoPath, NoQuery, Body<MarkNotificationsRead>>| async move {
                Ok(Json(service::mark_read(&api.pool, &api.ctx, &b).await?))
            }),
        route(Method::PATCH, NOTIFICATION, "updateNotification")
            .tag(TAG)
            .summary("Mark one notification read or unread")
            .description(
                "Session only. Another user's notification, and one about a CI whose class the caller may not view, \
                 are `404`, the same as one that does not exist. Not audited.",
            )
            .session_only()
            .errors(&[ErrorCode::NotFound])
            .handle(
                |api, In(IdPath(id), NoQuery, Body(b)): In<IdPath, NoQuery, Body<UpdateNotification>>| async move {
                    Ok(Json(service::update(&api.pool, &api.ctx, id, &b).await?))
                },
            ),
        route(Method::DELETE, NOTIFICATION, "deleteNotification")
            .tag(TAG)
            .summary("Dismiss one notification")
            .description("Session only. `404` as for `updateNotification`. Not audited.")
            .session_only()
            .errors(&[ErrorCode::NotFound])
            .handle(|api, In(IdPath(id), NoQuery, NoBody): In<IdPath, NoQuery, NoBody>| async move {
                service::delete(&api.pool, &api.ctx, id).await?;
                Ok(NoContent)
            }),
    ]
}
