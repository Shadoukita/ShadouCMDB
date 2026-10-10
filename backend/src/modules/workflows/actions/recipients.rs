//! Who an action's recipient sources name for one event (design SHAA-2725
//! §3.1), read when the action runs, never stored ahead:
//!
//! - `profile`, `group`, `user`: the holders, members, or the user;
//! - `ci_owner`: the user linked (`users.person_ci_id`) to the Person the
//!   type's owner field (its own, else the nearest ancestor's) refers to; an
//!   owner field of another type names no account;
//! - `ci_attribute`: the same for a reference field to the Person type;
//! - `service_owner`: the owners (users, and the members of owner groups) of
//!   the business services the CI is a direct member of (approvals A-Q3);
//! - `participant`: who ran the event, started the instance, requested the
//!   approval, or may decide its active step now (with the escalation
//!   approvers once it is overdue, less the request's excluded users, plus
//!   the live delegates of each of them);
//! - `address`: a fixed address, e-mail only.
//!
//! Who is then left out (inactive, no view right on the CI's class, no
//! address) is the channel's call; this only says who is named and why.

use std::collections::{BTreeMap, BTreeSet};

use sqlx::PgConnection;
use uuid::Uuid;

use super::{WorkflowActionParticipant, WorkflowActionRecipient, WorkflowActionRecipientSource as Source};
use crate::modules::mail::render::Why;
use crate::schema::model::{Model, QualityField};

/// The event a run is about, as far as the participant sources need it.
#[derive(Debug, Clone, Default)]
pub struct EventRef {
    /// `workflow_instance_events.kind`.
    pub kind: String,
    pub actor_type: String,
    pub actor_id: Option<String>,
    pub approval_request_id: Option<Uuid>,
}

impl EventRef {
    /// The user who ran the event: a person or an API token's owner.
    pub fn actor(&self) -> Option<Uuid> {
        (self.actor_type == "user" || self.actor_type == "api_client")
            .then(|| self.actor_id.as_deref().and_then(|a| a.parse().ok()))
            .flatten()
    }
}

/// What the CI-dependent sources resolve against, and the participant ones
/// with `run` (a preview has a CI at most).
#[derive(Debug, Clone)]
pub struct Subject {
    pub ci_id: Uuid,
    pub class_id: Uuid,
    pub run: Option<RunRef>,
}

/// The instance and event of a run.
#[derive(Debug, Clone)]
pub struct RunRef {
    pub instance_id: Uuid,
    pub definition_id: Uuid,
    pub event: EventRef,
}

/// The users and fixed addresses an action names, with why.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Resolved {
    pub users: BTreeMap<Uuid, BTreeSet<Why>>,
    /// Lower-cased, each once.
    pub addresses: BTreeSet<String>,
    /// Sources that need an event or a CI and had none (a preview without them).
    pub unresolved: Vec<String>,
}

impl Resolved {
    fn add(&mut self, users: impl IntoIterator<Item = Uuid>, why: Why) {
        for u in users {
            self.users.entry(u).or_default().insert(why.clone());
        }
    }
}

/// Why, as a short label for previews: `profile Ops`, `participant starter`.
pub fn why_label(why: &Why) -> String {
    match why {
        Why::Profile(n) => format!("profile {n}"),
        Why::Group(n) => format!("group {n}"),
        Why::User => "user".into(),
        Why::CiOwner => "ci_owner".into(),
        Why::CiAttribute(n) => format!("ci_attribute {n}"),
        Why::ServiceOwner(role) => format!("service_owner {role}"),
        Why::Actor => "participant actor".into(),
        Why::Starter => "participant starter".into(),
        Why::Requester => "participant requester".into(),
        Why::Approver => "participant approvers".into(),
        Why::Delegate(n) => format!("delegate of {n}"),
        Why::Address => "address".into(),
    }
}

/// A source as a short label for previews: `profile Ops`, `participant starter`.
pub fn label(r: &WorkflowActionRecipient) -> String {
    let named = r
        .profile
        .as_ref()
        .or(r.group.as_ref())
        .or(r.user.as_ref())
        .map(|p| p.name.clone())
        .or_else(|| r.attribute.as_ref().map(|a| a.key.clone()))
        .or_else(|| r.service_owner_role.map(|s| s.as_str().to_owned()))
        .or_else(|| r.participant.map(|p| super::participant_str(p).to_owned()))
        .or_else(|| r.address.clone());
    match named {
        Some(n) => format!("{} {n}", r.source.as_str()),
        None => r.source.as_str().to_owned(),
    }
}

