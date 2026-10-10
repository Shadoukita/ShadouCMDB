//! Approver assignments: who may decide each step of a transition's approval
//! policy (approvals design SHAA-1869 §3.1, §4.2, §6.2, §10.1).
//!
//! The policy (steps, quorum, due interval) is part of a version's graph; the
//! staffing is on the definition, keyed by transition and step key like the
//! grants, so people can join and leave without a new version. Sources are a
//! permission profile, a user group, a named user, a reference field of the
//! CI that points at a Person, or the owners of the business services the CI
//! belongs to. This module stores and audits them, lints them (at publish and
//! on every change), and previews who they resolve to for one CI.
//!
//! Re-resolving the eligibility of pending requests after a change is the
//! run-time slice's (A3/A4): no request exists before it.

use std::collections::{BTreeSet, HashMap, HashSet};

use serde_json::{Value, json};
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use super::graph::{self, Fields, Stored};
use super::schemas::*;
use super::service;
use crate::api::context::RequestContext;
use crate::api::validate;
use crate::auth::permissions::{ClassOp, Permissions};
use crate::data::auth as auth_data;
use crate::data::crud::{self, AuditAction, AuditEntry};
use crate::http::error::{AppError, FieldError, FieldLocation};
use crate::modules::business_services::service::may_browse_directory;
use crate::modules::classes::AttributeDataType;

/// Users listed by a preview.
const PREVIEW_USERS: usize = 500;

/// Where an assignment's approvers come from, with what names them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    Profile { id: Uuid, name: String },
    Group { id: Uuid, name: String },
    User { id: Uuid, name: String },
    Attribute { id: Uuid, key: String, class_key: String, label: String },
    ServiceOwner(WorkflowServiceOwnerRole),
}

impl Source {
    pub fn kind(&self) -> WorkflowApproverSource {
        match self {
            Source::Profile { .. } => WorkflowApproverSource::Profile,
            Source::Group { .. } => WorkflowApproverSource::Group,
            Source::User { .. } => WorkflowApproverSource::User,
            Source::Attribute { .. } => WorkflowApproverSource::CiAttribute,
            Source::ServiceOwner(_) => WorkflowApproverSource::ServiceOwner,
        }
    }

    /// The id it is stored by (none for a service owner role).
    fn id(&self) -> Option<Uuid> {
        match self {
            Source::Profile { id, .. }
            | Source::Group { id, .. }
            | Source::User { id, .. }
            | Source::Attribute { id, .. } => Some(*id),
            Source::ServiceOwner(_) => None,
        }
    }

    /// "group CAB", for messages.
    pub fn label(&self) -> String {
        match self {
            Source::Profile { name, .. } => format!("profile {name}"),
            Source::Group { name, .. } => format!("group {name}"),
            Source::User { name, .. } => format!("user {name}"),
            Source::Attribute { key, class_key, .. } => format!("field {class_key}.{key}"),
            Source::ServiceOwner(r) => format!("{} owners of the CI's business services", r.as_str()),
        }
    }

    /// Profiles, groups and named users resolve to the same people on every CI.
    fn is_static(&self) -> bool {
        matches!(self, Source::Profile { .. } | Source::Group { .. } | Source::User { .. })
    }
}

/// One approver assignment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Assignment {
    pub transition_key: String,
    pub step_key: String,
    pub role: WorkflowApproverRole,
    pub source: Source,
}

impl Assignment {
    fn of(&self, transition: &str, step: &str) -> bool {
        self.transition_key == transition && self.step_key == step
    }

    /// The identity that may appear only once per step and role.
    fn identity(&self) -> (String, String, WorkflowApproverRole, WorkflowApproverSource, Option<Uuid>, Option<String>) {
        let role = match &self.source {
            Source::ServiceOwner(r) => Some(r.as_str().to_owned()),
            _ => None,
        };
        (self.transition_key.clone(), self.step_key.clone(), self.role, self.source.kind(), self.source.id(), role)
    }

    /// Order of the API, the audit log and the configuration file.
    fn sort_key(&self) -> (String, String, WorkflowApproverRole, WorkflowApproverSource, String) {
        let name = match &self.source {
            Source::Profile { name, .. } | Source::Group { name, .. } | Source::User { name, .. } => {
                name.to_lowercase()
            }
            Source::Attribute { key, class_key, .. } => format!("{class_key}.{key}"),
            Source::ServiceOwner(r) => r.as_str().to_owned(),
        };
        (self.transition_key.clone(), self.step_key.clone(), self.role, self.source.kind(), name)
    }

    pub fn api(&self) -> WorkflowApprover {
        let principal = |id: &Uuid, name: &String| Some(WorkflowPrincipalRef { id: *id, name: name.clone() });
        let mut out = WorkflowApprover {
            transition_key: self.transition_key.clone(),
            step_key: self.step_key.clone(),
            role: self.role,
            source: self.source.kind(),
            profile: None,
            group: None,
            user: None,
            attribute: None,
            service_owner_role: None,
        };
        match &self.source {
            Source::Profile { id, name } => out.profile = principal(id, name),
            Source::Group { id, name } => out.group = principal(id, name),
            Source::User { id, name } => out.user = principal(id, name),
            Source::Attribute { id, key, class_key, label } => {
                out.attribute = Some(WorkflowAttributeRef {
                    id: *id,
                    key: key.clone(),
                    class_key: class_key.clone(),
                    label: label.clone(),
                })
            }
            Source::ServiceOwner(r) => out.service_owner_role = Some(*r),
        }
        out
    }

    /// The configuration file's form, by name; also the audit log's.
    pub fn spec(&self) -> WorkflowApproverSpec {
        let mut out = WorkflowApproverSpec {
            transition: self.transition_key.clone(),
            step: self.step_key.clone(),
            role: self.role,
            profile: None,
            group: None,
            user: None,
            attribute: None,
            service_owner: None,
        };
        match &self.source {
            Source::Profile { name, .. } => out.profile = Some(name.clone()),
            Source::Group { name, .. } => out.group = Some(name.clone()),
            Source::User { name, .. } => out.user = Some(name.clone()),
            Source::Attribute { key, class_key, .. } => out.attribute = Some(format!("{class_key}.{key}")),
            Source::ServiceOwner(r) => out.service_owner = Some(*r),
        }
        out
    }
}

pub fn sort(list: &mut [Assignment]) {
    list.sort_by_key(Assignment::sort_key);
}

/// The assignments in the configuration file's form, sorted: what the audit
/// log records and what an import compares (names regardless of case).
pub fn specs(list: &[Assignment]) -> Vec<WorkflowApproverSpec> {
    let mut list = list.to_vec();
    sort(&mut list);
    list.iter().map(Assignment::spec).collect()
}

fn comparable(list: &[Assignment]) -> Vec<(String, String, WorkflowApproverRole, WorkflowApproverSource, String)> {
    let mut keys: Vec<_> = list.iter().map(Assignment::sort_key).collect();
    keys.sort();
    keys.dedup();
    keys
}

/// Whether two sets of assignments are the same (names regardless of case).
pub fn same(a: &[Assignment], b: &[Assignment]) -> bool {
    comparable(a) == comparable(b)
}

#[derive(sqlx::FromRow)]
struct Row {
    transition_key: String,
    step_key: String,
    role: WorkflowApproverRole,
    profile_id: Option<Uuid>,
    profile_name: Option<String>,
    group_id: Option<Uuid>,
    group_name: Option<String>,
    user_id: Option<Uuid>,
    username: Option<String>,
    attribute_id: Option<Uuid>,
    attribute_key: Option<String>,
    attribute_class_key: Option<String>,
    attribute_label: Option<String>,
    service_owner_role: Option<WorkflowServiceOwnerRole>,
}

