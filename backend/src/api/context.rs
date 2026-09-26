//! Who is calling: the actor recorded in `audit_log`, and what they may do.
//!
//! Every HTTP request is resolved to a [`Caller`] before its handler runs (see
//! [`crate::api::route`]): anonymous for the public routes (health, login,
//! first-run setup), otherwise the signed-in user with their effective
//! permissions. Routes check global permissions declaratively; services check
//! class permissions with [`RequestContext::require_class`] and
//! [`RequestContext::class_scope`], because only they know a CI's class.
//! Imports and discovery run as [`Caller::System`] and are not restricted.

use std::net::IpAddr;
use std::sync::Arc;

use serde::Serialize;
use uuid::Uuid;

use crate::auth::Principal;
use crate::auth::permissions::{ClassOp, GlobalPermission};
use crate::http::error::{AppError, ErrorCode};

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
    /// The user's id for `user` actors.
    pub id: Option<String>,
    pub name: Option<String>,
}

#[derive(Debug, Clone)]
pub enum Caller {
    /// No session (public routes only).
    Anonymous,
    User(Arc<Principal>),
    /// The CLI, imports and discovery: not subject to permissions.
    System,
}

/// Where an HTTP request came from; recorded with authentication events.
/// Evidence only: the IP may come from client-controlled headers (see
/// [`crate::auth::session::client_ip`]), so never base an access decision on it.
#[derive(Debug, Clone, Default)]
pub struct ClientInfo {
    pub ip: Option<IpAddr>,
    pub user_agent: Option<String>,
}

/// Per-request context handed to services: the caller, the audit actor and a request id.
#[derive(Debug, Clone)]
pub struct RequestContext {
    pub caller: Caller,
    pub actor: Actor,
    pub request_id: String,
    /// Empty outside HTTP (CLI, imports).
    pub client: ClientInfo,
}

pub fn unauthenticated() -> AppError {
    AppError::new(ErrorCode::Unauthenticated, "Sign in to use this endpoint")
}

pub fn forbidden(message: impl Into<String>) -> AppError {
    AppError::new(ErrorCode::Forbidden, message)
}

impl RequestContext {
    pub fn user(principal: Arc<Principal>, request_id: String) -> Self {
        let actor = Actor {
            actor_type: ActorType::User,
            id: Some(principal.user_id.to_string()),
            name: Some(principal.username.clone()),
        };
        RequestContext { caller: Caller::User(principal), actor, request_id, client: ClientInfo::default() }
    }

    pub fn anonymous(request_id: String) -> Self {
        RequestContext {
            caller: Caller::Anonymous,
            actor: Actor { actor_type: ActorType::ApiClient, id: None, name: None },
            request_id,
            client: ClientInfo::default(),
        }
    }

    /// Changes made by the CLI or on behalf of the system (first-run setup).
    pub fn system(name: impl Into<String>, request_id: impl Into<String>) -> Self {
        RequestContext {
            caller: Caller::System,
            actor: Actor { actor_type: ActorType::System, id: None, name: Some(name.into()) },
            request_id: request_id.into(),
            client: ClientInfo::default(),
        }
    }

    /// Context for a bulk import or discovery run that calls the services
    /// without going through HTTP; its audit rows carry `actor_type = import`.
    #[allow(dead_code)] // seam for the import/discovery modules
    pub fn import(source: impl Into<String>, run_id: impl Into<String>) -> Self {
        RequestContext {
            caller: Caller::System,
            actor: Actor { actor_type: ActorType::Import, id: None, name: Some(source.into()) },
            request_id: run_id.into(),
            client: ClientInfo::default(),
        }
    }

    pub fn with_client(mut self, client: ClientInfo) -> Self {
        self.client = client;
        self
    }

    /// The same request, audited as this user (sign-in, before a session exists).
    pub fn acting_as_user(&self, user_id: Uuid, username: &str) -> Self {
        let actor =
            Actor { actor_type: ActorType::User, id: Some(user_id.to_string()), name: Some(username.to_owned()) };
        RequestContext { actor, ..self.clone() }
    }

    pub fn principal(&self) -> Option<&Principal> {
        match &self.caller {
            Caller::User(p) => Some(p),
            _ => None,
        }
    }

    pub fn require(&self, permission: GlobalPermission) -> Result<(), AppError> {
        match &self.caller {
            Caller::System => Ok(()),
            Caller::Anonymous => Err(unauthenticated()),
            Caller::User(p) if p.permissions.has(permission) => Ok(()),
            Caller::User(_) => Err(forbidden(format!("This requires the {} permission", permission.as_str()))),
        }
    }

    pub fn require_class(&self, class_id: Uuid, op: ClassOp) -> Result<(), AppError> {
        match &self.caller {
            Caller::System => Ok(()),
            Caller::Anonymous => Err(unauthenticated()),
            Caller::User(p) if p.permissions.can(class_id, op) => Ok(()),
            Caller::User(_) => {
                Err(forbidden(format!("You do not have the {} permission on this CI class", op.as_str())))
            }
        }
    }

    /// Classes the caller may perform `op` on; `None` means every class.
    pub fn class_scope(&self, op: ClassOp) -> Option<Vec<Uuid>> {
        match &self.caller {
            Caller::System => None,
            Caller::Anonymous => Some(Vec::new()),
            Caller::User(p) => p.permissions.class_scope(op),
        }
    }
}
