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
    /// A signed-in user, or an API token acting for its owner.
    User(Arc<Principal>),
    /// The CLI, imports and discovery: not subject to permissions.
    System,
}

/// Where an HTTP request came from; recorded with authentication events.
/// `ip` is the client address the server can vouch for: the TCP peer, or
/// behind one of the `TRUSTED_PROXIES` the client it reports (GH#282, see
/// [`crate::auth::session::throttle_ip`]). `claimed_ip` may come from
/// client-controlled headers (see [`crate::auth::session::client_ip`]), so it
/// is evidence only. The one access decision that uses the client address,
/// the sign-in throttle, reads `net`, derived from the same address as `ip`.
#[derive(Debug, Clone, Default)]
pub struct ClientInfo {
    /// The TCP peer, or behind a trusted proxy the client it reports: what the
    /// audit trail, `sessions.ip_address` and `api_tokens.last_used_ip` store.
    pub ip: Option<IpAddr>,
    /// The address the request claims (the leftmost forwarded hop), else the TCP peer.
    pub claimed_ip: Option<IpAddr>,
    /// The TCP peer: the one hop the client cannot forge (the proxy, if there is one).
    pub peer_ip: Option<IpAddr>,
    pub user_agent: Option<String>,
    /// The client's network, for the sign-in throttle only: the TCP peer's,
    /// or behind a trusted proxy the client's it reports. Set whatever
    /// `AUDIT_CAPTURE_CLIENT_IP` says (processed in memory only), and never
    /// stored or logged.
    pub net: crate::auth::throttle::Net,
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

    /// A request made with an API token: audited as `api_client` with the owner's id and name.
    pub fn token(principal: Arc<Principal>, request_id: String) -> Self {
        let actor = Actor {
            actor_type: ActorType::ApiClient,
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

    /// Context for a discovery run or another system import that calls the
    /// services without going through HTTP; its audit rows carry `actor_type = import`.
    /// It is the system caller, which may do anything: **bulk import
    /// (`modules/imports`) must not use it** and uses [`Self::import_for_user`].
    #[allow(dead_code)] // seam for the discovery module
    pub fn import(source: impl Into<String>, run_id: impl Into<String>) -> Self {
        RequestContext {
            caller: Caller::System,
            actor: Actor { actor_type: ActorType::Import, id: None, name: Some(source.into()) },
            request_id: run_id.into(),
            client: ClientInfo::default(),
        }
    }

    /// Bulk import on behalf of a user (SHAA-714 §3.6, T22): the user's
    /// permissions as their profiles give them now (rebuilt for every chunk),
    /// audited with `actor_type = import`, the user as actor and
    /// `request_id = import:<jobId>`.
    pub fn import_for_user(principal: Arc<Principal>, job: uuid::Uuid) -> Self {
        let actor = Actor {
            actor_type: ActorType::Import,
            id: Some(principal.user_id.to_string()),
            name: Some(principal.username.clone()),
        };
        RequestContext {
            caller: Caller::User(principal),
            actor,
            request_id: format!("import:{job}"),
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

    /// Only users whose profile is Administrator (`what`: "change a CI's ident").
    pub fn require_administrator(&self, what: &str) -> Result<(), AppError> {
        match &self.caller {
            Caller::System => Ok(()),
            Caller::Anonymous => Err(unauthenticated()),
            Caller::User(p) if p.permissions.administrator => Ok(()),
            Caller::User(_) => Err(forbidden(format!("Only administrators can {what}"))),
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

    /// A record in a class the caller may not view answers exactly like a
    /// missing one (404 `entity id not found`), so a 403 never confirms that
    /// it exists. Check this before [`Self::require_class`] for `op`s other
    /// than view.
    pub fn require_class_visible(&self, class_id: Uuid, entity: &str, id: Uuid) -> Result<(), AppError> {
        match &self.caller {
            Caller::User(p) if !p.permissions.can(class_id, ClassOp::View) => Err(AppError::missing(entity, id)),
            _ => self.require_class(class_id, ClassOp::View),
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

    /// Whether the caller may learn aggregates (counts, which values are stored)
    /// over the CIs of all these classes: only with the view right on each.
    /// A global right such as `datamodel.manage` never implies it.
    pub fn may_view_all(&self, classes: &[Uuid]) -> bool {
        self.class_scope(ClassOp::View).is_none_or(|v| classes.iter().all(|id| v.contains(id)))
    }
}

/// A count over CI data told to a client: withheld (`null`) unless the caller
/// may view every class it spans (GH#265). Build one with [`Count::scoped`];
/// a bare `Count::Exact` outside it deserves a reason in review.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Count {
    Exact(i64),
    Withheld,
}

impl Count {
    pub fn scoped(ctx: &RequestContext, spans: &[Uuid], n: i64) -> Self {
        if ctx.may_view_all(spans) { Count::Exact(n) } else { Count::Withheld }
    }

    pub fn exact(self) -> Option<i64> {
        match self {
            Count::Exact(n) => Some(n),
            Count::Withheld => None,
        }
    }
}

impl Serialize for Count {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.exact().serialize(s)
    }
}

/// A user with `datamodel.manage` who may view only these classes (tests of
/// what data-model answers tell a restricted manager).
#[cfg(test)]
pub fn datamodel_manager(view: &[Uuid]) -> RequestContext {
    use crate::auth::permissions::{ClassRights, Permissions};
    let permissions = Permissions {
        global: [GlobalPermission::DatamodelManage].into(),
        classes: view.iter().map(|id| (*id, ClassRights { view: true, ..Default::default() })).collect(),
        ..Default::default()
    };
    let principal = Principal {
        user_id: Uuid::new_v4(),
        username: "modeller".into(),
        credential: crate::auth::Credential::Token,
        permissions,
    };
    RequestContext::user(Arc::new(principal), "restricted-manager".into())
}
