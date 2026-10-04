//! Evaluating a transition's condition (design SHAA-1411 §3.3) against a CI's
//! values: its current values merged with the values sent with the
//! transition. Conditions are stored with field ids and were type-checked at
//! publish ([`super::condition`]); the values come in the API's form, except
//! lookup values, which the caller gives as the key of the value (conditions
//! name lookup values by key).
//!
//! The result is the list of leaves that fail: empty when the condition holds.
//! A failing `any` group reports each of its leaves, so the caller can show
//! every way to satisfy it.

use std::cmp::Ordering;
use std::collections::HashMap;

use chrono::{DateTime, FixedOffset, NaiveDate};
use serde_json::Value;
use uuid::Uuid;

use super::condition::Op;
use crate::modules::classes::AttributeDataType;
use crate::schema::model::Model;

/// One leaf of a condition that does not hold.
#[derive(Debug, Clone, PartialEq)]
pub struct Failure {
    /// Nil when the stored condition names a field the model no longer has.
    pub attribute: Uuid,
    /// The field's key, for `fields.<key>` in error details.
    pub key: String,
    pub message: String,
}

/// The values a condition is evaluated against, by field id. Absent and null
/// are both "not set"; so is an empty text.
pub struct Subject<'a> {
    pub model: &'a Model,
    pub values: &'a HashMap<Uuid, Value>,
}

/// The failing leaves of `condition` (the stored form): empty when it holds.
pub fn failures(condition: &Value, s: &Subject<'_>) -> Vec<Failure> {
    node(condition, s).1
}

fn node(v: &Value, s: &Subject<'_>) -> (bool, Vec<Failure>) {
    let Some(obj) = v.as_object() else { return (false, vec![unknown("Malformed condition")]) };
    if let Some(children) = obj.get("all").and_then(Value::as_array) {
        let mut failed = Vec::new();
        for c in children {
            let (ok, f) = node(c, s);
            if !ok {
                failed.extend(f);
            }
        }
        return (failed.is_empty(), failed);
    }
    if let Some(children) = obj.get("any").and_then(Value::as_array) {
        let mut failed = Vec::new();
        for c in children {
            let (ok, f) = node(c, s);
            if ok {
                return (true, Vec::new());
            }
            failed.extend(f);
        }
        return (false, failed);
    }
    match leaf(obj, s) {
        Ok(()) => (true, Vec::new()),
        Err(f) => (false, vec![f]),
    }
}

fn unknown(message: &str) -> Failure {
    Failure { attribute: Uuid::nil(), key: String::new(), message: message.into() }
}

fn leaf(obj: &serde_json::Map<String, Value>, s: &Subject<'_>) -> Result<(), Failure> {
    let id = obj.get("field").and_then(Value::as_str).and_then(|f| f.parse::<Uuid>().ok());
    let field = id.and_then(|id| s.model.field(id));
    let (Some(field), Some(op)) = (field, obj.get("op").and_then(Value::as_str).and_then(Op::parse)) else {
        return Err(unknown("The condition names a field that no longer exists"));
    };
    let t = field.data_type;
    let operand = obj.get("value");
    let current = s.values.get(&field.id).filter(|v| is_set(v));
    let eq = |b: &Value| current.is_some_and(|a| compare(t, a, b) == Some(Ordering::Equal));
    let holds = match op {
        Op::IsSet => current.is_some(),
        Op::IsNotSet => current.is_none(),
        Op::Eq => operand.is_some_and(eq),
        Op::Ne => !operand.is_some_and(eq),
        Op::In => list(operand).iter().any(eq),
        Op::NotIn => !list(operand).iter().any(eq),
        Op::Gt | Op::Gte | Op::Lt | Op::Lte => {
            let ord = current.zip(operand).and_then(|(a, b)| compare(t, a, b));
            match (op, ord) {
                (_, None) => false,
                (Op::Gt, Some(o)) => o == Ordering::Greater,
                (Op::Gte, Some(o)) => o != Ordering::Less,
                (Op::Lt, Some(o)) => o == Ordering::Less,
                (_, Some(o)) => o != Ordering::Greater,
            }
        }
        Op::Contains => match (current.and_then(Value::as_str), operand.and_then(Value::as_str)) {
            (Some(a), Some(b)) => a.to_lowercase().contains(&b.to_lowercase()),
            _ => false,
        },
    };
    if holds {
        return Ok(());
    }
    Err(Failure { attribute: field.id, key: field.key.clone(), message: describe(&field.label, op, operand) })
}

