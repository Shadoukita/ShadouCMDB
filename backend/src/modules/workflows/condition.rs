//! The condition language of a transition (design SHAA-1411 §3.3): a bounded
//! JSON tree of `all` / `any` groups and leaves `{field, op, value}`.
//!
//! The API speaks attribute keys; storage holds attribute ids, so a condition
//! survives nothing that could silently change its meaning (the key of a
//! field never changes, but its id is what `workflow_version_attribute_refs`
//! protects). Enum and lookup values are referenced by key in both forms, so
//! they survive renames and match the configuration export.
//!
//! There is no regex (no ReDoS) and no cross-CI lookup (Q4: relationship
//! conditions come after v0.4.0); the tree leaves room for a `related` node.
//! Evaluation is the runtime's (S3); this module parses and type-checks.

use serde_json::{Map, Value, json};
use uuid::Uuid;

use crate::api::validate;
use crate::http::error::{FieldError, FieldLocation};
use crate::modules::classes::AttributeDataType;
use crate::schema::model::Field;

/// Nesting levels, the root group counting as one.
pub const MAX_DEPTH: usize = 4;
/// Leaves in one condition.
pub const MAX_LEAVES: usize = 32;
/// Serialised size of one condition (the column's CHECK is on the stored form).
pub const MAX_BYTES: usize = 16 * 1024;
/// Values in one `in` / `notIn` list.
pub const MAX_LIST: usize = 100;
/// Length of one text operand.
const MAX_TEXT: usize = 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Eq,
    Ne,
    In,
    NotIn,
    IsSet,
    IsNotSet,
    Gt,
    Gte,
    Lt,
    Lte,
    Contains,
}

pub const OPS: &[&str] = &["eq", "ne", "in", "notIn", "isSet", "isNotSet", "gt", "gte", "lt", "lte", "contains"];

impl Op {
    pub fn parse(s: &str) -> Option<Op> {
        Some(match s {
            "eq" => Op::Eq,
            "ne" => Op::Ne,
            "in" => Op::In,
            "notIn" => Op::NotIn,
            "isSet" => Op::IsSet,
            "isNotSet" => Op::IsNotSet,
            "gt" => Op::Gt,
            "gte" => Op::Gte,
            "lt" => Op::Lt,
            "lte" => Op::Lte,
            "contains" => Op::Contains,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Op::Eq => "eq",
            Op::Ne => "ne",
            Op::In => "in",
            Op::NotIn => "notIn",
            Op::IsSet => "isSet",
            Op::IsNotSet => "isNotSet",
            Op::Gt => "gt",
            Op::Gte => "gte",
            Op::Lt => "lt",
            Op::Lte => "lte",
            Op::Contains => "contains",
        }
    }
}

/// A parsed, type-checked condition.
#[derive(Debug, Clone, PartialEq)]
pub enum Condition {
    All(Vec<Condition>),
    Any(Vec<Condition>),
    Leaf { attribute: Uuid, op: Op, value: Option<Value> },
}

/// What a condition can refer to: the fields of the workflow's type (its own
/// and its ancestors'), and the keys of the values of their lookup lists.
pub trait Scope {
    /// The field a leaf names: by key in the API form, by id in storage.
    fn field(&self, name: &str) -> Result<&Field, String>;
    /// Whether `key` is a value of lookup list `list`.
    fn lookup_value(&self, list: Uuid, key: &str) -> bool;
}

struct Parser<'a> {
    scope: &'a dyn Scope,
    errors: Vec<FieldError>,
    leaves: usize,
}

fn problem(path: &str, message: impl Into<String>, code: &str) -> FieldError {
    FieldError { location: FieldLocation::Body, field: path.to_owned(), message: message.into(), code: code.into() }
}

/// Parses and type-checks a condition at `path` (e.g. `transitions[2].conditions`).
pub fn parse(value: &Value, path: &str, scope: &dyn Scope) -> Result<Condition, Vec<FieldError>> {
    if serde_json::to_vec(value).map_or(usize::MAX, |b| b.len()) > MAX_BYTES {
        return Err(vec![problem(path, format!("A condition is at most {MAX_BYTES} bytes"), "too_large")]);
    }
    let mut p = Parser { scope, errors: Vec::new(), leaves: 0 };
    let parsed = p.node(value, path, 1);
    if p.leaves > MAX_LEAVES {
        p.errors.push(problem(path, format!("A condition has at most {MAX_LEAVES} leaves"), "too_many_leaves"));
    }
    match parsed {
        Some(c) if p.errors.is_empty() => Ok(c),
        _ => Err(p.errors),
    }
}

