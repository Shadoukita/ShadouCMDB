//! Workflow-driven state fields (design §3.2, Q3; slice S3b, SHAA-1698).
//!
//! While an active workflow drives a state field of a type, the field follows
//! the workflow: nobody writes it directly, whether or not the CI has an
//! instance. Every CI write plans through `items::plan`, which asks
//! [`StateFields`] before it accepts a value for such a field: a PATCH, bulk
//! edit and import rows are refused with `WORKFLOW_CONTROLLED_FIELD` (409).
//! The workflow engine's own writes pass [`StateFields::WorkflowWrite`], the
//! explicit bypass.

use serde_json::{Map, Value};
use sqlx::PgConnection;
use uuid::Uuid;

use crate::api::context::RequestContext;
use crate::data::classes::EffectiveAttributeRow;
use crate::data::items::StoredValue;
use crate::http::error::{AppError, ErrorCode, FieldError, FieldLocation};
use crate::schema::model::Model;

/// An active workflow that drives a state field.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Driver {
    pub definition_key: String,
    pub class_id: Uuid,
    pub include_subclasses: bool,
    pub attribute_id: Uuid,
    /// The value its current version's initial state sets (a new CI may start with it).
    pub initial_value_id: Option<Uuid>,
    /// GH#718: the caller may not view every type it covers, so it answers 404
    /// to them and a message must not name it (see [`Driver::named`]).
    #[sqlx(skip)]
    pub hidden: bool,
}

impl Driver {
    /// The problem param that names it: `workflow` (its key), or
    /// `hiddenWorkflow` when [`Driver::named`] must not name it.
    pub fn param(&self) -> (&'static str, String) {
        if self.hidden { ("hiddenWorkflow", "true".to_owned()) } else { ("workflow", self.definition_key.clone()) }
    }

    /// "the active workflow `key`", or a phrase that does not name it when it
    /// is hidden from the caller.
    pub fn named(&self) -> String {
        if self.hidden {
            "another active workflow on these CIs".to_owned()
        } else {
            format!("the active workflow {}", self.definition_key)
        }
    }

    /// This driver as `ctx` gets it: [`Driver::hidden`] unless the caller may
    /// view every type it covers (GH#718, GH#734).
    fn for_caller(&self, ctx: &RequestContext, model: &Model) -> Driver {
        let covered = if self.include_subclasses { model.subtree(self.class_id) } else { vec![self.class_id] };
        Driver { hidden: !ctx.may_view_all(&covered), ..self.clone() }
    }

    fn covers(&self, model: &Model, class_id: Uuid) -> bool {
        self.class_id == class_id
            || (self.include_subclasses && model.lineage(class_id).iter().any(|c| c.id == self.class_id))
    }
}

/// Which state fields a CI write may set.
#[derive(Debug, Clone)]
pub enum StateFields {
    /// A direct write: the fields these workflows drive are refused.
    Guarded(Vec<Driver>),
    /// The workflow engine itself (start, transition, force, auto-start): it is
    /// what keeps the field in step, so nothing is refused.
    WorkflowWrite,
}

/// The refusal of a driven field, naming its workflow only when the caller
/// may view it (GH#734: one they may not answers 404, so it is not named here).
fn refused(ctx: &RequestContext, model: &Model, def: &EffectiveAttributeRow, driver: &Driver) -> FieldError {
    FieldError {
        location: FieldLocation::Body,
        field: format!("attributes.{}", def.key),
        message: format!(
            "{} is set by {}: run a transition of its instance instead",
            def.label,
            driver.for_caller(ctx, model).named()
        ),
        code: "workflow_controlled".into(),
    }
}

fn error(errors: Vec<FieldError>) -> Result<(), AppError> {
    if errors.is_empty() {
        return Ok(());
    }
    let message = if errors.len() == 1 {
        errors[0].message.clone()
    } else {
        "These fields are set by workflows: run a transition of their instances instead".to_owned()
    };
    Err(AppError::new(ErrorCode::WorkflowControlledField, message).with_details(errors))
}

fn value_id(v: &Value) -> Option<Uuid> {
    v.as_str().and_then(|s| Uuid::parse_str(s).ok())
}