fn is_set(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::String(s) => !s.is_empty(),
        _ => true,
    }
}

fn list(v: Option<&Value>) -> &[Value] {
    v.and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default()
}

/// Orders two values of a field of type `t`; None when they do not compare.
fn compare(t: AttributeDataType, a: &Value, b: &Value) -> Option<Ordering> {
    use AttributeDataType as T;
    match t {
        T::Number | T::Integer => a.as_f64()?.partial_cmp(&b.as_f64()?),
        T::Boolean => Some(a.as_bool()?.cmp(&b.as_bool()?)),
        T::Date => {
            let p = |v: &Value| NaiveDate::parse_from_str(v.as_str()?, "%Y-%m-%d").ok();
            Some(p(a)?.cmp(&p(b)?))
        }
        T::Datetime => {
            let p = |v: &Value| DateTime::<FixedOffset>::parse_from_rfc3339(v.as_str()?).ok();
            Some(p(a)?.cmp(&p(b)?))
        }
        // CI ids: the same CI whatever the case of the hex digits.
        T::Reference => Some(a.as_str()?.to_lowercase().cmp(&b.as_str()?.to_lowercase())),
        T::Text | T::Enum | T::Lookup | T::Ip | T::Cidr => Some(a.as_str()?.cmp(b.as_str()?)),
    }
}

fn show(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(a) => a.iter().map(show).collect::<Vec<_>>().join(", "),
        other => other.to_string(),
    }
}

