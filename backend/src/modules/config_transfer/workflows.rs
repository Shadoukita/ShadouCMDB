//! The `workflows` section (format version 8, design SHAA-1411 §7; approval
//! policies and approvers since version 9, approvals design SHAA-1869 §11).
//!
//! Export writes each workflow's current published version, by key: drafts,
//! retired versions, instances and their events are data, never part of a
//! file (Q8). Import never changes a published version. The file's graph is
//! resolved against this install and checksummed as a draft would be: an
//! equal checksum leaves the workflow's versions alone, a different one
//! publishes a new version (running instances keep theirs), a new key creates
//! the workflow and publishes v1. Settings and grants are replaced. Every
//! write goes through the definitions API's own functions, so it is checked,
//! linted and audited exactly like a manual change, inside the import's
//! single transaction: a graph the publish lint refuses fails the import.
//!
//! The approval policy is part of the graph and its checksum, so a changed
//! policy publishes a new version. Approvers are replaced as a whole, like
//! grants: profiles by name, fields as `<type key>.<field key>`, groups and
//! users by name. Groups and users are identity data a file never carries; one
//! the install does not have fails the import before anything is written
//! (approvals design A-Q1).

use std::collections::{HashMap, HashSet};

use serde_json::{Value, json};
use sqlx::PgConnection;
use uuid::Uuid;

use super::format::{ConfigFile, WorkflowGrantSpec, WorkflowGraphSpec, WorkflowSpec};
use super::{ChangeAction, FieldChange, Ids, ImportWarning, Importer, at, diff, not_in_file, problem};
use crate::api::context::RequestContext;
use crate::http::error::{AppError, ErrorCode, FieldError, FieldLocation};
use crate::modules::classes::ClassSystemRole;
use crate::modules::workflows::approvers::{self, Assignment, Facts, PersonFields, Source};
use crate::modules::workflows::graph::{self, Fields, LintContext, VERSION_COLUMNS, VersionRow};
use crate::modules::workflows::schemas::{
    WorkflowApproverSpec, WorkflowDefinition, WorkflowDefinitionCreate, WorkflowDefinitionUpdate, WorkflowDraftReplace,
    WorkflowGrant, WorkflowProblemSeverity,
};
use crate::modules::workflows::service::{self, Access};
use crate::schema::model::Model;

/// The change note of a version an import publishes.
pub const IMPORT_NOTE: &str = "Imported from configuration file";

const SECTION: &str = "workflows";

#[derive(sqlx::FromRow)]
struct Row {
    id: Uuid,
    key: String,
    name: String,
    description: Option<String>,
    class_key: String,
    class_system_role: Option<ClassSystemRole>,
    include_subclasses: bool,
    state_attribute_key: Option<String>,
    auto_start: bool,
    is_active: bool,
    current_version_id: Uuid,
}

/// GH#667: whether the caller may read workflow `w` of the export (every type it covers viewable).
pub(super) fn exportable(ctx: &RequestContext, model: &Model, ids: &Ids, w: &WorkflowSpec) -> bool {
    let Some(class) = ids.classes.get(&w.class).copied() else {
        return false;
    };
    let classes = if w.include_subclasses { model.subtree(class) } else { vec![class] };
    service::require_classes(ctx, &classes, Uuid::nil(), Access::Read).is_ok()
}

