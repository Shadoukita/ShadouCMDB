//! Request validation driven by the same JSON Schemas the OpenAPI document
//! publishes.
//!
//! Every request body and query string is checked against the utoipa schema of
//! its Rust type *before* it is deserialised: unknown keys, required fields,
//! types, string lengths, patterns, numeric bounds, enums and formats. All
//! problems are collected (not just the first) and reported as per-field
//! details in the error envelope, so the spec cannot promise something the
//! validator does not enforce. Rules that JSON Schema cannot express
//! (cross-field checks, database lookups) run afterwards in the services.

use std::any::TypeId;
use std::collections::HashMap;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::sync::{Arc, LazyLock, RwLock};

use regex::Regex;
use serde_json::{Map, Value};

use crate::http::error::{FieldError, FieldLocation};

/// Custom messages for the patterns the API uses, instead of echoing the regex.
pub fn pattern_message(pattern: &str) -> Option<&'static str> {
    Some(match pattern {
        super::schemas::KEY_PATTERN => "Must be lower_snake_case: a letter, then letters, digits or _ (max 63)",
        super::schemas::HOSTNAME_PATTERN => "Letters, digits, \".\", \"_\" and \"-\", starting with a letter or digit",
        super::schemas::NOT_BLANK_PATTERN => "Must not be blank",
        crate::data::items::SORT_PATTERN => {
            "One of label, ident, className, validFrom, validUntil, createdAt, updatedAt or attributes.<key>, \
             optionally prefixed with \"-\""
        }
        super::schemas::IDENT_PATTERN => {
            "Letters, digits, \".\", \"_\" and \"-\", starting with a letter or digit (max 64)"
        }
        super::schemas::USERNAME_PATTERN => {
            "Letters, digits, \".\", \"_\", \"@\" and \"-\", starting with a letter or digit (max 64)"
        }
        _ => return None,
    })
}

static REGEX_CACHE: LazyLock<RwLock<HashMap<String, Option<Arc<Regex>>>>> = LazyLock::new(Default::default);

/// Compiled and cached; `None` when the pattern is not a valid regular expression.
pub fn cached_regex(pattern: &str) -> Option<Arc<Regex>> {
    if let Some(hit) = REGEX_CACHE.read().ok().and_then(|m| m.get(pattern).cloned()) {
        return hit;
    }
    let compiled = Regex::new(pattern).ok().map(Arc::new);
    if let Ok(mut m) = REGEX_CACHE.write() {
        // Attribute patterns come from user data; keep the cache bounded.
        if m.len() > 1000 {
            m.clear();
        }
        m.insert(pattern.to_owned(), compiled.clone());
    }
    compiled
}

static SCHEMA_CACHE: LazyLock<RwLock<HashMap<TypeId, Arc<Value>>>> = LazyLock::new(Default::default);

/// The JSON form of a type's schema, built once per type.
pub fn cached_schema<T: 'static>(build: impl FnOnce() -> Value) -> Arc<Value> {
    let id = TypeId::of::<T>();
    if let Some(hit) = SCHEMA_CACHE.read().ok().and_then(|m| m.get(&id).cloned()) {
        return hit;
    }
    let schema = Arc::new(build());
    if let Ok(mut m) = SCHEMA_CACHE.write() {
        m.insert(id, schema.clone());
    }
    schema
}

// ---------------------------------------------------------------------------
// Formats
// ---------------------------------------------------------------------------

static UUID_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        "^([0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[1-8][0-9a-fA-F]{3}-[89abAB][0-9a-fA-F]{3}-[0-9a-fA-F]{12}|00000000-0000-0000-0000-000000000000|ffffffff-ffff-ffff-ffff-ffffffffffff)$",
    )
    .expect("uuid regex")
});
static DATETIME_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(\.\d+)?(Z|[+-]\d{2}:\d{2})$").expect("datetime regex")
});
static EMAIL_LOCAL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z0-9_'+\-.]*[A-Za-z0-9_+-]$").expect("email regex"));
static EMAIL_DOMAIN_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^([A-Za-z0-9][A-Za-z0-9\-]*\.)+[A-Za-z]{2,}$").expect("email regex"));