/// The assignments of definition `id`, sorted.
pub async fn load(conn: &mut PgConnection, id: Uuid) -> Result<Vec<Assignment>, AppError> {
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT a.transition_key, a.step_key, a.role,
                a.profile_id, p.name AS profile_name, a.group_id, g.name AS group_name, a.user_id, u.username,
                a.attribute_id, ad.key AS attribute_key, c.key AS attribute_class_key, ad.label AS attribute_label,
                a.service_owner_role
         FROM cmdb.workflow_approval_assignments a
         LEFT JOIN cmdb.permission_profiles p ON p.id = a.profile_id
         LEFT JOIN cmdb.user_groups g ON g.id = a.group_id
         LEFT JOIN cmdb.users u ON u.id = a.user_id
         LEFT JOIN cmdb.ci_attribute_definitions ad ON ad.id = a.attribute_id
         LEFT JOIN cmdb.ci_classes c ON c.id = ad.class_id
         WHERE a.definition_id = $1",
    )
    .bind(id)
    .fetch_all(&mut *conn)
    .await?;
    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        let source = match (r.profile_id, r.group_id, r.user_id, r.attribute_id, r.service_owner_role) {
            (Some(id), ..) => Source::Profile { id, name: r.profile_name.unwrap_or_default() },
            (_, Some(id), ..) => Source::Group { id, name: r.group_name.unwrap_or_default() },
            (_, _, Some(id), ..) => Source::User { id, name: r.username.unwrap_or_default() },
            (_, _, _, Some(id), _) => Source::Attribute {
                id,
                key: r.attribute_key.unwrap_or_default(),
                class_key: r.attribute_class_key.unwrap_or_default(),
                label: r.attribute_label.unwrap_or_default(),
            },
            (_, _, _, _, Some(role)) => Source::ServiceOwner(role),
            // The table's check allows no other shape.
            _ => return Err(AppError::internal()),
        };
        out.push(Assignment { transition_key: r.transition_key, step_key: r.step_key, role: r.role, source });
    }
    sort(&mut out);
    Ok(out)
}

/// `(transition key, step key)` of every approval step in any version of
/// definition `id`, its draft included: what an assignment may name.
pub async fn known_steps(conn: &mut PgConnection, id: Uuid) -> Result<HashSet<(String, String)>, AppError> {
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT DISTINCT t.key, s.key FROM cmdb.workflow_transition_approval_steps s
         JOIN cmdb.workflow_transitions t ON t.id = s.transition_id
         JOIN cmdb.workflow_versions v ON v.id = t.version_id WHERE v.definition_id = $1",
    )
    .bind(id)
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows.into_iter().collect())
}

pub fn unknown_step(workflow: &str, transition: &str, step: &str) -> String {
    format!("Workflow {workflow} has no approval step {step} on transition {transition} in any version or draft")
}

// ---------------------------------------------------------------------------
// Reference fields to the Person type
// ---------------------------------------------------------------------------

/// What a `ci_attribute` source must be: a reference field of the workflow's
/// type (own or inherited) to the Person type.
pub struct PersonFields {
    person: Option<Uuid>,
    /// Field id -> the type it refers to.
    targets: HashMap<Uuid, Option<Uuid>>,
}

impl PersonFields {
    pub async fn load(conn: &mut PgConnection) -> Result<PersonFields, AppError> {
        let person: Option<Uuid> = sqlx::query_scalar("SELECT id FROM cmdb.ci_classes WHERE system_role = 'person'")
            .fetch_optional(&mut *conn)
            .await?;
        let targets: Vec<(Uuid, Option<Uuid>)> = sqlx::query_as(
            "SELECT id, reference_class_id FROM cmdb.ci_attribute_definitions WHERE data_type = 'reference'",
        )
        .fetch_all(&mut *conn)
        .await?;
        Ok(PersonFields { person, targets: targets.into_iter().collect() })
    }

    /// Why field `id` cannot name approvers (an error), or None.
    pub fn problem(&self, fields: &Fields, id: Uuid) -> Option<(&'static str, String)> {
        let Some(f) = fields.model.field(id) else {
            return Some(("unknown_attribute", "The field no longer exists".into()));
        };
        if !fields.on_type(f) {
            return Some((
                "unknown_attribute",
                format!("{} is not a field of type {} (own or inherited)", f.key, fields.class_key),
            ));
        }
        if f.data_type != AttributeDataType::Reference {
            return Some((
                "attribute_type",
                format!("Field {} is a {} field, not a reference to the Person type", f.key, f.data_type.as_str()),
            ));
        }
        let target = self.targets.get(&id).copied().flatten();
        let is_person = match (target, self.person) {
            (Some(t), Some(p)) => fields.model.lineage(t).iter().any(|c| c.id == p),
            _ => false,
        };
        if !is_person {
            let to = target.and_then(|t| fields.model.class(t)).map_or("another type", |c| c.key.as_str());
            return Some(("attribute_type", format!("Field {} refers to {to}, not to the Person type", f.key)));
        }
        None
    }

    /// Field `key` (or id) of the workflow's type as a source, or why not.
    pub fn resolve(&self, fields: &Fields, given: &str) -> Result<Source, (&'static str, String)> {
        let field = if validate::is_uuid(given) {
            given.parse::<Uuid>().ok().and_then(|id| fields.model.field(id)).filter(|f| fields.on_type(f))
        } else {
            fields.by_key(given)
        };
        let Some(f) = field else {
            return Err((
                "unknown_attribute",
                format!("Type {} has no field {given} (own or inherited)", fields.class_key),
            ));
        };
        if let Some(p) = self.problem(fields, f.id) {
            return Err(p);
        }
        let class_key = fields.model.class(f.class_id).map(|c| c.key.clone()).unwrap_or_default();
        Ok(Source::Attribute { id: f.id, key: f.key.clone(), class_key, label: f.label.clone() })
    }
}

// ---------------------------------------------------------------------------
// Lint
// ---------------------------------------------------------------------------