/// Every workflow with a current published version, ordered by key.
pub(super) async fn snapshot(conn: &mut PgConnection) -> Result<Vec<WorkflowSpec>, AppError> {
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT d.id, d.key, d.name, d.description, c.key AS class_key, c.system_role AS class_system_role,
                d.include_subclasses, a.key AS state_attribute_key, d.auto_start, d.is_active, d.current_version_id
         FROM cmdb.workflow_definitions d JOIN cmdb.ci_classes c ON c.id = d.class_id
         LEFT JOIN cmdb.ci_attribute_definitions a ON a.id = d.state_attribute_id
         WHERE d.current_version_id IS NOT NULL ORDER BY lower(d.key)",
    )
    .fetch_all(&mut *conn)
    .await?;
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    let model = Model::load(conn).await?;
    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        let version = current_version(conn, r.current_version_id).await?;
        let layout = version.layout.as_ref().map(|l| l.0.clone());
        let stored = graph::load(conn, version).await?;
        let (initial_state, states, transitions) = stored.graph(&model);
        let grants = specs(service::grant_rows(conn, r.id).await?);
        let approvers = approvers::specs(&approvers::load(conn, r.id).await?);
        out.push(WorkflowSpec {
            key: r.key,
            name: r.name,
            description: r.description,
            class: r.class_key,
            class_system_role: r.class_system_role,
            include_subclasses: r.include_subclasses,
            state_attribute: r.state_attribute_key,
            auto_start: r.auto_start,
            is_active: r.is_active,
            graph: WorkflowGraphSpec { initial_state, states, transitions, layout },
            grants,
            approvers,
        });
    }
    Ok(out)
}

fn specs(grants: Vec<WorkflowGrant>) -> Vec<WorkflowGrantSpec> {
    grants
        .into_iter()
        .map(|g| WorkflowGrantSpec {
            transition: g.transition_key,
            profiles: g.profiles.into_iter().map(|p| p.name).collect(),
        })
        .collect()
}

async fn current_version(conn: &mut PgConnection, id: Uuid) -> Result<VersionRow, AppError> {
    Ok(sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {VERSION_COLUMNS} FROM cmdb.workflow_versions WHERE id = $1"
    )))
    .bind(id)
    .fetch_one(conn)
    .await?)
}

/// The user groups and users the file's approvers name that exist here, into
/// `ids` (a file never carries them, so they are looked up by name).
pub(super) async fn principals(conn: &mut PgConnection, file: &ConfigFile, ids: &mut Ids) -> Result<(), AppError> {
    let named = |f: fn(&WorkflowApproverSpec) -> &Option<String>| -> Vec<String> {
        file.workflows
            .iter()
            .flatten()
            .flat_map(|w| &w.approvers)
            .filter_map(|a| f(a).as_ref())
            .map(|n| n.to_lowercase())
            .collect()
    };
    let (groups, users) = (named(|a| &a.group), named(|a| &a.user));
    if groups.is_empty() && users.is_empty() {
        return Ok(());
    }
    let rows: Vec<(Uuid, String)> = sqlx::query_as("SELECT id, name FROM cmdb.user_groups WHERE lower(name) = ANY($1)")
        .bind(&groups)
        .fetch_all(&mut *conn)
        .await?;
    ids.groups = rows.into_iter().map(|(id, name)| (name.to_lowercase(), (id, name))).collect();
    let rows: Vec<(Uuid, String)> =
        sqlx::query_as("SELECT id, username FROM cmdb.users WHERE lower(username) = ANY($1)")
            .bind(&users)
            .fetch_all(&mut *conn)
            .await?;
    ids.users = rows.into_iter().map(|(id, name)| (name.to_lowercase(), (id, name))).collect();
    Ok(())
}