/// A uuid in the canonical, RFC 9562 form (same rule as the SHAA-3 API).
pub fn is_uuid(s: &str) -> bool {
    UUID_RE.is_match(s)
}

pub fn is_ip(s: &str) -> bool {
    s.parse::<Ipv4Addr>().is_ok() || s.parse::<Ipv6Addr>().is_ok()
}

fn is_cidr_v4(s: &str) -> bool {
    matches!(s.split_once('/'), Some((a, b)) if a.parse::<Ipv4Addr>().is_ok() && bits(b, 32))
}

fn is_cidr_v6(s: &str) -> bool {
    matches!(s.split_once('/'), Some((a, b)) if a.parse::<Ipv6Addr>().is_ok() && bits(b, 128))
}

fn bits(s: &str, max: u32) -> bool {
    (1..=3).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_digit()) && s.parse::<u32>().is_ok_and(|n| n <= max)
}

pub fn is_ip_or_cidr(s: &str) -> bool {
    is_ip(s) || is_cidr_v4(s) || is_cidr_v6(s)
}

pub fn is_date(s: &str) -> bool {
    s.len() == 10 && chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").is_ok()
}

/// ISO 8601 with seconds and a mandatory offset (Z or +hh:mm); fractions optional.
pub fn is_datetime(s: &str) -> bool {
    if !DATETIME_RE.is_match(s) {
        return false;
    }
    let date_ok = is_date(&s[..10]);
    let t = &s[11..];
    let (h, m) = (t[0..2].parse::<u32>().unwrap_or(99), t[3..5].parse::<u32>().unwrap_or(99));
    let sec_ok = t[6..8].parse::<u32>().is_ok_and(|s| s < 60);
    date_ok && h < 24 && m < 60 && sec_ok
}

fn is_email(s: &str) -> bool {
    let Some((local, domain)) = s.rsplit_once('@') else { return false };
    !local.starts_with('.') && !s.contains("..") && EMAIL_LOCAL_RE.is_match(local) && EMAIL_DOMAIN_RE.is_match(domain)
}

/// `None` when the value satisfies the format (unknown formats are not checked).
fn format_error(format: &str, s: &str) -> Option<&'static str> {
    let ok = match format {
        "uuid" => is_uuid(s),
        "email" => is_email(s),
        "ipv4" => s.parse::<Ipv4Addr>().is_ok(),
        "ipv6" => s.parse::<Ipv6Addr>().is_ok(),
        "cidrv4" => is_cidr_v4(s),
        "cidrv6" => is_cidr_v6(s),
        "date" => is_date(s),
        "date-time" => is_datetime(s),
        _ => true,
    };
    if ok {
        return None;
    }
    Some(match format {
        "uuid" => "Invalid UUID",
        "email" => "Invalid email address",
        "ipv4" => "Invalid IPv4 address",
        "ipv6" => "Invalid IPv6 address",
        "cidrv4" | "cidrv6" => "Invalid CIDR block",
        "date" => "Invalid ISO date",
        _ => "Invalid ISO datetime",
    })
}

/// Message for a failed union whose branches are all string formats.
fn union_message(formats: &[&str]) -> Option<&'static str> {
    if formats.iter().all(|f| *f == "ipv4" || *f == "ipv6") {
        Some("Must be an IPv4 or IPv6 address")
    } else if formats.iter().all(|f| *f == "cidrv4" || *f == "cidrv6") {
        Some("Must be a CIDR block, e.g. 10.0.0.0/24")
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Walker
// ---------------------------------------------------------------------------

fn type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn is_integer(n: &serde_json::Number) -> bool {
    n.is_i64() || n.is_u64() || n.as_f64().is_some_and(|f| f.fract() == 0.0 && f.is_finite())
}

fn type_matches(ty: &str, v: &Value) -> bool {
    match (ty, v) {
        ("integer", Value::Number(n)) => is_integer(n),
        (t, v) => t == type_name(v),
    }
}

fn num_str(n: f64) -> String {
    if n.fract() == 0.0 && n.abs() < 1e15 { format!("{}", n as i64) } else { n.to_string() }
}

struct Walker<'a> {
    location: FieldLocation,
    components: Option<&'a Map<String, Value>>,
    errors: Vec<FieldError>,
}