impl Parser<'_> {
    fn node(&mut self, value: &Value, path: &str, depth: usize) -> Option<Condition> {
        let Some(obj) = value.as_object() else {
            self.errors.push(problem(path, "Expected an object: {\"all\": [...]}, {\"any\": [...]} or a leaf", "type"));
            return None;
        };
        if depth > MAX_DEPTH {
            self.errors.push(problem(path, format!("Conditions nest at most {MAX_DEPTH} levels deep"), "too_deep"));
            return None;
        }
        for group in ["all", "any"] {
            if let Some(children) = obj.get(group) {
                if obj.len() != 1 {
                    self.errors.push(problem(path, format!("A group has only the \"{group}\" property"), "unknown"));
                    return None;
                }
                let Some(children) = children.as_array().filter(|a| !a.is_empty()) else {
                    self.errors.push(problem(
                        &format!("{path}.{group}"),
                        "Expected a non-empty array of conditions",
                        "type",
                    ));
                    return None;
                };
                let mut parsed = Vec::with_capacity(children.len());
                for (i, child) in children.iter().enumerate() {
                    if let Some(c) = self.node(child, &format!("{path}.{group}[{i}]"), depth + 1) {
                        parsed.push(c);
                    }
                }
                if parsed.len() != children.len() {
                    return None;
                }
                return Some(if group == "all" { Condition::All(parsed) } else { Condition::Any(parsed) });
            }
        }
        if obj.contains_key("related") {
            self.errors.push(problem(path, "Relationship conditions are not supported yet", "unsupported"));
            return None;
        }
        self.leaf(obj, path)
    }

    fn leaf(&mut self, obj: &Map<String, Value>, path: &str) -> Option<Condition> {
        self.leaves += 1;
        if let Some(extra) = obj.keys().find(|k| !matches!(k.as_str(), "field" | "op" | "value")) {
            self.errors.push(problem(&format!("{path}.{extra}"), "Unknown property", "unknown"));
            return None;
        }
        let Some(name) = obj.get("field").and_then(Value::as_str) else {
            self.errors.push(problem(&format!("{path}.field"), "Required: the key of a field of the type", "required"));
            return None;
        };
        let field = match self.scope.field(name) {
            Ok(f) => f,
            Err(message) => {
                self.errors.push(problem(&format!("{path}.field"), message, "unknown_attribute"));
                return None;
            }
        };
        let Some(op) = obj.get("op").and_then(Value::as_str).and_then(Op::parse) else {
            self.errors.push(problem(
                &format!("{path}.op"),
                format!("Required: one of {}", OPS.join(", ")),
                "invalid_op",
            ));
            return None;
        };
        let value = obj.get("value");
        let vpath = format!("{path}.value");
        let t = field.data_type;
        let ok = match op {
            Op::IsSet | Op::IsNotSet => {
                if value.is_some() {
                    self.errors.push(problem(&vpath, format!("{} takes no value", op.as_str()), "unexpected"));
                    false
                } else {
                    true
                }
            }
            Op::Gt | Op::Gte | Op::Lt | Op::Lte
                if !matches!(
                    t,
                    AttributeDataType::Number
                        | AttributeDataType::Integer
                        | AttributeDataType::Date
                        | AttributeDataType::Datetime
                ) =>
            {
                self.errors.push(problem(
                    &format!("{path}.op"),
                    format!("{} compares numbers, integers, dates and datetimes, not {}", op.as_str(), t.as_str()),
                    "op_type",
                ));
                false
            }
            Op::Contains if t != AttributeDataType::Text => {
                self.errors.push(problem(
                    &format!("{path}.op"),
                    format!("contains applies to text fields, not {}", t.as_str()),
                    "op_type",
                ));
                false
            }
            Op::In | Op::NotIn => match value.and_then(Value::as_array) {
                Some(list) if !list.is_empty() && list.len() <= MAX_LIST => {
                    let before = self.errors.len();
                    for (i, v) in list.iter().enumerate() {
                        self.operand(field, v, &format!("{vpath}[{i}]"));
                    }
                    self.errors.len() == before
                }
                _ => {
                    self.errors.push(problem(&vpath, format!("Expected an array of 1 to {MAX_LIST} values"), "type"));
                    false
                }
            },
            _ => match value {
                Some(v) => self.operand(field, v, &vpath),
                None => {
                    self.errors.push(problem(&vpath, "Required", "required"));
                    false
                }
            },
        };
        ok.then(|| Condition::Leaf { attribute: field.id, op, value: value.cloned() })
    }

    /// One value compared with `field`; pushes a problem and returns false when it does not fit.
    fn operand(&mut self, field: &Field, v: &Value, path: &str) -> bool {
        use AttributeDataType as T;
        let s = v.as_str();
        let fits = match field.data_type {
            T::Text => s.is_some_and(|s| !s.is_empty() && s.chars().count() <= MAX_TEXT),
            T::Number => v.is_number(),
            T::Integer => v.is_i64() || v.is_u64(),
            T::Boolean => v.is_boolean(),
            T::Date => s.is_some_and(validate::is_date),
            T::Datetime => s.is_some_and(validate::is_datetime),
            T::Ip => s.is_some_and(validate::is_ip),
            T::Cidr => s.is_some_and(validate::is_ip_or_cidr),
            T::Reference => s.is_some_and(validate::is_uuid),
            T::Enum => {
                if let Some(s) = s
                    && !field.enum_list().iter().any(|e| e == s)
                {
                    self.errors.push(problem(
                        path,
                        format!("\"{s}\" is not a value of {}", field.key),
                        "unknown_value",
                    ));
                    return false;
                }
                s.is_some()
            }
            T::Lookup => {
                if let (Some(s), Some(list)) = (s, field.lookup_list_id)
                    && !self.scope.lookup_value(list, s)
                {
                    self.errors.push(problem(
                        path,
                        format!("\"{s}\" is not the key of a value of the list of {}", field.key),
                        "unknown_value",
                    ));
                    return false;
                }
                s.is_some()
            }
        };
        if !fits {
            self.errors.push(problem(
                path,
                format!("Not a valid {} value for {}", field.data_type.as_str(), field.key),
                "type",
            ));
        }
        fits
    }
}