/// What can be checked before anything is written: duplicate keys, types and
/// profiles that exist neither in the file nor here, and groups and users
/// approvers name that do not exist here.
pub(super) fn validate(
    list: &[WorkflowSpec],
    classes: &HashSet<String>,
    profiles: &HashSet<String>,
    ids: &Ids,
    e: &mut Vec<FieldError>,
) {
    let mut seen = HashSet::new();
    for (i, w) in list.iter().enumerate() {
        let p = format!("workflows.{i}");
        if !seen.insert(w.key.to_lowercase()) {
            problem(e, format!("{p}.key"), "duplicate", format!("\"{}\" appears more than once", w.key));
        }
        if !classes.contains(&w.class) {
            problem(e, format!("{p}.class"), "not_found", format!("Class \"{}\" does not exist", w.class));
        }
        let mut transitions = HashSet::new();
        for (j, g) in w.grants.iter().enumerate() {
            if !transitions.insert(g.transition.as_str()) {
                problem(e, format!("{p}.grants.{j}.transition"), "duplicate", "One entry per transition");
            }
            let mut names = HashSet::new();
            for (k, name) in g.profiles.iter().enumerate() {
                let path = format!("{p}.grants.{j}.profiles.{k}");
                if !profiles.contains(&name.to_lowercase()) {
                    problem(e, path, "not_found", format!("No permission profile \"{name}\""));
                } else if !names.insert(name.to_lowercase()) {
                    problem(e, path, "duplicate", "Listed more than once");
                }
            }
        }
        let mut assignments = HashSet::new();
        for (k, a) in w.approvers.iter().enumerate() {
            let path = format!("{p}.approvers.{k}");
            let lower = |v: &Option<String>| v.as_ref().map(|n| n.to_lowercase());
            let identity = (
                a.transition.clone(),
                a.step.clone(),
                a.role,
                lower(&a.profile),
                lower(&a.group),
                lower(&a.user),
                a.attribute.clone(),
                a.service_owner,
            );
            if !assignments.insert(identity) {
                problem(e, path.clone(), "duplicate", "The same assignment is listed more than once");
            }
            if let Some(name) = &a.profile
                && !profiles.contains(&name.to_lowercase())
            {
                problem(e, format!("{path}.profile"), "not_found", format!("No permission profile \"{name}\""));
            }
            if let Some(name) = &a.group
                && !ids.groups.contains_key(&name.to_lowercase())
            {
                problem(
                    e,
                    format!("{path}.group"),
                    "not_found",
                    format!(
                        "No user group \"{name}\" here. Groups are not part of a configuration file: create it, then \
                         import again"
                    ),
                );
            }
            if let Some(name) = &a.user
                && !ids.users.contains_key(&name.to_lowercase())
            {
                problem(
                    e,
                    format!("{path}.user"),
                    "not_found",
                    format!(
                        "No user \"{name}\" here. Users are not part of a configuration file: create the account, \
                         then import again"
                    ),
                );
            }
            if let Some(attribute) = &a.attribute {
                match attribute.split_once('.') {
                    Some((class, _)) if classes.contains(class) => {}
                    Some((class, _)) => problem(
                        e,
                        format!("{path}.attribute"),
                        "not_found",
                        format!("Class \"{class}\" does not exist"),
                    ),
                    None => problem(
                        e,
                        format!("{path}.attribute"),
                        "invalid_format",
                        "A field as <type key>.<field key>, e.g. change_request.owner",
                    ),
                }
            }
        }
    }
}

/// The settings a file sets on a workflow, for the diff (graph and grants are reported on their own).
fn settings(w: &WorkflowSpec) -> Value {
    json!({
        "name": w.name,
        "description": w.description,
        "includeSubclasses": w.include_subclasses,
        "stateAttribute": w.state_attribute,
        "autoStart": w.auto_start,
        "isActive": w.is_active,
    })
}

/// `{transition: [profile names, lower case, sorted]}`, to compare grants.
fn grant_set(grants: &[WorkflowGrantSpec]) -> Value {
    let mut map: HashMap<&str, Vec<String>> = HashMap::new();
    for g in grants.iter().filter(|g| !g.profiles.is_empty()) {
        map.entry(&g.transition).or_default().extend(g.profiles.iter().map(|p| p.to_lowercase()));
    }
    let mut keys: Vec<&&str> = map.keys().collect();
    keys.sort();
    Value::Object(
        keys.into_iter()
            .map(|k| {
                let mut names = map[*k].clone();
                names.sort();
                names.dedup();
                ((*k).to_owned(), json!(names))
            })
            .collect(),
    )
}

fn invalid(path: &str, field: &str, message: String, code: &str) -> AppError {
    at(path, AppError::field(field, message, code))
}