impl<'a> Walker<'a> {
    fn push(&mut self, path: &[String], message: impl Into<String>, code: &str) {
        let field = if path.is_empty() { "(root)".to_owned() } else { path.join(".") };
        self.errors.push(FieldError { location: self.location, field, message: message.into(), code: code.into() });
    }

    fn resolve<'s>(&self, schema: &'s Value) -> &'s Value
    where
        'a: 's,
        Self: 's,
    {
        if let Some(r) = schema.get("$ref").and_then(Value::as_str)
            && let Some(name) = r.strip_prefix("#/components/schemas/")
            && let Some(found) = self.components.and_then(|c| c.get(name))
        {
            return found;
        }
        schema
    }

    fn walk(&mut self, schema: &Value, value: &Value, path: &mut Vec<String>) {
        let schema = self.resolve(schema);

        if let Some(branches) = schema.get("anyOf").or_else(|| schema.get("oneOf")).and_then(Value::as_array) {
            self.union(branches, value, path);
            // Sibling keywords of a union (e.g. description) carry no constraints here.
            return;
        }
        if let Some(parts) = schema.get("allOf").and_then(Value::as_array) {
            for p in parts {
                self.walk(p, value, path);
            }
        }

        if let Some(ty) = schema.get("type") {
            let allowed: Vec<&str> = match ty {
                Value::String(s) => vec![s.as_str()],
                Value::Array(a) => a.iter().filter_map(Value::as_str).collect(),
                _ => vec![],
            };
            if !allowed.is_empty() && !allowed.iter().any(|t| type_matches(t, value)) {
                let expected: Vec<&str> = allowed.iter().copied().filter(|t| *t != "null").collect();
                let expected = if expected.is_empty() { "null".to_owned() } else { expected.join(" | ") };
                let received = match value {
                    Value::Number(n) if allowed.contains(&"integer") && !is_integer(n) => "number",
                    other => type_name(other),
                };
                let msg = if allowed.contains(&"integer") && value.is_number() {
                    "Invalid input: expected int, received number".to_owned()
                } else {
                    format!("Invalid input: expected {expected}, received {received}")
                };
                self.push(path, msg, "invalid_type");
                return;
            }
        }
        if value.is_null() {
            return;
        }

        if let Some(options) = schema.get("enum").and_then(Value::as_array)
            && !options.contains(value)
        {
            let list: Vec<String> = options.iter().map(|o| o.to_string()).collect();
            self.push(path, format!("Invalid option: expected one of {}", list.join("|")), "invalid_value");
            return;
        }

        match value {
            Value::String(s) => self.string(schema, s, path),
            Value::Number(n) => self.number(schema, n, path),
            Value::Array(items) => self.array(schema, items, path),
            Value::Object(obj) => self.object(schema, obj, path),
            _ => {}
        }
    }

    fn union(&mut self, branches: &[Value], value: &Value, path: &mut Vec<String>) {
        let mut failures: Vec<(Vec<FieldError>, &Value)> = Vec::new();
        for b in branches {
            let b = self.resolve(b);
            let mut sub = Walker { location: self.location, components: self.components, errors: Vec::new() };
            sub.walk(b, value, path);
            if sub.errors.is_empty() {
                return;
            }
            failures.push((sub.errors, b));
        }
        // Report the branch the value was meant for: ignore the `null` branch of a nullable.
        let real: Vec<_> = failures.into_iter().filter(|(_, b)| b.get("type") != Some(&Value::from("null"))).collect();
        if real.len() == 1 {
            let (errs, _) = real.into_iter().next().expect("one branch");
            self.errors.extend(errs);
            return;
        }
        let formats: Vec<&str> = real.iter().filter_map(|(_, b)| b.get("format").and_then(Value::as_str)).collect();
        if value.is_string()
            && formats.len() == real.len()
            && let Some(msg) = union_message(&formats)
        {
            self.push(path, msg, "invalid_format");
            return;
        }
        self.push(path, "Invalid input", "invalid_union");
    }

    fn string(&mut self, schema: &Value, s: &str, path: &[String]) {
        let len = s.chars().count() as u64;
        let pattern = schema.get("pattern").and_then(Value::as_str);
        // Trimmed, non-blank strings: one "Must not be blank" instead of a length and a pattern error.
        if pattern == Some(super::schemas::NOT_BLANK_PATTERN) && s.trim().is_empty() {
            self.push(path, "Must not be blank", "too_small");
            return;
        }
        // New passwords: the policy message ("Must be at least 12 characters") instead of the generic length text.
        if schema.get("format").and_then(Value::as_str) == Some("password") {
            if let Some(msg) = crate::auth::password::policy_error(s) {
                self.push(path, msg, "password_policy");
            }
            return;
        }
        if let Some(min) = schema.get("minLength").and_then(Value::as_u64)
            && len < min
        {
            self.push(path, format!("Too small: expected string to have >={min} characters"), "too_small");
        }
        if let Some(max) = schema.get("maxLength").and_then(Value::as_u64)
            && len > max
        {
            self.push(path, format!("Too big: expected string to have <={max} characters"), "too_big");
        }
        if let Some(format) = schema.get("format").and_then(Value::as_str)
            && let Some(msg) = format_error(format, s)
        {
            self.push(path, msg, "invalid_format");
        }
        if let Some(pattern) = pattern
            && let Some(re) = cached_regex(pattern)
            && !re.is_match(s)
        {
            let msg = pattern_message(pattern)
                .map(str::to_owned)
                .unwrap_or_else(|| format!("Invalid string: must match pattern /{pattern}/"));
            self.push(path, msg, "invalid_format");
        }
    }

    fn number(&mut self, schema: &Value, n: &serde_json::Number, path: &[String]) {
        let Some(v) = n.as_f64() else { return };
        if let Some(min) = schema.get("minimum").and_then(Value::as_f64)
            && v < min
        {
            self.push(path, format!("Too small: expected number to be >={}", num_str(min)), "too_small");
        }
        if let Some(max) = schema.get("maximum").and_then(Value::as_f64)
            && v > max
        {
            self.push(path, format!("Too big: expected number to be <={}", num_str(max)), "too_big");
        }
    }

    fn array(&mut self, schema: &Value, items: &[Value], path: &mut Vec<String>) {
        let n = items.len() as u64;
        if let Some(min) = schema.get("minItems").and_then(Value::as_u64)
            && n < min
        {
            self.push(path, format!("Too small: expected array to have >={min} items"), "too_small");
        }
        if let Some(max) = schema.get("maxItems").and_then(Value::as_u64)
            && n > max
        {
            self.push(path, format!("Too big: expected array to have <={max} items"), "too_big");
        }
        if let Some(item_schema) = schema.get("items") {
            for (i, item) in items.iter().enumerate() {
                path.push(i.to_string());
                self.walk(item_schema, item, path);
                path.pop();
            }
        }
        if schema.get("uniqueItems") == Some(&Value::Bool(true)) {
            let mut seen = std::collections::HashSet::new();
            if !items.iter().all(|i| seen.insert(i.to_string())) {
                self.push(path, "Values must be unique", "custom");
            }
        }
    }

    fn object(&mut self, schema: &Value, obj: &Map<String, Value>, path: &mut Vec<String>) {
        let props = schema.get("properties").and_then(Value::as_object);
        if let Some(required) = schema.get("required").and_then(Value::as_array) {
            for key in required.iter().filter_map(Value::as_str) {
                if !obj.contains_key(key) {
                    path.push(key.to_owned());
                    self.push(path, "Required", "required");
                    path.pop();
                }
            }
        }
        let additional = schema.get("additionalProperties");
        let mut unknown = Vec::new();
        for (key, v) in obj {
            if let Some(prop) = props.and_then(|p| p.get(key)) {
                path.push(key.clone());
                self.walk(prop, v, path);
                path.pop();
            } else {
                match additional {
                    Some(Value::Bool(false)) => unknown.push(format!("\"{key}\"")),
                    Some(extra @ Value::Object(_)) => {
                        path.push(key.clone());
                        self.walk(extra, v, path);
                        path.pop();
                    }
                    _ => {}
                }
            }
        }
        if !unknown.is_empty() {
            let noun = if unknown.len() == 1 { "key" } else { "keys" };
            self.push(path, format!("Unrecognized {noun}: {}", unknown.join(", ")), "unrecognized_keys");
        }
    }
}