/// What the lint needs to know about a set of assignments, gathered up front
/// so the lint itself reads no database.
#[derive(Default)]
pub struct Facts {
    assignments: Vec<Assignment>,
    /// Per assignment, for a profile, group or named user: the active users of
    /// it who may view the workflow's type. None for the CI-dependent sources.
    viewers: Vec<Option<HashSet<Uuid>>>,
    /// Per assignment: why a field source cannot name approvers (error), or
    /// that the field is archived (warning).
    attribute: Vec<Option<(WorkflowProblemSeverity, &'static str, String)>>,
}

impl Facts {
    pub async fn gather(
        conn: &mut PgConnection,
        class_id: Uuid,
        fields: &Fields,
        assignments: Vec<Assignment>,
    ) -> Result<Facts, AppError> {
        let ids = |kind: WorkflowApproverSource| -> Vec<Uuid> {
            assignments.iter().filter(|a| a.source.kind() == kind).filter_map(|a| a.source.id()).collect()
        };
        let members: Vec<(Uuid, Uuid)> = sqlx::query_as(
            "SELECT up.profile_id, up.user_id FROM cmdb.user_permission_profiles up
             JOIN cmdb.users u ON u.id = up.user_id WHERE up.profile_id = ANY($1) AND u.is_active
             UNION ALL
             SELECT m.group_id, m.user_id FROM cmdb.user_group_members m
             JOIN cmdb.users u ON u.id = m.user_id WHERE m.group_id = ANY($2) AND u.is_active
             UNION ALL
             SELECT id, id FROM cmdb.users WHERE id = ANY($3) AND is_active",
        )
        .bind(ids(WorkflowApproverSource::Profile))
        .bind(ids(WorkflowApproverSource::Group))
        .bind(ids(WorkflowApproverSource::User))
        .fetch_all(&mut *conn)
        .await?;
        let users: Vec<Uuid> = members.iter().map(|(_, u)| *u).collect::<BTreeSet<_>>().into_iter().collect();
        let permissions = auth_data::load_permissions_of(&mut *conn, &users).await?;
        let can_view = |u: &Uuid| permissions.get(u).is_some_and(|p| p.can(class_id, ClassOp::View));
        let mut by_source: HashMap<Uuid, HashSet<Uuid>> = HashMap::new();
        for (source, user) in &members {
            let set = by_source.entry(*source).or_default();
            if can_view(user) {
                set.insert(*user);
            }
        }
        let person = PersonFields::load(&mut *conn).await?;
        let viewers = assignments
            .iter()
            .map(|a| {
                a.source
                    .is_static()
                    .then(|| a.source.id().and_then(|id| by_source.get(&id).cloned()).unwrap_or_default())
            })
            .collect();
        let attribute = assignments
            .iter()
            .map(|a| match &a.source {
                Source::Attribute { id, key, .. } => match person.problem(fields, *id) {
                    Some((code, message)) => Some((WorkflowProblemSeverity::Error, code, message)),
                    None if fields.model.field(*id).is_some_and(|f| !f.is_active) => Some((
                        WorkflowProblemSeverity::Warning,
                        "inactive_attribute",
                        format!("Field {key} is archived; its values still name approvers"),
                    )),
                    None => None,
                },
                _ => None,
            })
            .collect();
        Ok(Facts { assignments, viewers, attribute })
    }

    pub fn assignments(&self) -> &[Assignment] {
        &self.assignments
    }

    /// The problems of the approval steps of version `g`; `path(i, j)` names
    /// step `j` of transition `i`.
    pub fn lint_steps(
        &self,
        g: &Stored,
        class_key: &str,
        path: &dyn Fn(usize, usize) -> String,
    ) -> Vec<WorkflowProblem> {
        let mut out = Vec::new();
        let problem = |path: String, severity, code: &str, message: String| WorkflowProblem {
            path,
            code: code.into(),
            message,
            severity,
        };
        for (i, t) in g.transitions.iter().enumerate() {
            for (j, s) in g.steps_of(t.id).enumerate() {
                let here: Vec<usize> =
                    (0..self.assignments.len()).filter(|n| self.assignments[*n].of(&t.key, &s.key)).collect();
                let approvers: Vec<usize> = here
                    .iter()
                    .copied()
                    .filter(|n| self.assignments[*n].role == WorkflowApproverRole::Approver)
                    .collect();
                for n in &here {
                    if let Some((severity, code, message)) = &self.attribute[*n] {
                        out.push(problem(path(i, j), *severity, code, message.clone()));
                    }
                }
                if approvers.is_empty() {
                    out.push(problem(
                        path(i, j),
                        WorkflowProblemSeverity::Warning,
                        "no_approvers",
                        format!(
                            "Nobody is assigned to approve step {} of transition {}: its requests would wait until \
                             someone is",
                            s.key, t.key
                        ),
                    ));
                    continue;
                }
                let mut available: HashSet<Uuid> = HashSet::new();
                for n in &approvers {
                    if let Some(viewers) = &self.viewers[*n] {
                        if viewers.is_empty() {
                            out.push(problem(
                                path(i, j),
                                WorkflowProblemSeverity::Warning,
                                "approvers_cannot_view",
                                format!(
                                    "No active user of {} may view type {class_key}: they would never see a request \
                                     of step {}",
                                    self.assignments[*n].source.label(),
                                    s.key
                                ),
                            ));
                        }
                        available.extend(viewers);
                    }
                }
                // A field names at most one user per CI; service owners can be any number.
                let open_ended =
                    approvers.iter().any(|n| matches!(self.assignments[*n].source, Source::ServiceOwner(_)));
                let fields =
                    approvers.iter().filter(|n| matches!(self.assignments[**n].source, Source::Attribute { .. }));
                let most = available.len() + fields.count();
                if !open_ended && most < s.required_approvals as usize {
                    out.push(problem(
                        path(i, j),
                        WorkflowProblemSeverity::Warning,
                        "understaffed",
                        format!(
                            "Step {} of transition {} needs {} approvals, but at most {most} approvers who may view \
                             type {class_key} are assigned (the requester never counts)",
                            s.key, t.key, s.required_approvals
                        ),
                    ));
                }
            }
        }
        out
    }

    /// Assignments for a step version `g` does not have: kept, because they
    /// may serve instances pinned to an older version.
    pub fn lint_unknown(&self, g: &Stored) -> Vec<WorkflowProblem> {
        let steps: HashSet<(&str, &str)> = g
            .transitions
            .iter()
            .flat_map(|t| g.steps_of(t.id).map(move |s| (t.key.as_str(), s.key.as_str())))
            .collect();
        let mut seen = HashSet::new();
        self.assignments
            .iter()
            .filter(|a| !steps.contains(&(a.transition_key.as_str(), a.step_key.as_str())))
            .filter(|a| seen.insert((a.transition_key.clone(), a.step_key.clone())))
            .map(|a| WorkflowProblem {
                path: "approvers".into(),
                code: "unknown_step".into(),
                message: format!(
                    "Approvers are assigned to step {} of transition {}, which this version does not have; they \
                     still serve instances on older versions",
                    a.step_key, a.transition_key
                ),
                severity: WorkflowProblemSeverity::Warning,
            })
            .collect()
    }