/// The file's approvers of `w` as assignments: names resolved against this
/// install (profiles, groups and users were checked in `validate`), fields
/// resolved and checked against the workflow's type.
fn assignments(
    w: &WorkflowSpec,
    fields: &Fields,
    person: &PersonFields,
    profiles: &HashMap<String, (Uuid, String)>,
    ids: &Ids,
    path: &str,
) -> Result<Vec<Assignment>, AppError> {
    let mut out = Vec::with_capacity(w.approvers.len());
    let mut errors = Vec::new();
    for (k, a) in w.approvers.iter().enumerate() {
        let named = |map: &HashMap<String, (Uuid, String)>, name: &str| map.get(&name.to_lowercase()).cloned();
        let source = if let Some(name) = &a.profile {
            named(profiles, name).map(|(id, name)| Source::Profile { id, name })
        } else if let Some(name) = &a.group {
            named(&ids.groups, name).map(|(id, name)| Source::Group { id, name })
        } else if let Some(name) = &a.user {
            named(&ids.users, name).map(|(id, name)| Source::User { id, name })
        } else if let Some(attribute) = &a.attribute {
            let field = attribute.split_once('.').and_then(|(class, key)| {
                fields.by_key(key).filter(|f| fields.model.class(f.class_id).is_some_and(|c| c.key == class))
            });
            let resolved = match field {
                Some(f) => person.resolve(fields, &f.id.to_string()),
                None => Err((
                    "unknown_attribute",
                    format!("Type {} has no field {attribute} (own or inherited)", fields.class_key),
                )),
            };
            match resolved {
                Ok(source) => Some(source),
                Err((code, message)) => {
                    errors.push(FieldError {
                        location: FieldLocation::Body,
                        field: format!("{path}.approvers.{k}.attribute"),
                        message,
                        code: code.into(),
                    });
                    continue;
                }
            }
        } else {
            a.service_owner.map(Source::ServiceOwner)
        };
        // Checked in validate and the file's own check: every name is here and one source is set.
        let source = source.ok_or_else(AppError::internal)?;
        out.push(Assignment { transition_key: a.transition.clone(), step_key: a.step.clone(), role: a.role, source });
    }
    if errors.is_empty() { Ok(out) } else { Err(AppError::validation(errors)) }
}

/// Every approver of `w` must name a step of some version of `d` (the
/// file's graph is one by now), as the approvers API requires.
async fn check_approver_steps(
    conn: &mut PgConnection,
    d: &WorkflowDefinition,
    w: &WorkflowSpec,
    path: &str,
) -> Result<(), AppError> {
    let known = approvers::known_steps(conn, d.id).await?;
    let errors: Vec<FieldError> = w
        .approvers
        .iter()
        .enumerate()
        .filter(|(_, a)| !known.contains(&(a.transition.clone(), a.step.clone())))
        .map(|(k, a)| FieldError {
            location: FieldLocation::Body,
            field: format!("{path}.approvers.{k}.step"),
            message: approvers::unknown_step(&d.key, &a.transition, &a.step),
            code: "unknown_step".into(),
        })
        .collect();
    if errors.is_empty() { Ok(()) } else { Err(AppError::validation(errors)) }
}

/// Every grant of `w` must name `_cancel`, `_start` or a transition of some version of
/// `d` (the file's graph is one by now), as the grants API requires.
async fn check_grant_keys(
    conn: &mut PgConnection,
    d: &WorkflowDefinition,
    w: &WorkflowSpec,
    path: &str,
) -> Result<(), AppError> {
    let known = service::transition_keys(conn, d.id).await?;
    let errors: Vec<FieldError> = w
        .grants
        .iter()
        .enumerate()
        .filter(|(_, g)| !known.contains(&g.transition))
        .map(|(j, g)| FieldError {
            location: FieldLocation::Body,
            field: format!("{path}.grants.{j}.transition"),
            message: service::unknown_transition(&d.key, &g.transition),
            code: "unknown_transition".into(),
        })
        .collect();
    if errors.is_empty() { Ok(()) } else { Err(AppError::validation(errors)) }
}