/// Resolves `recipients` now. Without `subject` (a preview without a CI),
/// the CI-dependent and participant sources are listed as unresolved.
pub async fn resolve(
    conn: &mut PgConnection,
    model: &Model,
    recipients: &[WorkflowActionRecipient],
    subject: Option<&Subject>,
) -> sqlx::Result<Resolved> {
    let mut out = Resolved::default();
    let ids = |s: Source| -> Vec<Uuid> {
        recipients
            .iter()
            .filter(|r| r.source == s)
            .filter_map(|r| r.profile.as_ref().or(r.group.as_ref()).or(r.user.as_ref()).map(|p| p.id))
            .collect()
    };
    let members: Vec<(String, Uuid, Uuid)> = sqlx::query_as(
        "SELECT 'profile', profile_id, user_id FROM cmdb.user_permission_profiles WHERE profile_id = ANY($1)
         UNION ALL
         SELECT 'group', group_id, user_id FROM cmdb.user_group_members WHERE group_id = ANY($2)
         UNION ALL
         SELECT 'user', id, id FROM cmdb.users WHERE id = ANY($3)",
    )
    .bind(ids(Source::Profile))
    .bind(ids(Source::Group))
    .bind(ids(Source::User))
    .fetch_all(&mut *conn)
    .await?;
    let name_of = |id: Uuid| -> String {
        recipients
            .iter()
            .find_map(|r| r.profile.as_ref().or(r.group.as_ref()).filter(|p| p.id == id).map(|p| p.name.clone()))
            .unwrap_or_default()
    };
    for (kind, source, user) in members {
        let why = match kind.as_str() {
            "profile" => Why::Profile(name_of(source)),
            "group" => Why::Group(name_of(source)),
            _ => Why::User,
        };
        out.add([user], why);
    }
    for r in recipients {
        match r.source {
            Source::Profile | Source::Group | Source::User => {}
            Source::Address => {
                if let Some(a) = &r.address {
                    out.addresses.insert(a.trim().to_lowercase());
                }
            }
            _ if subject.is_none() => out.unresolved.push(label(r)),
            Source::Participant if subject.is_some_and(|s| s.run.is_none()) => out.unresolved.push(label(r)),
            Source::CiOwner => {
                let s = subject.expect("checked");
                if let Some(field) = model.quality_field(s.class_id, QualityField::Owner) {
                    let users = linked_users(conn, model, s.ci_id, &field.key).await?;
                    out.add(users, Why::CiOwner);
                }
            }
            Source::CiAttribute => {
                let s = subject.expect("checked");
                if let Some(field) = r.attribute.as_ref().and_then(|a| model.field(a.id)) {
                    let users = linked_users(conn, model, s.ci_id, &field.key).await?;
                    out.add(users, Why::CiAttribute(field.label.clone()));
                }
            }
            Source::ServiceOwner => {
                let s = subject.expect("checked");
                if let Some(role) = r.service_owner_role {
                    let users = service_owners(conn, s.ci_id, role.as_str()).await?;
                    out.add(users, Why::ServiceOwner(role.as_str()));
                }
            }
            Source::Participant => {
                let s = subject.and_then(|s| s.run.as_ref()).expect("checked");
                match r.participant {
                    Some(WorkflowActionParticipant::Actor) => out.add(s.event.actor(), Why::Actor),
                    Some(WorkflowActionParticipant::Starter) => {
                        let starter: Option<Uuid> =
                            sqlx::query_scalar("SELECT started_by_id FROM cmdb.workflow_instances WHERE id = $1")
                                .bind(s.instance_id)
                                .fetch_optional(&mut *conn)
                                .await?
                                .flatten();
                        out.add(starter, Why::Starter);
                    }
                    Some(WorkflowActionParticipant::Requester) => {
                        let requester: Option<Uuid> = match s.event.approval_request_id {
                            Some(req) => sqlx::query_scalar(
                                "SELECT requested_by_id FROM cmdb.workflow_approval_requests WHERE id = $1",
                            )
                            .bind(req)
                            .fetch_optional(&mut *conn)
                            .await?
                            .flatten(),
                            None => None,
                        };
                        out.add(requester, Why::Requester);
                    }
                    Some(WorkflowActionParticipant::Approvers) => {
                        if let Some(req) = s.event.approval_request_id {
                            let overdue = s.event.kind == "approval_overdue";
                            for (user, delegate_of) in approvers(conn, req, s.definition_id, overdue).await? {
                                let why = match delegate_of {
                                    Some(name) => Why::Delegate(name),
                                    None => Why::Approver,
                                };
                                out.add([user], why);
                            }
                        }
                    }
                    None => {}
                }
            }
        }
    }
    Ok(out)
}