/// Validate `value` against `schema`, returning every problem found.
pub fn check(
    schema: &Value,
    value: &Value,
    location: FieldLocation,
    components: Option<&Map<String, Value>>,
) -> Vec<FieldError> {
    let mut w = Walker { location, components, errors: Vec::new() };
    w.walk(schema, value, &mut Vec::new());
    w.errors
}

// ---------------------------------------------------------------------------
// Query strings
// ---------------------------------------------------------------------------

/// Query parameters as published: name, required and the parameter schema.
pub struct QueryParam {
    pub name: String,
    pub required: bool,
    pub schema: Value,
}

/// Parses a raw query string against its documented parameters into a JSON
/// object ready for deserialisation: repeated keys are joined with commas,
/// numeric parameters are coerced from text, defaults are applied, unknown
/// keys are rejected.
pub fn parse_query(raw: Option<&str>, params: &[QueryParam]) -> Result<Value, Vec<FieldError>> {
    let mut given: Vec<(String, String)> = Vec::new();
    for (k, v) in url::form_urlencoded::parse(raw.unwrap_or_default().as_bytes()) {
        match given.iter_mut().find(|(key, _)| *key == k) {
            Some((_, existing)) => {
                existing.push(',');
                existing.push_str(&v);
            }
            None => given.push((k.into_owned(), v.into_owned())),
        }
    }

    let mut errors = Vec::new();
    let mut out = Map::new();
    let mut unknown = Vec::new();
    for (key, raw_value) in &given {
        let Some(param) = params.iter().find(|p| p.name == *key) else {
            unknown.push(format!("\"{key}\""));
            continue;
        };
        let numeric = matches!(param.schema.get("type").and_then(Value::as_str), Some("integer" | "number"));
        let value = if numeric {
            // Like Number() in the SHAA-3 API: blank is 0, text is NaN.
            let text = raw_value.trim();
            match if text.is_empty() { Ok(0.0) } else { text.parse::<f64>() } {
                Ok(n) if !n.is_finite() => Value::Null,
                Ok(n) if n.fract() == 0.0 && n.abs() < 9e15 => Value::from(n as i64),
                Ok(n) => serde_json::Number::from_f64(n).map(Value::Number).unwrap_or(Value::Null),
                Err(_) => {
                    errors.push(FieldError {
                        location: FieldLocation::Query,
                        field: key.clone(),
                        message: "Invalid input: expected number, received NaN".into(),
                        code: "invalid_type".into(),
                    });
                    continue;
                }
            }
        } else {
            Value::String(raw_value.clone())
        };
        let mut problems = check(&param.schema, &value, FieldLocation::Query, None);
        for p in &mut problems {
            p.field = if p.field == "(root)" { key.clone() } else { format!("{key}.{}", p.field) };
        }
        errors.extend(problems);
        out.insert(key.clone(), value);
    }
    for p in params {
        if out.contains_key(&p.name) || given.iter().any(|(k, _)| *k == p.name) {
            continue;
        }
        if let Some(default) = p.schema.get("default") {
            out.insert(p.name.clone(), default.clone());
        } else if p.required {
            errors.push(FieldError {
                location: FieldLocation::Query,
                field: p.name.clone(),
                message: "Required".into(),
                code: "required".into(),
            });
        }
    }
    if !unknown.is_empty() {
        let noun = if unknown.len() == 1 { "key" } else { "keys" };
        errors.push(FieldError {
            location: FieldLocation::Query,
            field: "(root)".into(),
            message: format!("Unrecognized {noun}: {}", unknown.join(", ")),
            code: "unrecognized_keys".into(),
        });
    }
    if errors.is_empty() { Ok(Value::Object(out)) } else { Err(errors) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn codes(schema: Value, value: Value) -> Vec<(String, String)> {
        check(&schema, &value, FieldLocation::Body, None).into_iter().map(|e| (e.field, e.code)).collect()
    }

    #[test]
    fn object_rules() {
        let schema = json!({
            "type": "object",
            "properties": { "key": { "type": "string", "pattern": "^[a-z]+$" }, "n": { "type": ["integer", "null"], "minimum": 1 } },
            "required": ["key"],
            "additionalProperties": false
        });
        assert!(codes(schema.clone(), json!({"key": "ab", "n": null})).is_empty());
        assert_eq!(
            codes(schema.clone(), json!({"n": 0, "x": 1})),
            vec![
                ("key".into(), "required".into()),
                ("n".into(), "too_small".into()),
                ("(root)".into(), "unrecognized_keys".into())
            ]
        );
        assert_eq!(codes(schema, json!({"key": "A", "n": 1.5})).len(), 2);
    }

    #[test]
    fn unions_and_formats() {
        let ip = json!({"anyOf": [{"anyOf": [{"type": "string", "format": "ipv4"}, {"type": "string", "format": "ipv6"}]}, {"type": "null"}]});
        assert!(codes(ip.clone(), json!("10.0.0.1")).is_empty());
        assert!(codes(ip.clone(), json!("::1")).is_empty());
        assert!(codes(ip.clone(), Value::Null).is_empty());
        let errs = check(&ip, &json!("10.1.1.300"), FieldLocation::Body, None);
        assert_eq!(errs[0].message, "Must be an IPv4 or IPv6 address");
        assert!(is_uuid("00000000-0000-4000-8000-000000000000"));
        assert!(!is_uuid("00000000-0000-0000-8000-000000000000"));
        assert!(is_datetime("2026-01-02T03:04:00Z") && is_datetime("2026-01-02T03:04:05.123+02:00"));
        assert!(!is_datetime("2026-01-02T03:04Z"));
        assert!(!is_datetime("not-a-date") && !is_datetime("2026-02-30T00:00Z"));
        assert!(is_email("a.b@example.com") && !is_email("not-an-email") && !is_email(".a@x.io"));
    }

    #[test]
    fn password_policy_messages() {
        let schema = json!({"type": "string", "format": "password", "minLength": 12, "maxLength": 256});
        let msg = |v: &str| {
            check(&schema, &json!(v), FieldLocation::Body, None)
                .into_iter()
                .map(|e| (e.message, e.code))
                .collect::<Vec<_>>()
        };
        let policy = |m: &str| vec![(m.to_owned(), "password_policy".to_owned())];
        assert!(msg("correct horse battery").is_empty());
        assert_eq!(msg("short"), policy("Must be at least 12 characters"));
        // Counted in characters: 6 emoji are 12 UTF-16 units but 6 characters.
        assert_eq!(msg(&"🔑".repeat(6)), policy("Must be at least 12 characters"));
        assert_eq!(msg(&"x".repeat(257)), policy("Must be at most 256 characters"));
        assert_eq!(msg(&" ".repeat(14)), policy("Must not be only whitespace"));
    }

    #[test]
    fn query_coercion_and_defaults() {
        let params = vec![
            QueryParam {
                name: "limit".into(),
                required: false,
                schema: json!({"type": "integer", "minimum": 1, "maximum": 200, "default": 50}),
            },
            QueryParam { name: "q".into(), required: true, schema: json!({"type": "string", "minLength": 1}) },
        ];
        let v = parse_query(Some("q=a&q=b"), &params).unwrap();
        assert_eq!(v, json!({"q": "a,b", "limit": 50}));
        let e = parse_query(Some("limit=500&x=1"), &params).unwrap_err();
        let got: Vec<_> = e.iter().map(|e| (e.field.as_str(), e.code.as_str())).collect();
        assert_eq!(got, vec![("limit", "too_big"), ("q", "required"), ("(root)", "unrecognized_keys")]);
        assert!(parse_query(Some("limit=abc&q=x"), &params).is_err());
    }
}