impl StateFields {
    /// The state fields of every active workflow, for a direct write.
    pub async fn load(conn: &mut PgConnection) -> sqlx::Result<StateFields> {
        let drivers = sqlx::query_as::<_, Driver>(
            "SELECT d.key AS definition_key, d.class_id, d.include_subclasses, d.state_attribute_id AS attribute_id,
                    s.state_value_id AS initial_value_id
             FROM cmdb.workflow_definitions d
             LEFT JOIN cmdb.workflow_versions v ON v.id = d.current_version_id
             LEFT JOIN cmdb.workflow_states s ON s.id = v.initial_state_id
             WHERE d.is_active AND d.state_attribute_id IS NOT NULL
             ORDER BY d.key",
        )
        .fetch_all(conn)
        .await?;
        Ok(StateFields::Guarded(drivers))
    }

    /// The active workflows other than `definition_key` that drive a field on
    /// a CI a workflow on `class_id` (with `include_subclasses`) may cover: a
    /// transition of that workflow must not take their state field (GH#668).
    /// The ones `ctx` may not view are marked [`Driver::hidden`] (GH#718).
    pub fn overlapping(
        &self,
        ctx: &RequestContext,
        model: &Model,
        definition_key: &str,
        class_id: Uuid,
        include_subclasses: bool,
    ) -> Vec<Driver> {
        let StateFields::Guarded(drivers) = self else { return Vec::new() };
        drivers
            .iter()
            .filter(|d| d.definition_key != definition_key)
            .filter(|d| {
                d.covers(model, class_id)
                    || (include_subclasses && model.lineage(d.class_id).iter().any(|c| c.id == class_id))
            })
            .map(|d| d.for_caller(ctx, model))
            .collect()
    }

    /// The driven fields among `defs` for a CI of `class_id`, with their workflow.
    fn driven<'a, 'd>(
        &'a self,
        model: &Model,
        class_id: Uuid,
        defs: &'d [EffectiveAttributeRow],
    ) -> Vec<(&'d EffectiveAttributeRow, &'a Driver)> {
        let StateFields::Guarded(drivers) = self else { return Vec::new() };
        drivers
            .iter()
            .filter(|d| d.covers(model, class_id))
            .filter_map(|d| Some((defs.iter().find(|a| a.id == d.attribute_id)?, d)))
            .collect()
    }

    /// Keys of the fields these workflows drive on a CI of `class_id`.
    pub fn driven_keys(&self, model: &Model, class_id: Uuid) -> Vec<String> {
        let StateFields::Guarded(drivers) = self else { return Vec::new() };
        drivers
            .iter()
            .filter(|d| d.covers(model, class_id))
            .filter_map(|d| model.field(d.attribute_id).map(|f| f.key.clone()))
            .collect()
    }

    /// A new CI of `class_id` may leave a driven field out, or give it the
    /// value the workflow starts with or the field's default (what a form
    /// fills in); any other value is refused.
    pub fn check_create(
        &self,
        ctx: &RequestContext,
        model: &Model,
        class_id: Uuid,
        defs: &[EffectiveAttributeRow],
        input: Option<&Map<String, Value>>,
    ) -> Result<(), AppError> {
        let mut errors = Vec::new();
        for (def, driver) in self.driven(model, class_id, defs) {
            let Some(v) = input.and_then(|i| i.get(&def.key)).filter(|v| !v.is_null()) else { continue };
            let default = def.default_value.as_ref().map(|d| &d.0);
            if value_id(v).is_some_and(|id| Some(id) == driver.initial_value_id) || Some(v) == default {
                continue;
            }
            errors.push(refused(ctx, model, def, driver));
        }
        error(errors)
    }

    /// A change of an existing CI may resend a driven field's current value
    /// (a form saving every field), but not set another one or clear it.
    #[allow(clippy::too_many_arguments)]
    pub fn check_update(
        &self,
        ctx: &RequestContext,
        model: &Model,
        class_id: Uuid,
        defs: &[EffectiveAttributeRow],
        before: &Map<String, Value>,
        set: &[(&EffectiveAttributeRow, StoredValue)],
        clear: &[Uuid],
    ) -> Result<(), AppError> {
        let mut errors = Vec::new();
        for (def, driver) in self.driven(model, class_id, defs) {
            let current = before.get(&def.key).filter(|v| !v.is_null()).and_then(value_id);
            let changes = match set.iter().find(|(d, _)| d.id == def.id) {
                Some((_, StoredValue::Lookup(id))) => current != Some(*id),
                Some(_) => true,
                None => clear.contains(&def.id) && current.is_some(),
            };
            if changes {
                errors.push(refused(ctx, model, def, driver));
            }
        }
        error(errors)
    }
}