impl Condition {
    /// Every field the condition reads.
    pub fn attributes(&self, out: &mut Vec<Uuid>) {
        match self {
            Condition::All(c) | Condition::Any(c) => c.iter().for_each(|c| c.attributes(out)),
            Condition::Leaf { attribute, .. } => out.push(*attribute),
        }
    }

    /// The JSON form, naming each field with `name` (its id for storage, its key for the API).
    pub fn to_json(&self, name: &dyn Fn(Uuid) -> String) -> Value {
        match self {
            Condition::All(c) => json!({ "all": c.iter().map(|c| c.to_json(name)).collect::<Vec<_>>() }),
            Condition::Any(c) => json!({ "any": c.iter().map(|c| c.to_json(name)).collect::<Vec<_>>() }),
            Condition::Leaf { attribute, op, value } => {
                let mut leaf = Map::new();
                leaf.insert("field".into(), Value::String(name(*attribute)));
                leaf.insert("op".into(), Value::String(op.as_str().into()));
                if let Some(v) = value {
                    leaf.insert("value".into(), v.clone());
                }
                Value::Object(leaf)
            }
        }
    }
}

/// Rewrites the field names of a stored condition (ids) to keys without
/// checking anything, for display: a field the model no longer has keeps its
/// id, and the lint reports it. Leaves come back as `field`, `op`, `value`
/// (jsonb does not keep the order they were written in).
pub fn rename_fields(v: &Value, name: &dyn Fn(&str) -> Option<String>) -> Value {
    const LEAF: [&str; 3] = ["field", "op", "value"];
    match v {
        Value::Object(obj) => Value::Object(
            LEAF.iter()
                .filter_map(|k| obj.get_key_value(*k))
                .chain(obj.iter().filter(|(k, _)| !LEAF.contains(&k.as_str())))
                .map(|(k, v)| {
                    let v = match (k.as_str(), v) {
                        ("field", Value::String(s)) => Value::String(name(s).unwrap_or_else(|| s.clone())),
                        ("value", v) => v.clone(),
                        (_, v) => rename_fields(v, name),
                    };
                    (k.clone(), v)
                })
                .collect(),
        ),
        Value::Array(a) => Value::Array(a.iter().map(|v| rename_fields(v, name)).collect()),
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    struct Fields(Vec<Field>, HashMap<Uuid, Vec<&'static str>>);

    impl Scope for Fields {
        fn field(&self, name: &str) -> Result<&Field, String> {
            self.0.iter().find(|f| f.key == name).ok_or_else(|| format!("No field {name}"))
        }
        fn lookup_value(&self, list: Uuid, key: &str) -> bool {
            self.1.get(&list).is_some_and(|v| v.contains(&key))
        }
    }

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

    fn scope() -> Fields {
        let list = Uuid::new_v4();
        let mut env = field("environment", AttributeDataType::Lookup);
        env.lookup_list_id = Some(list);
        let mut tier = field("tier", AttributeDataType::Enum);
        tier.enum_values = Some(sqlx::types::Json(vec!["gold".into(), "silver".into()]));
        Fields(
            vec![
                env,
                tier,
                field("risk", AttributeDataType::Integer),
                field("owner", AttributeDataType::Reference),
                field("notes", AttributeDataType::Text),
                field("go_live", AttributeDataType::Date),
            ],
            [(list, vec!["prod", "dr", "test"])].into(),
        )
    }

    fn codes(v: Value) -> Vec<(String, String)> {
        parse(&v, "c", &scope()).unwrap_err().into_iter().map(|e| (e.field, e.code)).collect()
    }

    #[test]
    fn the_design_example_parses_and_round_trips() {
        let s = scope();
        let v = json!({ "all": [
            { "field": "environment", "op": "in", "value": ["prod", "dr"] },
            { "any": [ { "field": "owner", "op": "isSet" }, { "field": "risk", "op": "lte", "value": 2 } ] }
        ] });
        let c = parse(&v, "c", &s).unwrap();
        let mut attrs = Vec::new();
        c.attributes(&mut attrs);
        assert_eq!(attrs.len(), 3);
        let by_key = |id: Uuid| s.0.iter().find(|f| f.id == id).unwrap().key.clone();
        assert_eq!(c.to_json(&by_key), v);
        let stored = c.to_json(&|id| id.to_string());
        let back = rename_fields(&stored, &|s| s.parse().ok().map(by_key));
        assert_eq!(back, v);
    }

    #[test]
    fn operators_values_and_limits_are_checked() {
        assert_eq!(
            codes(json!({ "field": "nope", "op": "eq", "value": 1 })),
            [("c.field".into(), "unknown_attribute".into())]
        );
        assert_eq!(codes(json!({ "field": "risk", "op": "like", "value": 1 })), [("c.op".into(), "invalid_op".into())]);
        assert_eq!(codes(json!({ "field": "notes", "op": "gt", "value": "a" })), [("c.op".into(), "op_type".into())]);
        assert_eq!(
            codes(json!({ "field": "risk", "op": "contains", "value": "a" })),
            [("c.op".into(), "op_type".into())]
        );
        assert_eq!(codes(json!({ "field": "risk", "op": "eq", "value": "2" })), [("c.value".into(), "type".into())]);
        assert_eq!(
            codes(json!({ "field": "risk", "op": "isSet", "value": 1 })),
            [("c.value".into(), "unexpected".into())]
        );
        assert_eq!(codes(json!({ "field": "risk", "op": "eq" })), [("c.value".into(), "required".into())]);
        assert_eq!(
            codes(json!({ "field": "tier", "op": "eq", "value": "bronze" })),
            [("c.value".into(), "unknown_value".into())]
        );
        assert_eq!(
            codes(json!({ "field": "environment", "op": "notIn", "value": ["prod", "lab"] })),
            [("c.value[1]".into(), "unknown_value".into())]
        );
        assert_eq!(
            codes(json!({ "field": "environment", "op": "in", "value": [] })),
            [("c.value".into(), "type".into())]
        );
        assert_eq!(
            codes(json!({ "field": "go_live", "op": "gte", "value": "2026-13-01" })),
            [("c.value".into(), "type".into())]
        );
        assert_eq!(
            codes(json!({ "field": "risk", "op": "eq", "value": 1, "x": 1 })),
            [("c.x".into(), "unknown".into())]
        );
        assert_eq!(codes(json!({ "all": [] })), [("c.all".into(), "type".into())]);
        assert_eq!(
            codes(json!({ "all": [{ "field": "risk", "op": "isSet" }], "any": [] })),
            [("c".into(), "unknown".into())]
        );
        assert_eq!(codes(json!({ "related": {} })), [("c".into(), "unsupported".into())]);
        assert_eq!(codes(json!([1])), [("c".into(), "type".into())]);
        // Depth: the root group is level 1; a leaf at level 5 is too deep.
        let leaf = json!({ "field": "risk", "op": "isSet" });
        let deep = json!({ "all": [{ "any": [{ "all": [{ "any": [leaf.clone()] }] }] }] });
        assert_eq!(codes(deep), [("c.all[0].any[0].all[0].any[0]".into(), "too_deep".into())]);
        assert!(parse(&json!({ "all": [{ "any": [{ "all": [leaf.clone()] }] }] }), "c", &scope()).is_ok());
        // Leaves.
        let many = json!({ "all": vec![leaf.clone(); MAX_LEAVES + 1] });
        assert_eq!(codes(many), [("c".into(), "too_many_leaves".into())]);
        assert!(parse(&json!({ "all": vec![leaf; MAX_LEAVES] }), "c", &scope()).is_ok());
        // Size.
        let big = json!({ "field": "notes", "op": "in", "value": vec!["x".repeat(999); 20] });
        assert_eq!(codes(big), [("c".into(), "too_large".into())]);
    }
}