/// The users linked to the Person field `key` of CI `ci` refers to.
async fn linked_users(conn: &mut PgConnection, model: &Model, ci: Uuid, key: &str) -> sqlx::Result<Vec<Uuid>> {
    let values = crate::data::items::values(&mut *conn, model, &[ci]).await?;
    let Some(person) = values.iter().find(|v| v.key == key).and_then(|v| v.reference()) else {
        return Ok(Vec::new());
    };
    sqlx::query_scalar("SELECT id FROM cmdb.users WHERE person_ci_id = $1").bind(person).fetch_all(&mut *conn).await
}

/// The `role` owners of the business services CI `ci` is a direct member of:
/// owner users and the members of owner groups.
async fn service_owners(conn: &mut PgConnection, ci: Uuid, role: &str) -> sqlx::Result<Vec<Uuid>> {
    sqlx::query_scalar(
        "WITH owners AS (
           SELECT o.user_id, o.group_id FROM cmdb.ci_relationships r
           JOIN cmdb.relationship_types rt ON rt.id = r.relationship_type_id
             AND rt.system_role = 'business_service_member'
           JOIN cmdb.configuration_items sv ON sv.id = r.source_ci_id AND sv.deleted_at IS NULL
           JOIN cmdb.business_service_owners o ON o.service_ci_id = sv.id AND o.role = $2
           WHERE r.target_ci_id = $1 AND r.deleted_at IS NULL)
         SELECT user_id FROM owners WHERE user_id IS NOT NULL
         UNION
         SELECT m.user_id FROM owners JOIN cmdb.user_group_members m ON m.group_id = owners.group_id",
    )
    .bind(ci)
    .bind(role)
    .fetch_all(&mut *conn)
    .await
}

/// Who may decide the active step of request `req` now: its approver
/// principals (and escalation ones when `overdue`) expanded to users, less
/// the request's excluded users, plus the live delegates of each (with the
/// name of whom they act for).
async fn approvers(
    conn: &mut PgConnection,
    req: Uuid,
    definition: Uuid,
    overdue: bool,
) -> sqlx::Result<Vec<(Uuid, Option<String>)>> {
    sqlx::query_as(
        "WITH r AS (
           SELECT id, current_step_no, excluded_user_ids FROM cmdb.workflow_approval_requests WHERE id = $1),
         e AS (
           SELECT el.principal_kind, el.principal_id FROM cmdb.workflow_approval_eligibility el, r
           WHERE el.request_id = r.id AND el.step_no = r.current_step_no
             AND (el.role = 'approver' OR ($3 AND el.role = 'escalation'))),
         eligible AS (
           SELECT principal_id AS user_id FROM e WHERE principal_kind = 'user'
           UNION SELECT m.user_id FROM e JOIN cmdb.user_permission_profiles m ON m.profile_id = e.principal_id
                 WHERE e.principal_kind = 'profile'
           UNION SELECT m.user_id FROM e JOIN cmdb.user_group_members m ON m.group_id = e.principal_id
                 WHERE e.principal_kind = 'group'),
         deciders AS (
           SELECT g.user_id FROM eligible g, r WHERE NOT (g.user_id = ANY (r.excluded_user_ids)))
         SELECT user_id, NULL::text FROM deciders
         UNION ALL
         SELECT d.delegate_id, d.principal_name FROM cmdb.workflow_approval_delegations d, r
         WHERE d.principal_id IN (SELECT user_id FROM deciders) AND d.delegate_id IS NOT NULL
           AND NOT (d.delegate_id = ANY (r.excluded_user_ids))
           AND d.revoked_at IS NULL AND now() >= d.starts_at AND now() < d.ends_at
           AND (d.definition_id IS NULL OR d.definition_id = $2)",
    )
    .bind(req)
    .bind(definition)
    .bind(overdue)
    .fetch_all(&mut *conn)
    .await
}