/// What the leaf requires, for the person who has to fix it.
fn describe(label: &str, op: Op, value: Option<&Value>) -> String {
    let v = value.map(show).unwrap_or_default();
    match op {
        Op::IsSet => format!("{label} must be set"),
        Op::IsNotSet => format!("{label} must be empty"),
        Op::Eq => format!("{label} must be {v}"),
        Op::Ne => format!("{label} must not be {v}"),
        Op::In => format!("{label} must be one of {v}"),
        Op::NotIn => format!("{label} must not be one of {v}"),
        Op::Gt => format!("{label} must be greater than {v}"),
        Op::Gte => format!("{label} must be at least {v}"),
        Op::Lt => format!("{label} must be less than {v}"),
        Op::Lte => format!("{label} must be at most {v}"),
        Op::Contains => format!("{label} must contain \"{v}\""),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::schema::model::Field;

    fn field(key: &str, data_type: AttributeDataType) -> Field {
        Field {
            id: Uuid::new_v4(),
            class_id: Uuid::nil(),
            key: key.into(),
            label: key.into(),
            data_type,
            enum_values: None,
            is_required: false,
            is_active: true,
            sort_order: 0,
            lookup_list_id: None,
            system_role: None,
        }
    }

    struct World {
        model: Model,
    }

    impl World {
        fn new() -> World {
            let fields = vec![
                field("environment", AttributeDataType::Lookup),
                field("owner", AttributeDataType::Reference),
                field("risk", AttributeDataType::Integer),
                field("notes", AttributeDataType::Text),
                field("go_live", AttributeDataType::Date),
                field("seen", AttributeDataType::Datetime),
                field("tier", AttributeDataType::Enum),
            ];
            World { model: Model { areas: Vec::new(), classes: Vec::new(), fields } }
        }

        fn id(&self, key: &str) -> Uuid {
            self.model.fields.iter().find(|f| f.key == key).unwrap().id
        }

        /// The condition in the stored form (field ids).
        fn stored(&self, v: Value) -> Value {
            match v {
                Value::Object(mut o) => {
                    if let Some(Value::String(k)) = o.get("field").cloned() {
                        o.insert("field".into(), Value::String(self.id(&k).to_string()));
                    }
                    for g in ["all", "any"] {
                        if let Some(Value::Array(c)) = o.remove(g) {
                            o.insert(g.into(), Value::Array(c.into_iter().map(|c| self.stored(c)).collect()));
                        }
                    }
                    Value::Object(o)
                }
                other => other,
            }
        }

        fn failing(&self, condition: Value, values: &[(&str, Value)]) -> Vec<String> {
            let values: HashMap<Uuid, Value> = values.iter().map(|(k, v)| (self.id(k), v.clone())).collect();
            failures(&self.stored(condition), &Subject { model: &self.model, values: &values })
                .into_iter()
                .map(|f| f.key)
                .collect()
        }
    }

    #[test]
    fn the_design_example_holds_or_names_its_failing_leaves() {
        let w = World::new();
        let c = json!({ "all": [
            { "field": "environment", "op": "in", "value": ["prod", "dr"] },
            { "any": [ { "field": "owner", "op": "isSet" }, { "field": "risk", "op": "lte", "value": 2 } ] }
        ] });
        let owner = json!(Uuid::new_v4().to_string());
        assert!(w.failing(c.clone(), &[("environment", json!("prod")), ("owner", owner)]).is_empty());
        assert!(w.failing(c.clone(), &[("environment", json!("dr")), ("risk", json!(2))]).is_empty());
        assert_eq!(w.failing(c.clone(), &[("environment", json!("test")), ("risk", json!(1))]), ["environment"]);
        // A failing `any` names every way out.
        assert_eq!(w.failing(c.clone(), &[("environment", json!("prod")), ("risk", json!(3))]), ["owner", "risk"]);
        assert_eq!(w.failing(c, &[]), ["environment", "owner", "risk"]);
    }

    #[test]
    fn operators_compare_by_type_and_unset_values_fail_comparisons() {
        let w = World::new();
        let ok = |c: Value, v: &[(&str, Value)]| w.failing(c, v).is_empty();
        assert!(ok(json!({ "field": "notes", "op": "isNotSet" }), &[("notes", json!(""))]));
        assert!(!ok(json!({ "field": "notes", "op": "isSet" }), &[("notes", json!(""))]));
        assert!(ok(json!({ "field": "notes", "op": "contains", "value": "CAB" }), &[("notes", json!("ok by cab"))]));
        assert!(!ok(json!({ "field": "notes", "op": "contains", "value": "CAB" }), &[]));
        assert!(ok(json!({ "field": "risk", "op": "gt", "value": 2 }), &[("risk", json!(2.5))]));
        assert!(!ok(json!({ "field": "risk", "op": "gt", "value": 2 }), &[("risk", json!(2))]));
        assert!(ok(json!({ "field": "risk", "op": "gte", "value": 2 }), &[("risk", json!(2))]));
        assert!(!ok(json!({ "field": "risk", "op": "lt", "value": 2 }), &[]), "unset is not less than anything");
        assert!(ok(json!({ "field": "risk", "op": "ne", "value": 2 }), &[]), "unset is not equal to anything");
        assert!(ok(
            json!({ "field": "go_live", "op": "lt", "value": "2027-01-01" }),
            &[("go_live", json!("2026-12-31"))]
        ));
        assert!(ok(
            json!({ "field": "seen", "op": "gte", "value": "2026-10-01T12:00:00Z" }),
            &[("seen", json!("2026-10-01T14:00:00+02:00"))]
        ));
        assert!(ok(json!({ "field": "tier", "op": "notIn", "value": ["gold"] }), &[("tier", json!("silver"))]));
        let owner = Uuid::new_v4().to_string();
        assert!(ok(json!({ "field": "owner", "op": "eq", "value": owner.to_uppercase() }), &[("owner", json!(owner))]));
    }

    #[test]
    fn messages_say_what_is_required() {
        let w = World::new();
        let values = HashMap::new();
        let f = failures(
            &w.stored(json!({ "field": "environment", "op": "in", "value": ["prod", "dr"] })),
            &Subject { model: &w.model, values: &values },
        );
        assert_eq!(f[0].message, "environment must be one of prod, dr");
        let f = failures(
            &json!({ "field": Uuid::new_v4().to_string(), "op": "isSet" }),
            &Subject { model: &w.model, values: &values },
        );
        assert_eq!((f[0].attribute, f[0].key.as_str()), (Uuid::nil(), ""));
    }
}
