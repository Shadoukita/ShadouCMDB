//! Who is making a change, as recorded in `audit_log`.
//!
//! This is the seam for authentication: Milestone 1 has no auth, so the
//! default resolver labels every caller an unauthenticated `api_client` and
//! takes an optional, untrusted display name from the `X-Actor-Name` header.
//! An auth module replaces the resolver in [`crate::http::AppState`] (and fills
//! `id`) without touching routes or services. Route-level authorisation (RBAC)
//! hooks in at the same point: a resolver may reject the request.

use std::future::Future;
use std::pin::Pin;

use axum::http::HeaderMap;
use serde::Serialize;

use crate::http::error::AppError;

// `audit_log.actor_type`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, utoipa::ToSchema, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "text", rename_all = "snake_case")]
pub enum ActorType {
    System,
    User,
    ApiClient,
    // Bulk imports and discovery: call the services directly with `RequestContext::import`.
    Import,
}

impl ActorType {
    pub fn as_str(self) -> &'static str {
        match self {
            ActorType::System => "system",
            ActorType::User => "user",
            ActorType::ApiClient => "api_client",
            ActorType::Import => "import",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Actor {
    pub actor_type: ActorType,
    pub id: Option<String>,
    pub name: Option<String>,
}

/// Per-request context handed to services: the actor and a request id for audit correlation.
#[derive(Debug, Clone)]
pub struct RequestContext {
    pub actor: Actor,
    pub request_id: String,
}

impl RequestContext {
    /// Context for a bulk import or discovery run that calls the services
    /// without going through HTTP; its audit rows carry `actor_type = import`.
    #[allow(dead_code)] // seam for the import/discovery modules
    pub fn import(source: impl Into<String>, run_id: impl Into<String>) -> Self {
        RequestContext {
            actor: Actor { actor_type: ActorType::Import, id: None, name: Some(source.into()) },
            request_id: run_id.into(),
        }
    }
}

pub type ActorFuture<'a> = Pin<Box<dyn Future<Output = Result<Actor, AppError>> + Send + 'a>>;

/// Resolves the actor of an HTTP request. Async so that an auth module can
/// verify a token or look up a session.
pub trait ActorResolver: Send + Sync + 'static {
    fn resolve<'a>(&'a self, headers: &'a HeaderMap) -> ActorFuture<'a>;
}

pub const ACTOR_NAME_HEADER: &str = "x-actor-name";

/// Milestone 1: no authentication, optional display name from `X-Actor-Name`.
pub struct AnonymousActorResolver;

impl ActorResolver for AnonymousActorResolver {
    fn resolve<'a>(&'a self, headers: &'a HeaderMap) -> ActorFuture<'a> {
        let name = headers
            .get(ACTOR_NAME_HEADER)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.trim().chars().take(200).collect::<String>())
            .filter(|s| !s.is_empty());
        Box::pin(async move { Ok(Actor { actor_type: ActorType::ApiClient, id: None, name }) })
    }
}
