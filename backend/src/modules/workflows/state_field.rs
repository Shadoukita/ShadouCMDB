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
}

impl Driver {
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

fn refused(def: &EffectiveAttributeRow, driver: &Driver) -> FieldError {
    FieldError {
        location: FieldLocation::Body,
        field: format!("attributes.{}", def.key),
        message: format!(
            "{} is set by the workflow {}: run a transition of its instance instead",
            def.label, driver.definition_key
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

    /// A new CI of `class_id` may leave a driven field out, or give it the
    /// value the workflow starts with or the field's default (what a form
    /// fills in); any other value is refused.
    pub fn check_create(
        &self,
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
            errors.push(refused(def, driver));
        }
        error(errors)
    }

    /// A change of an existing CI may resend a driven field's current value
    /// (a form saving every field), but not set another one or clear it.
    pub fn check_update(
        &self,
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
                errors.push(refused(def, driver));
            }
        }
        error(errors)
    }
}