    /// Both, as publishing lints them.
    pub fn lint(&self, g: &Stored, class_key: &str, path: &dyn Fn(usize, usize) -> String) -> Vec<WorkflowProblem> {
        let mut out = self.lint_steps(g, class_key, path);
        out.extend(self.lint_unknown(g));
        out
    }
}

/// The lint of definition `d`'s assignments against its current version and
/// its draft, steps named by key (`transitions.approve.steps.cab`).
async fn problems(conn: &mut PgConnection, d: &WorkflowDefinition) -> Result<Vec<WorkflowProblem>, AppError> {
    let assignments = load(&mut *conn, d.id).await?;
    let fields = Fields::load(&mut *conn, d.class_id).await?;
    let facts = Facts::gather(&mut *conn, d.class_id, &fields, assignments).await?;
    let mut versions = Vec::new();
    let current: Option<graph::VersionRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {} FROM cmdb.workflow_versions WHERE id = (SELECT current_version_id FROM cmdb.workflow_definitions \
         WHERE id = $1)",
        graph::VERSION_COLUMNS
    )))
    .bind(d.id)
    .fetch_optional(&mut *conn)
    .await?;
    for row in current.into_iter().chain(service::draft_row(&mut *conn, d.id, false).await?) {
        versions.push(graph::load(&mut *conn, row).await?);
    }
    let mut out: Vec<WorkflowProblem> = Vec::new();
    for g in &versions {
        let path = |i: usize, j: usize| {
            let t = &g.transitions[i];
            let s = g.steps_of(t.id).nth(j).map(|s| s.key.as_str()).unwrap_or_default();
            format!("transitions.{}.steps.{s}", t.key)
        };
        for p in facts.lint_steps(g, &fields.class_key, &path) {
            if !out.iter().any(|o| o.path == p.path && o.code == p.code && o.message == p.message) {
                out.push(p);
            }
        }
    }
    let steps: HashSet<(&str, &str)> = versions
        .iter()
        .flat_map(|g| {
            g.transitions.iter().flat_map(move |t| g.steps_of(t.id).map(move |s| (t.key.as_str(), s.key.as_str())))
        })
        .collect();
    let mut seen = HashSet::new();
    for (k, a) in facts.assignments().iter().enumerate() {
        if !steps.contains(&(a.transition_key.as_str(), a.step_key.as_str()))
            && seen.insert((a.transition_key.as_str(), a.step_key.as_str()))
        {
            out.push(WorkflowProblem {
                path: format!("approvers[{k}]"),
                code: "unknown_step".into(),
                message: format!(
                    "Neither the current version nor the draft has step {} of transition {}: the assignment serves \
                     only instances on older versions",
                    a.step_key, a.transition_key
                ),
                severity: WorkflowProblemSeverity::Warning,
            });
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// GET and PUT
// ---------------------------------------------------------------------------

pub async fn get(pool: &PgPool, ctx: &RequestContext, id: Uuid) -> Result<WorkflowApprovers, AppError> {
    let mut conn = pool.acquire().await?;
    let d = service::load_for(&mut conn, ctx, id, false, service::Access::Read).await?;
    let approvers = load(&mut conn, id).await?.iter().map(Assignment::api).collect();
    let problems = problems(&mut conn, &d).await?;
    Ok(WorkflowApprovers { version: d.version, approvers, problems })
}

fn not_found(field: String, message: String) -> FieldError {
    FieldError { location: FieldLocation::Body, field, message, code: "not_found".into() }
}

pub async fn replace(
    pool: &PgPool,
    ctx: &RequestContext,
    id: Uuid,
    b: &WorkflowApproversReplace,
) -> Result<WorkflowApprovers, AppError> {
    let mut tx = pool.begin().await?;
    let before = service::load_for(&mut tx, ctx, id, true, service::Access::Write).await?;
    service::check_version(b.version, before.version)?;
    let known = known_steps(&mut tx, id).await?;
    let fields = Fields::load(&mut tx, before.class_id).await?;
    let person = PersonFields::load(&mut tx).await?;
    // Every profile, and the groups and users the body names (by id or by name).
    let profiles: Vec<(Uuid, String)> =
        sqlx::query_as("SELECT id, name FROM cmdb.permission_profiles").fetch_all(&mut *tx).await?;
    let named = |f: fn(&WorkflowApproverInput) -> &Option<String>| -> Vec<String> {
        b.approvers.iter().filter_map(|a| f(a).as_ref().map(|s| s.to_lowercase())).collect()
    };
    let groups: Vec<(Uuid, String)> =
        sqlx::query_as("SELECT id, name FROM cmdb.user_groups WHERE id::text = ANY($1) OR lower(name) = ANY($1)")
            .bind(named(|a| &a.group))
            .fetch_all(&mut *tx)
            .await?;
    let users: Vec<(Uuid, String)> =
        sqlx::query_as("SELECT id, username FROM cmdb.users WHERE id::text = ANY($1) OR lower(username) = ANY($1)")
            .bind(named(|a| &a.user))
            .fetch_all(&mut *tx)
            .await?;
    let find = |list: &[(Uuid, String)], given: &str| -> Option<(Uuid, String)> {
        let by_id = validate::is_uuid(given).then(|| given.parse::<Uuid>().ok()).flatten();
        list.iter().find(|(id, name)| Some(*id) == by_id || name.to_lowercase() == given.to_lowercase()).cloned()
    };
    // Users and groups by name only for who may look them up (GH#839).
    let directory = may_browse_directory(&mut tx, ctx).await?;
    let (mut kept_groups, mut kept_users) = (HashSet::new(), HashSet::new());
    for a in load(&mut tx, id).await? {
        match a.source {
            Source::Group { name, .. } => kept_groups.insert(name.to_lowercase()),
            Source::User { name, .. } => kept_users.insert(name.to_lowercase()),
            _ => false,
        };
    }

    let mut errors = Vec::new();
    let mut rows: Vec<Assignment> = Vec::new();
    let mut seen = HashSet::new();
    for (i, a) in b.approvers.iter().enumerate() {
        let path = format!("approvers[{i}]");
        if !known.contains(&(a.transition_key.clone(), a.step_key.clone())) {
            errors.push(FieldError {
                location: FieldLocation::Body,
                field: format!("{path}.stepKey"),
                message: unknown_step(&before.key, &a.transition_key, &a.step_key),
                code: "unknown_step".into(),
            });
        }
        let source = match a.source {
            WorkflowApproverSource::Profile => {
                let given = a.profile.as_deref().unwrap_or_default();
                find(&profiles, given)
                    .map(|(id, name)| Source::Profile { id, name })
                    .ok_or_else(|| not_found(format!("{path}.profile"), format!("No permission profile \"{given}\"")))
            }
            WorkflowApproverSource::Group => {
                let given = a.group.as_deref().unwrap_or_default();
                service::directory_ref(directory, &kept_groups, given, format!("{path}.group"), "user group").and_then(
                    |()| {
                        find(&groups, given)
                            .map(|(id, name)| Source::Group { id, name })
                            .ok_or_else(|| not_found(format!("{path}.group"), format!("No user group \"{given}\"")))
                    },
                )
            }
            WorkflowApproverSource::User => {
                let given = a.user.as_deref().unwrap_or_default();
                service::directory_ref(directory, &kept_users, given, format!("{path}.user"), "user").and_then(|()| {
                    find(&users, given)
                        .map(|(id, name)| Source::User { id, name })
                        .ok_or_else(|| not_found(format!("{path}.user"), format!("No user \"{given}\"")))
                })
            }
            WorkflowApproverSource::CiAttribute => person
                .resolve(&fields, a.attribute.as_deref().unwrap_or_default())
                .map_err(|(code, message)| FieldError {
                    location: FieldLocation::Body,
                    field: format!("{path}.attribute"),
                    message,
                    code: code.into(),
                }),
            WorkflowApproverSource::ServiceOwner => {
                a.service_owner_role.map(Source::ServiceOwner).ok_or_else(|| FieldError {
                    location: FieldLocation::Body,
                    field: format!("{path}.serviceOwnerRole"),
                    message: "Required".into(),
                    code: "required".into(),
                })
            }
        };
        match source {
            Ok(source) => {
                let assignment = Assignment {
                    transition_key: a.transition_key.clone(),
                    step_key: a.step_key.clone(),
                    role: a.role,
                    source,
                };
                if seen.insert(assignment.identity()) {
                    rows.push(assignment);
                } else {
                    errors.push(FieldError {
                        location: FieldLocation::Body,
                        field: path,
                        message: "The same assignment is listed more than once".into(),
                        code: "duplicate".into(),
                    });
                }
            }
            Err(e) => errors.push(e),
        }
    }
    if !errors.is_empty() {
        return Err(AppError::validation(errors));
    }
    let version = set_in(&mut tx, ctx, &before, &rows).await?;
    let after = service::load(&mut tx, id, false).await?;
    let approvers = load(&mut tx, id).await?.iter().map(Assignment::api).collect();
    let problems = problems(&mut tx, &after).await?;
    tx.commit().await?;
    Ok(WorkflowApprovers { version, approvers, problems })
}

/// Replaces the assignments of `before` with `rows`, audited when they
/// change (an `update` on the definition, by name), inside the caller's
/// transaction. Returns the definition's row version now.
pub async fn set_in(
    tx: &mut PgConnection,
    ctx: &RequestContext,
    before: &WorkflowDefinition,
    rows: &[Assignment],
) -> Result<i32, AppError> {
    let id = before.id;
    let old = load(&mut *tx, id).await?;
    if same(&old, rows) {
        return Ok(before.version);
    }
    sqlx::query("DELETE FROM cmdb.workflow_approval_assignments WHERE definition_id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    let transitions: Vec<&str> = rows.iter().map(|a| a.transition_key.as_str()).collect();
    let steps: Vec<&str> = rows.iter().map(|a| a.step_key.as_str()).collect();
    let roles: Vec<&str> = rows.iter().map(|a| a.role.as_str()).collect();
    let sources: Vec<&str> = rows.iter().map(|a| a.source.kind().as_str()).collect();
    let pick = |kind: WorkflowApproverSource| -> Vec<Option<Uuid>> {
        rows.iter().map(|a| (a.source.kind() == kind).then(|| a.source.id()).flatten()).collect()
    };
    let owner_roles: Vec<Option<&str>> = rows
        .iter()
        .map(|a| match &a.source {
            Source::ServiceOwner(r) => Some(r.as_str()),
            _ => None,
        })
        .collect();
    sqlx::query(
        "INSERT INTO cmdb.workflow_approval_assignments
           (definition_id, transition_key, step_key, role, source, profile_id, group_id, user_id, attribute_id,
            service_owner_role)
         SELECT $1, u.* FROM unnest($2::text[], $3::text[], $4::text[], $5::text[], $6::uuid[], $7::uuid[], $8::uuid[],
                                    $9::uuid[], $10::text[]) AS u",
    )
    .bind(id)
    .bind(&transitions)
    .bind(&steps)
    .bind(&roles)
    .bind(&sources)
    .bind(pick(WorkflowApproverSource::Profile))
    .bind(pick(WorkflowApproverSource::Group))
    .bind(pick(WorkflowApproverSource::User))
    .bind(pick(WorkflowApproverSource::CiAttribute))
    .bind(&owner_roles)
    .execute(&mut *tx)
    .await?;
    let (user_id, user_name) = service::actor(ctx);
    sqlx::query(
        "UPDATE cmdb.workflow_definitions SET version = version + 1, updated_by_id = $2, updated_by_name = $3
         WHERE id = $1",
    )
    .bind(id)
    .bind(user_id)
    .bind(&user_name)
    .execute(&mut *tx)
    .await?;
    let entry = AuditEntry {
        action: AuditAction::Update,
        entity_type: "workflow_definitions",
        entity_id: id,
        old_value: Some(json!({ "version": before.version, "approvers": specs(&old) })),
        new_value: Some(json!({ "version": before.version + 1, "approvers": specs(rows) })),
    };
    crud::write_audit(&mut *tx, ctx, vec![entry]).await?;
    Ok(before.version + 1)
}

/// Drops, audited, the assignments of definition `id` whose step is in no
/// version and not in the draft any more, so the stored assignments always
/// pass the PUT and the configuration import again.
pub async fn prune(tx: &mut PgConnection, ctx: &RequestContext, id: Uuid) -> Result<(), AppError> {
    let known = known_steps(&mut *tx, id).await?;
    let old = load(&mut *tx, id).await?;
    let keep: Vec<Assignment> =
        old.iter().filter(|a| known.contains(&(a.transition_key.clone(), a.step_key.clone()))).cloned().collect();
    if keep.len() == old.len() {
        return Ok(());
    }
    let before = service::load(&mut *tx, id, true).await?;
    set_in(tx, ctx, &before, &keep).await?;
    Ok(())
}

/// The assignments as the definition's audit value records them.
pub async fn audit_value(conn: &mut PgConnection, id: Uuid) -> Result<Value, AppError> {
    Ok(json!(specs(&load(conn, id).await?)))
}

// ---------------------------------------------------------------------------
// Who named the approvers (GH#664, GH#708, GH#709, GH#715)
// ---------------------------------------------------------------------------

/// The columns of an audit row `a` that made a change: actor type, id, name
/// and time, and for a change made with an API token the users who minted
/// that token for its owner (GH#709): an `api_client` row records only the
/// owner. The token is the one whose `token.use` row (written just before, in
/// the same request) shares the change's request id. When that row was pruned
/// (it is an access event), every user who minted one of the owner's tokens
/// before the change counts, so pruning never clears an edit made through a
/// lent token.
///
/// Last, for a change an approval applied (its row names the decider as the
/// actor, and `approvalRequestId` and `requestedBy` in its new value), the
/// users that request excluded and its requester (GH#715): a requester who
/// staged the field on an earlier gated transition chose its value.
const CHANGE_COLUMNS: &str = "a.actor_type, a.actor_id, a.actor_name, a.occurred_at,
    CASE WHEN a.actor_type = 'api_client'
          AND a.actor_id ~ '^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$'
    THEN ARRAY(
      SELECT DISTINCT t.created_by_user_id FROM cmdb.api_tokens t
      WHERE t.user_id = a.actor_id::uuid AND t.created_at <= a.occurred_at
        AND t.created_by_user_id IS NOT NULL AND t.created_by_user_id <> t.user_id
        AND (EXISTS (SELECT 1 FROM audit_log u
                     WHERE u.entity_type = 'api_tokens' AND u.entity_id = t.id AND u.action = 'token.use'
                       AND u.occurred_at BETWEEN a.occurred_at - interval '1 day' AND a.occurred_at
                       AND u.request_id = a.request_id)
             OR NOT EXISTS (SELECT 1 FROM cmdb.api_tokens t2
                            JOIN audit_log u ON u.entity_type = 'api_tokens' AND u.entity_id = t2.id
                             AND u.action = 'token.use'
                             AND u.occurred_at BETWEEN a.occurred_at - interval '1 day' AND a.occurred_at
                             AND u.request_id = a.request_id
                            WHERE t2.user_id = a.actor_id::uuid))
      ORDER BY 1)
    ELSE '{}'::uuid[] END,
    ARRAY(
      SELECT DISTINCT x FROM (
        SELECT unnest(r.excluded_user_ids) FROM cmdb.workflow_approval_requests r
        WHERE r.id = CASE WHEN a.new_value->>'approvalRequestId' ~ '^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$'
                          THEN (a.new_value->>'approvalRequestId')::uuid END
        UNION ALL
        SELECT (a.new_value->'requestedBy'->>'id')::uuid
        WHERE a.new_value->'requestedBy'->>'id' ~ '^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$'
      ) s(x) WHERE x IS NOT NULL ORDER BY 1)";

/// An audit row's actor type, actor id, actor name, time, token creators and
/// the users the approval request that applied it excluded.
type ChangeRow = (String, Option<String>, Option<String>, chrono::DateTime<chrono::Utc>, Vec<Uuid>, Vec<Uuid>);

fn change_of(
    (actor_type, actor_id, actor_name, changed_at, token_created_by, approval_requested_by): ChangeRow,
) -> WorkflowFieldChange {
    WorkflowFieldChange {
        actor_type,
        actor_id: actor_id.and_then(|a| a.parse().ok()),
        actor_name,
        changed_at,
        token_created_by,
        approval_requested_by,
    }
}

/// The latest change among the audit rows `filter` (on alias `a`) selects.
fn latest_sql(filter: &str) -> sqlx::AssertSqlSafe<String> {
    sqlx::AssertSqlSafe(format!(
        "SELECT {CHANGE_COLUMNS} FROM audit_log a WHERE {filter} ORDER BY a.occurred_at DESC, a.id DESC LIMIT 1"
    ))
}

/// Why a source is dropped, and the reason in words.
type Dropped = Option<(WorkflowApprovalDropReason, String)>;

/// The audited change that set the current value of field `key` of CI `ci`:
/// the latest create, update or restore row whose old and new values of the
/// field differ, so a save that left the field as it was does not hide who
/// set it. None when no audit row changed it (pruned, or older than the log).
pub async fn last_change(
    conn: &mut PgConnection,
    ci: Uuid,
    key: &str,
) -> Result<Option<WorkflowFieldChange>, AppError> {
    let row: Option<ChangeRow> = sqlx::query_as(latest_sql(
        "a.entity_type = 'configuration_items' AND a.entity_id = $1 AND a.action IN ('create', 'update', 'restore')
           AND coalesce(a.new_value->'attributes'->$2, 'null')
               IS DISTINCT FROM coalesce(a.old_value->'attributes'->$2, 'null')
           AND a.new_value ? 'attributes'",
    ))
    .bind(ci)
    .bind(key)
    .fetch_optional(&mut *conn)
    .await?;
    Ok(row.map(change_of))
}

/// The audited change that made `principal` (`{"kind", "id"}`) a `role`
/// owner of business service `service`: the latest owner update whose new
/// list has it and whose old list did not. None when no audit row did.
async fn owner_change(
    conn: &mut PgConnection,
    service: Uuid,
    role: &str,
    principal: Value,
) -> Result<Option<WorkflowFieldChange>, AppError> {
    let row: Option<ChangeRow> = sqlx::query_as(latest_sql(
        "a.entity_type = 'configuration_items' AND a.entity_id = $1 AND a.action = 'update'
           AND a.new_value->'owners'->$2 @> $3
           AND NOT coalesce(a.old_value->'owners'->$2, '[]') @> $3",
    ))
    .bind(service)
    .bind(role)
    .bind(json!([principal]))
    .fetch_optional(&mut *conn)
    .await?;
    Ok(row.map(change_of))
}

/// The audited change that created (or restored) membership edge `edge`.
async fn membership_change(conn: &mut PgConnection, edge: Uuid) -> Result<Option<WorkflowFieldChange>, AppError> {
    let row: Option<ChangeRow> = sqlx::query_as(latest_sql(
        "a.entity_type = 'ci_relationships' AND a.entity_id = $1 AND a.action IN ('create', 'restore')",
    ))
    .bind(edge)
    .fetch_optional(&mut *conn)
    .await?;
    Ok(row.map(change_of))
}

/// What a change named, for the reason in words: `did` after its author
/// ("set field server.owner"), `done` in the passive ("field server.owner was
/// set") and what is `unused` because of it.
pub struct Named<'a> {
    pub did: &'a str,
    pub done: &'a str,
    pub unused: &'a str,
}

/// Why the approvers a change named may not decide a request that the users
/// `excluded` may not decide: one of them made the change, directly or with an
/// API token one of them minted for its owner (GH#709), or requested it on an
/// approval request someone else approved (GH#715), or an API token or import
/// made it with no user recorded. A system change (a first-run setup, an
/// upgrade) counts as nobody's.
pub fn drop_reason(change: &WorkflowFieldChange, excluded: &[Uuid], what: &Named) -> Dropped {
    let by = change.actor_name.as_deref().unwrap_or("an unnamed user");
    let Named { did, done, unused } = what;
    match change.actor_id {
        Some(user) if excluded.contains(&user) => Some((
            WorkflowApprovalDropReason::FieldSetByRequester,
            format!("Separation of duties: {by} {did} and may not decide this request, so {unused}"),
        )),
        _ if change.approval_requested_by.iter().any(|u| excluded.contains(u)) => Some((
            WorkflowApprovalDropReason::FieldSetByRequester,
            format!(
                "Separation of duties: {by} {did} by approving a request made by someone who may not decide this \
                 request, so {unused}"
            ),
        )),
        Some(_) if change.token_created_by.iter().any(|c| excluded.contains(c)) => Some((
            WorkflowApprovalDropReason::FieldSetByRequester,
            format!(
                "Separation of duties: {by} {did} with an API token minted for them by someone who may not decide \
                 this request, so {unused}"
            ),
        )),
        None if matches!(change.actor_type.as_str(), "api_client" | "import") => Some((
            WorkflowApprovalDropReason::FieldSetByUnattributed,
            format!(
                "Separation of duties: {done} through an API token or an import that recorded no user, so {unused}"
            ),
        )),
        _ => None,
    }
}

/// [`drop_reason`] for field source `label` (`field server.owner`).
pub fn field_drop_reason(change: &WorkflowFieldChange, excluded: &[Uuid], label: &str) -> Dropped {
    let (did, done) = (format!("set {label}"), format!("{label} was set"));
    drop_reason(change, excluded, &Named { did: &did, done: &done, unused: "the approvers it names are not used" })
}

/// What a `service_owner` source resolves to on one CI.
#[derive(Default)]
pub struct ServiceOwners {
    /// The owner users and groups used.
    pub users: Vec<Uuid>,
    pub groups: Vec<Uuid>,
    /// The owners and memberships not used, and why.
    pub dropped: Vec<WorkflowApprovalDroppedSource>,
    /// Whether the CI's services name any owner of the role at all.
    pub any: bool,
}

/// The `role` owners of the business services CI `ci` is a direct member of
/// (approvals design A-Q3), less those a request that the users `excluded`
/// may not decide drops (GH#708): all owners of a service one of them added
/// the CI to, and each owner one of them made an owner (by
/// [`drop_reason`]). An owner or membership with no audit history is kept,
/// as a field is. With `excluded` empty nothing is dropped.
pub async fn service_owners(
    conn: &mut PgConnection,
    ci: Uuid,
    role: WorkflowServiceOwnerRole,
    excluded: &[Uuid],
) -> Result<ServiceOwners, AppError> {
    type OwnerRow = (Uuid, Uuid, String, Option<Uuid>, Option<Uuid>, Option<String>);
    let rows: Vec<OwnerRow> = sqlx::query_as(
        "SELECT r.id, sv.id, sv.label, o.user_id, o.group_id, coalesce(u.username, 'group ' || g.name)
         FROM cmdb.ci_relationships r
         JOIN cmdb.relationship_types rt ON rt.id = r.relationship_type_id
           AND rt.system_role = 'business_service_member'
         JOIN cmdb.configuration_items sv ON sv.id = r.source_ci_id AND sv.deleted_at IS NULL
         JOIN cmdb.business_service_owners o ON o.service_ci_id = sv.id AND o.role = $2
         LEFT JOIN cmdb.users u ON u.id = o.user_id
         LEFT JOIN cmdb.user_groups g ON g.id = o.group_id
         WHERE r.target_ci_id = $1 AND r.deleted_at IS NULL
         ORDER BY lower(sv.label), sv.id, o.position",
    )
    .bind(ci)
    .bind(role.as_str())
    .fetch_all(&mut *conn)
    .await?;
    let role = role.as_str();
    let mut out = ServiceOwners { any: !rows.is_empty(), ..Default::default() };
    let source = WorkflowApproverSource::ServiceOwner;
    let mut edge: Option<(Uuid, bool)> = None;
    for (edge_id, service, name, user, group, principal) in rows {
        if !excluded.is_empty() && edge.is_none_or(|(e, _)| e != edge_id) {
            let mut skip = false;
            if let Some(c) = membership_change(&mut *conn, edge_id).await? {
                let (did, done, unused) = (
                    format!("added the CI to business service {name}"),
                    format!("the CI was added to business service {name}"),
                    format!("its {role} owners are not used"),
                );
                if let Some((reason, message)) =
                    drop_reason(&c, excluded, &Named { did: &did, done: &done, unused: &unused })
                {
                    out.dropped.push(WorkflowApprovalDroppedSource {
                        source,
                        label: format!("{role} owners of business service {name}"),
                        reason,
                        message,
                        field_last_changed: c,
                    });
                    skip = true;
                }
            }
            edge = Some((edge_id, skip));
        }
        if edge.is_some_and(|(_, skip)| skip) {
            continue;
        }
        if !excluded.is_empty() {
            let (kind, id) = match (user, group) {
                (Some(u), _) => ("user", u),
                (_, Some(g)) => ("group", g),
                _ => continue,
            };
            let principal = principal.unwrap_or_default();
            if let Some(c) = owner_change(&mut *conn, service, role, json!({ "kind": kind, "id": id })).await? {
                let (did, done, unused) = (
                    format!("made {principal} a {role} owner of business service {name}"),
                    format!("{principal} was made a {role} owner of business service {name}"),
                    format!("{principal} is not used as an approver"),
                );
                if let Some((reason, message)) =
                    drop_reason(&c, excluded, &Named { did: &did, done: &done, unused: &unused })
                {
                    out.dropped.push(WorkflowApprovalDroppedSource {
                        source,
                        label: format!("{role} owner {principal} of business service {name}"),
                        reason,
                        message,
                        field_last_changed: c,
                    });
                    continue;
                }
            }
        }
        out.users.extend(user);
        out.groups.extend(group);
    }
    out.users.sort();
    out.users.dedup();
    out.groups.sort();
    out.groups.dedup();
    Ok(out)
}

/// Whether the caller may view business services, whose names and owners a
/// dropped service owner source names (GH#717).
pub async fn sees_services(conn: &mut PgConnection, ctx: &RequestContext) -> Result<bool, AppError> {
    let class = crate::data::service_owners::service_class_id(conn).await?;
    Ok(ctx.require_class(class, ClassOp::View).is_ok())
}

/// The dropped service owner sources of `dropped` as a caller who may not view
/// business services gets them (GH#717): the label and reason in general
/// words, and the change without who made it, so neither names a service, an
/// owner or who changed them. Field sources stay as they are.
pub fn hide_service_names(dropped: &mut [WorkflowApprovalDroppedSource]) {
    use WorkflowServiceOwnerRole as R;
    for d in dropped.iter_mut().filter(|d| d.source == WorkflowApproverSource::ServiceOwner) {
        // Every label `service_owners` writes starts with the role.
        let role = [R::Technical, R::Business]
            .into_iter()
            .map(R::as_str)
            .find(|r| d.label.split(' ').next() == Some(r))
            .map_or_else(String::new, |r| format!("{r} "));
        let how = match d.reason {
            WorkflowApprovalDropReason::FieldSetByRequester => "by someone who may not decide this request",
            WorkflowApprovalDropReason::FieldSetByUnattributed => {
                "through an API token or an import that recorded no user"
            }
        };
        d.label = format!("{role}owners of the CI's business services");
        d.message = format!(
            "Separation of duties: a {role}owner of one of the CI's business services, or the CI's membership of \
             that service, was set {how}, so it is not used"
        );
        let c = &mut d.field_last_changed;
        c.actor_id = None;
        c.actor_name = None;
        c.token_created_by.clear();
        c.approval_requested_by.clear();
    }
}

/// [`hide_service_names`] on dropped sources stored as JSON (an audit value).
/// A service owner source that does not read as one keeps only its source and
/// reason, so an unexpected shape never shows the names.
pub fn hide_service_names_in(value: &mut Value) {
    let Some(items) = value.as_array_mut() else { return };
    for item in items.iter_mut().filter(|i| i["source"] == WorkflowApproverSource::ServiceOwner.as_str()) {
        *item = match serde_json::from_value::<WorkflowApprovalDroppedSource>(item.clone()) {
            Ok(d) => {
                let mut one = [d];
                hide_service_names(&mut one);
                json!(one[0])
            }
            Err(_) => json!({ "source": item["source"], "reason": item["reason"] }),
        };
    }
}

// ---------------------------------------------------------------------------
// Preview
// ---------------------------------------------------------------------------

#[derive(sqlx::FromRow)]
struct UserRow {
    id: Uuid,
    username: String,
    display_name: String,
    is_active: bool,
}

fn param(field: &str, message: String, code: &str) -> AppError {
    AppError::validation(vec![FieldError {
        location: FieldLocation::Query,
        field: field.into(),
        message,
        code: code.into(),
    }])
}

/// Who the assignments of one step resolve to, for one CI or in general, and
/// why each user may or may not decide it.
pub async fn preview(
    pool: &PgPool,
    ctx: &RequestContext,
    id: Uuid,
    q: &WorkflowApproverPreviewQuery,
) -> Result<WorkflowApproverPreview, AppError> {
    let mut conn = pool.acquire().await?;
    let d = service::load_for(&mut conn, ctx, id, false, service::Access::Read).await?;
    if !known_steps(&mut conn, id).await?.contains(&(q.transition.clone(), q.step.clone())) {
        return Err(param("step", unknown_step(&d.key, &q.transition, &q.step), "unknown_step"));
    }
    // The step's quorum: the draft's, else the current version's.
    let required: Option<i32> = sqlx::query_scalar(
        "SELECT s.required_approvals::int FROM cmdb.workflow_transition_approval_steps s
         JOIN cmdb.workflow_transitions t ON t.id = s.transition_id
         JOIN cmdb.workflow_versions v ON v.id = t.version_id
         JOIN cmdb.workflow_definitions d ON d.id = v.definition_id
         WHERE v.definition_id = $1 AND t.key = $2 AND s.key = $3
           AND (v.status = 'draft' OR v.id = d.current_version_id)
         ORDER BY v.status = 'draft' DESC LIMIT 1",
    )
    .bind(id)
    .bind(&q.transition)
    .bind(&q.step)
    .fetch_optional(&mut *conn)
    .await?;
    let model = crate::schema::model::Model::load(&mut conn).await?;
    let class_id = match q.ci_id {
        None => d.class_id,
        Some(ci) => {
            let class: Option<Uuid> = sqlx::query_scalar(
                "SELECT class_id FROM cmdb.configuration_items WHERE id = $1 AND deleted_at IS NULL",
            )
            .bind(ci)
            .fetch_optional(&mut *conn)
            .await?;
            let class = class.ok_or_else(|| AppError::missing("Configuration item", ci))?;
            ctx.require_class_visible(class, "Configuration item", ci)?;
            if !service::covered(&model, &d).contains(&class) {
                return Err(param("ciId", format!("Workflow {} does not run on the CI's type", d.key), "not_covered"));
            }
            class
        }
    };
    let class_key = model.class(class_id).map(|c| c.key.clone()).unwrap_or_default();
    let assignments: Vec<Assignment> =
        load(&mut conn, id).await?.into_iter().filter(|a| a.of(&q.transition, &q.step)).collect();

    // Each assignment's users, and a note when it resolves to nobody.
    let values = match q.ci_id {
        Some(ci) => crate::modules::items::service::details(&mut conn, &model, &[ci])
            .await?
            .pop()
            .map(|c| c.attributes)
            .unwrap_or_default(),
        None => Default::default(),
    };
    // A field source on a CI: who set the field, and whether a request by `requestedBy` drops it (GH#664).
    let mut fields: Vec<(Option<WorkflowFieldChange>, Dropped)> = Vec::with_capacity(assignments.len());
    for a in &assignments {
        let change = match (&a.source, q.ci_id) {
            (Source::Attribute { key, .. }, Some(ci)) => last_change(&mut conn, ci, key).await?,
            _ => None,
        };
        let dropped =
            change.as_ref().zip(q.requested_by).and_then(|(c, r)| field_drop_reason(c, &[r], &a.source.label()));
        fields.push((change, dropped));
    }
    let excluded: Vec<Uuid> = q.requested_by.into_iter().collect();
    // A service owner source on a CI: the owners and memberships a request by `requestedBy` drops (GH#708).
    let mut parts: Vec<Vec<WorkflowApprovalDroppedSource>> = vec![Vec::new(); assignments.len()];
    let mut resolved: Vec<(Vec<Uuid>, Option<String>)> = Vec::with_capacity(assignments.len());
    for (n, (a, (_, dropped))) in assignments.iter().zip(fields.iter_mut()).enumerate() {
        let users: Vec<Uuid> = match &a.source {
            Source::Profile { id, .. } => {
                sqlx::query_scalar("SELECT user_id FROM cmdb.user_permission_profiles WHERE profile_id = $1")
                    .bind(id)
                    .fetch_all(&mut *conn)
                    .await?
            }
            Source::Group { id, .. } => {
                sqlx::query_scalar("SELECT user_id FROM cmdb.user_group_members WHERE group_id = $1")
                    .bind(id)
                    .fetch_all(&mut *conn)
                    .await?
            }
            Source::User { id, .. } => vec![*id],
            Source::Attribute { .. } | Source::ServiceOwner(_) if q.ci_id.is_none() => {
                resolved.push((Vec::new(), Some("Resolved on each CI: give ciId to see whom".into())));
                continue;
            }
            Source::Attribute { key, .. } => {
                let person = values.get(key).and_then(Value::as_str).and_then(|s| s.parse::<Uuid>().ok());
                let Some(person) = person else {
                    resolved.push((Vec::new(), Some(format!("The CI's field {key} is empty"))));
                    continue;
                };
                if let Some((_, message)) = dropped.as_ref() {
                    resolved.push((Vec::new(), Some(message.clone())));
                    continue;
                }
                let user: Vec<Uuid> = sqlx::query_scalar("SELECT id FROM cmdb.users WHERE person_ci_id = $1")
                    .bind(person)
                    .fetch_all(&mut *conn)
                    .await?;
                if user.is_empty() {
                    resolved
                        .push((Vec::new(), Some(format!("No user account is linked to the Person in field {key}"))));
                    continue;
                }
                user
            }
            Source::ServiceOwner(role) => {
                let Some(ci) = q.ci_id else { return Err(AppError::internal()) };
                let mut owners = service_owners(&mut conn, ci, *role, &excluded).await?;
                if !sees_services(&mut conn, ctx).await? {
                    hide_service_names(&mut owners.dropped);
                }
                let members: Vec<Uuid> =
                    sqlx::query_scalar("SELECT user_id FROM cmdb.user_group_members WHERE group_id = ANY($1)")
                        .bind(&owners.groups)
                        .fetch_all(&mut *conn)
                        .await?;
                let users: Vec<Uuid> =
                    owners.users.iter().copied().chain(members).collect::<BTreeSet<_>>().into_iter().collect();
                let used = !owners.users.is_empty() || !owners.groups.is_empty();
                let note = if !owners.any {
                    Some(format!("The CI is not a direct member of a business service with {} owners", role.as_str()))
                } else if !used && !owners.dropped.is_empty() {
                    *dropped = owners.dropped.first().map(|d| (d.reason, d.message.clone()));
                    Some(owners.dropped.iter().map(|d| d.message.as_str()).collect::<Vec<_>>().join(" "))
                } else {
                    users.is_empty().then(|| "Has no members".to_owned())
                };
                parts[n] = owners.dropped;
                resolved.push((users, note));
                continue;
            }
        };
        let note = users.is_empty().then(|| "Has no members".to_owned());
        resolved.push((users, note));
    }

    let all: Vec<Uuid> =
        resolved.iter().flat_map(|(u, _)| u.iter().copied()).collect::<BTreeSet<_>>().into_iter().collect();
    let rows: Vec<UserRow> =
        sqlx::query_as("SELECT id, username, display_name, is_active FROM cmdb.users WHERE id = ANY($1)")
            .bind(&all)
            .fetch_all(&mut *conn)
            .await?;
    let permissions: HashMap<Uuid, Permissions> = auth_data::load_permissions_of(&mut conn, &all).await?;
    let mut users: Vec<WorkflowApproverPreviewUser> = rows
        .into_iter()
        .map(|u| {
            let via: Vec<&Assignment> = assignments
                .iter()
                .zip(&resolved)
                .filter(|(_, (users, _))| users.contains(&u.id))
                .map(|(a, _)| a)
                .collect();
            let can_view = permissions.get(&u.id).is_some_and(|p| p.can(class_id, ClassOp::View));
            let approver = via.iter().any(|a| a.role == WorkflowApproverRole::Approver);
            use WorkflowApproverPreviewReason as R;
            let (reason, message) = if !u.is_active {
                (R::Inactive, "The account is disabled".to_owned())
            } else if !can_view {
                (
                    R::NoViewRight,
                    format!(
                        "None of their permission profiles lets them view type {class_key}: they would never see the \
                         request"
                    ),
                )
            } else if q.requested_by == Some(u.id) {
                (R::Excluded, "The requester: four-eyes never lets anyone decide their own request".to_owned())
            } else if !approver {
                (R::EscalationOnly, "Assigned for escalation only: may decide once the step is overdue".to_owned())
            } else {
                (R::Eligible, "May decide this step".to_owned())
            };
            WorkflowApproverPreviewUser {
                id: u.id,
                username: u.username,
                display_name: u.display_name,
                eligible: reason == R::Eligible,
                reason,
                message,
                via: via.iter().map(|a| format!("{}: {}", a.role.as_str(), a.source.label())).collect(),
            }
        })
        .collect();
    users.sort_by(|a, b| {
        a.reason.cmp(&b.reason).then_with(|| a.username.to_lowercase().cmp(&b.username.to_lowercase()))
    });
    let eligible_count = users.iter().filter(|u| u.eligible).count() as i64;
    let truncated = users.len() > PREVIEW_USERS;
    users.truncate(PREVIEW_USERS);
    // Counts only for a caller who may not look up users (GH#839).
    let users_hidden = !may_browse_directory(&mut conn, ctx).await?;
    if users_hidden {
        users.clear();
    }
    let sources = assignments
        .iter()
        .zip(resolved)
        .zip(fields)
        .zip(parts)
        .map(|(((a, (users, note)), (change, dropped)), dropped_parts)| WorkflowApproverPreviewSource {
            role: a.role,
            source: a.source.kind(),
            label: a.source.label(),
            user_count: users.len() as i64,
            note,
            field_last_changed: change,
            dropped: dropped.map(|(reason, _)| reason),
            dropped_parts,
        })
        .collect();
    Ok(WorkflowApproverPreview {
        transition_key: q.transition.clone(),
        step_key: q.step.clone(),
        ci_id: q.ci_id,
        required_approvals: required,
        eligible_count,
        users,
        truncated,
        users_hidden,
        sources,
    })
}