impl Importer<'_> {
    /// Stores `draft` as the definition's draft (its empty v1, or a new
    /// version), lints it with the file's grants and approvers and publishes it.
    #[allow(clippy::too_many_arguments)]
    async fn publish_graph(
        &mut self,
        d: &WorkflowDefinition,
        fields: &Fields,
        draft: &WorkflowDraftReplace,
        granted: &HashSet<String>,
        facts: &Facts,
        path: &str,
        warnings: &mut Vec<ImportWarning>,
    ) -> Result<i32, AppError> {
        let gpath = format!("{path}.graph");
        let version_id = match service::draft_row(self.conn, d.id, true).await? {
            Some(v) => v.id,
            None => {
                let next: i32 = sqlx::query_scalar(
                    "SELECT coalesce(max(version_no), 0) + 1 FROM cmdb.workflow_versions WHERE definition_id = $1",
                )
                .bind(d.id)
                .fetch_one(&mut *self.conn)
                .await?;
                service::new_draft(self.conn, d.id, next).await?
            }
        };
        graph::store_draft(self.conn, version_id, fields, d.state_attribute_id, draft)
            .await
            .map_err(|e| at(&gpath, e))?;
        let row = service::draft_row(self.conn, d.id, false).await?.ok_or_else(AppError::internal)?;
        let stored = graph::load(self.conn, row).await?;
        let problems = graph::lint(
            &stored,
            &LintContext { fields, state_attribute: d.state_attribute_id, granted, approvers: facts },
        );
        let sum = stored.checksum(&fields.model);
        for p in problems.iter().filter(|p| p.severity == WorkflowProblemSeverity::Warning) {
            warnings.push(ImportWarning { path: format!("{gpath}.{}", p.path), message: p.message.clone() });
        }
        let published =
            service::publish_in(self.conn, self.ctx, d, &stored, fields, &problems, &sum, Some(IMPORT_NOTE))
                .await
                .map_err(|e| at(&gpath, e))?;
        Ok(published.version_no)
    }

    pub(super) async fn workflows(
        &mut self,
        list: &[WorkflowSpec],
        current: &[WorkflowSpec],
        warnings: &mut Vec<ImportWarning>,
    ) -> Result<(), AppError> {
        // GH#667: the workflows on types the importer may not all view and edit
        // are left alone, as the definition API refuses them; the ones they may
        // not view are not even counted.
        let model = Model::load(self.conn).await?;
        let stored: Vec<(String, Uuid, bool)> =
            sqlx::query_as("SELECT lower(key), class_id, include_subclasses FROM cmdb.workflow_definitions")
                .fetch_all(&mut *self.conn)
                .await?;
        let covers = |class: Uuid, subtypes: bool| if subtypes { model.subtree(class) } else { vec![class] };
        let here: Vec<String> = stored
            .iter()
            .filter(|(_, class, subtypes)| self.ctx.may_view_all(&covers(*class, *subtypes)))
            .map(|(key, ..)| key.clone())
            .collect();
        let keys: HashSet<String> = list.iter().map(|w| w.key.to_lowercase()).collect();
        self.section(SECTION, not_in_file(here.iter(), &keys));
        let old: HashMap<String, &WorkflowSpec> = current.iter().map(|w| (w.key.to_lowercase(), w)).collect();
        let named_profiles: HashMap<String, (Uuid, String)> =
            sqlx::query_as::<_, (Uuid, String)>("SELECT id, name FROM cmdb.permission_profiles")
                .fetch_all(&mut *self.conn)
                .await?
                .into_iter()
                .map(|(id, name)| (name.to_lowercase(), (id, name)))
                .collect();
        let profiles: HashMap<String, Uuid> = named_profiles.iter().map(|(k, (id, _))| (k.clone(), *id)).collect();
        let person = PersonFields::load(self.conn).await?;

        for (i, w) in list.iter().enumerate() {
            let path = format!("{SECTION}.{i}");
            let class_id = self.ids.classes[&w.class];
            let mut classes = covers(class_id, w.include_subclasses);
            if let Some((_, class, subtypes)) = stored.iter().find(|(key, ..)| *key == w.key.to_lowercase()) {
                classes.extend(covers(*class, *subtypes));
            }
            if service::require_classes(self.ctx, &classes, Uuid::nil(), Access::Write).is_err() {
                warnings.push(ImportWarning {
                    path,
                    message: format!(
                        "You cannot view and edit every type workflow \"{}\" runs on; it was skipped",
                        w.key
                    ),
                });
                continue;
            }
            let fields = Fields::load(self.conn, class_id).await?;
            let state_attribute = match &w.state_attribute {
                None => None,
                Some(k) => Some(
                    fields
                        .by_key(k)
                        .ok_or_else(|| {
                            invalid(
                                &path,
                                "stateAttribute",
                                format!("Class \"{}\" has no field \"{k}\" (own or inherited)", w.class),
                                "not_found",
                            )
                        })?
                        .id,
                ),
            };
            let mut rows: Vec<(String, Uuid)> = Vec::new();
            for g in &w.grants {
                for name in &g.profiles {
                    // Checked in validate: every name is a profile here once the profiles are imported.
                    let profile = profiles.get(&name.to_lowercase()).copied().ok_or_else(AppError::internal)?;
                    if !rows.contains(&(g.transition.clone(), profile)) {
                        rows.push((g.transition.clone(), profile));
                    }
                }
            }
            let granted: HashSet<String> = rows.iter().map(|(k, _)| k.clone()).collect();
            let wanted_approvers = assignments(w, &fields, &person, &named_profiles, &self.ids, &path)?;
            let facts = Facts::gather(self.conn, class_id, &fields, wanted_approvers.clone()).await?;
            let draft = w.graph.to_draft();
            let incoming =
                graph::draft_checksum(&fields, state_attribute, &draft).map_err(|e| at(&format!("{path}.graph"), e))?;

            let existing: Option<Uuid> =
                sqlx::query_scalar("SELECT id FROM cmdb.workflow_definitions WHERE lower(key) = lower($1)")
                    .bind(&w.key)
                    .fetch_optional(&mut *self.conn)
                    .await?;
            let Some(id) = existing else {
                let create = WorkflowDefinitionCreate {
                    key: w.key.clone(),
                    name: w.name.clone(),
                    description: w.description.clone(),
                    class_id,
                    include_subclasses: Some(w.include_subclasses),
                    state_attribute_id: state_attribute,
                    auto_start: Some(w.auto_start),
                    is_active: Some(w.is_active),
                };
                let d = service::create_in(self.conn, self.ctx, &create).await.map_err(|e| at(&path, e))?;
                self.publish_graph(&d, &fields, &draft, &granted, &facts, &path, warnings).await?;
                let d = service::load(self.conn, d.id, true).await?;
                check_grant_keys(self.conn, &d, w, &path).await?;
                service::set_grants_in(self.conn, self.ctx, &d, &rows).await.map_err(|e| at(&path, e))?;
                check_approver_steps(self.conn, &d, w, &path).await?;
                let d = service::load(self.conn, d.id, true).await?;
                approvers::set_in(self.conn, self.ctx, &d, &wanted_approvers).await.map_err(|e| at(&path, e))?;
                self.record(SECTION, w.key.clone(), Some(ChangeAction::Create), Vec::new());
                continue;
            };

            let d = service::load(self.conn, id, true).await?;
            if d.class_id != class_id {
                return Err(invalid(
                    &path,
                    "class",
                    format!(
                        "Workflow \"{}\" runs on type \"{}\" here; a workflow never moves to another type",
                        d.key, d.class_key
                    ),
                    "immutable",
                ));
            }
            let before = old.get(&w.key.to_lowercase()).copied();
            let mut changes = Vec::new();

            // Settings, as PATCH would change them.
            let update = WorkflowDefinitionUpdate {
                version: d.version,
                name: (d.name != w.name).then(|| w.name.clone()),
                description: (d.description != w.description).then(|| w.description.clone()),
                include_subclasses: (d.include_subclasses != w.include_subclasses).then_some(w.include_subclasses),
                state_attribute_id: (d.state_attribute_id != state_attribute).then_some(state_attribute),
                auto_start: (d.auto_start != w.auto_start).then_some(w.auto_start),
                is_active: (d.is_active != w.is_active).then_some(w.is_active),
            };
            let d = if update.columns().0.is_empty() {
                d
            } else {
                let was = before.map(settings).unwrap_or_else(|| {
                    json!({ "name": d.name, "description": d.description, "includeSubclasses": d.include_subclasses,
                            "stateAttribute": d.state_attribute_key, "autoStart": d.auto_start,
                            "isActive": d.is_active })
                });
                changes.extend(diff(&was, &settings(w)));
                service::update_in(self.conn, self.ctx, id, &update).await.map_err(|e| at(&path, e))?.1
            };

            // The graph: a new version only when it differs from the current one.
            let now = match d.current_version_no {
                None => None,
                Some(no) => {
                    let version_id: Uuid =
                        sqlx::query_scalar("SELECT current_version_id FROM cmdb.workflow_definitions WHERE id = $1")
                            .bind(id)
                            .fetch_one(&mut *self.conn)
                            .await?;
                    let row = current_version(self.conn, version_id).await?;
                    let stored = graph::load(self.conn, row).await?;
                    Some((no, stored.checksum(&fields.model)))
                }
            };
            if now.as_ref().map(|(_, sum)| sum) != Some(&incoming) {
                if let Some(v) = service::draft_row(self.conn, id, false).await? {
                    return Err(at(
                        &format!("{path}.graph"),
                        AppError::new(
                            ErrorCode::Conflict,
                            format!(
                                "Workflow \"{}\" has an unpublished draft (version {}) here, and the file changes its \
                                 graph. Publish or delete the draft, then import again",
                                d.key, v.version_no
                            ),
                        ),
                    ));
                }
                let no = self.publish_graph(&d, &fields, &draft, &granted, &facts, &path, warnings).await?;
                let from = now.map_or(Value::Null, |(no, sum)| json!({ "versionNo": no, "checksum": sum }));
                changes.push(FieldChange {
                    field: "graph".into(),
                    from,
                    to: json!({ "versionNo": no, "checksum": incoming }),
                });
            }

            // Grants are replaced.
            check_grant_keys(self.conn, &d, w, &path).await?;
            let was = grant_set(&specs(service::grant_rows(self.conn, id).await?));
            let wanted = grant_set(&w.grants);
            if was != wanted {
                let d = service::load(self.conn, id, true).await?;
                service::set_grants_in(self.conn, self.ctx, &d, &rows).await.map_err(|e| at(&path, e))?;
                changes.push(FieldChange { field: "grants".into(), from: was, to: wanted });
            }

            // Approvers are replaced.
            check_approver_steps(self.conn, &d, w, &path).await?;
            let was = approvers::load(self.conn, id).await?;
            if !approvers::same(&was, &wanted_approvers) {
                let d = service::load(self.conn, id, true).await?;
                approvers::set_in(self.conn, self.ctx, &d, &wanted_approvers).await.map_err(|e| at(&path, e))?;
                changes.push(FieldChange {
                    field: "approvers".into(),
                    from: json!(approvers::specs(&was)),
                    to: json!(approvers::specs(&wanted_approvers)),
                });
            }

            let action = (!changes.is_empty()).then_some(ChangeAction::Update);
            self.record(SECTION, w.key.clone(), action, changes);
        }
        Ok(())
    }
}

/// The workflows of `file` that name a built-in class by role apply to this
/// install's class of that role (see `system_roles`).
pub(super) fn match_classes(
    file: &mut ConfigFile,
    class_of_role: &HashMap<ClassSystemRole, &str>,
    renamed: &HashMap<String, String>,
    role_of_class: &HashMap<&str, ClassSystemRole>,
) {
    for w in file.workflows.iter_mut().flatten() {
        match w.class_system_role.and_then(|r| class_of_role.get(&r)) {
            Some(here) => w.class = (*here).to_owned(),
            None => {
                if let Some(k) = renamed.get(&w.class) {
                    w.class = k.clone();
                }
            }
        }
        w.class_system_role = role_of_class.get(w.class.as_str()).copied();
    }
}
