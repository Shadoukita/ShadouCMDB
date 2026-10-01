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
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock, Mutex, RwLock};

use regex::Regex;
use regex_automata::util::syntax;
use regex_automata::{Input, MatchKind, meta};
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

/// Upper bound on a compiled pattern. The regex crate's default is 10 MiB, so
/// 500 characters such as `\w{200}` used to cost about 10 MiB of memory each
/// (GH#412). 1 MiB still fits `\w{20}`, `.{0,500}` and every built-in template
/// pattern.
pub const PATTERN_SIZE_LIMIT: usize = 1 << 20;
/// Capacity of each lazy-DFA cache (forward and reverse) a match uses. When it
/// is full the DFA starts over, or the search falls back to a slower engine.
const MATCH_DFA_LIMIT: usize = 256 * 1024;
/// Memory of the compiled programs in [`REGEX_CACHE`], as measured by the regex crate.
const REGEX_CACHE_BUDGET: usize = 64 << 20;
const REGEX_CACHE_ENTRIES: usize = 1000;
/// Memory of the idle matching caches kept for reuse, across all patterns and
/// threads (GH#430). The regex crate keeps one cache per pattern for each
/// thread that ever matched it, so its own pool grows with the thread count.
const IDLE_MATCH_CACHE_BUDGET: usize = 32 << 20;
/// Idle matching caches kept per pattern.
const IDLE_MATCH_CACHES: usize = 4;

/// Why an attribute validation pattern is refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatternError {
    /// Not a valid regular expression.
    Syntax,
    /// Valid, but compiles to more than [`PATTERN_SIZE_LIMIT`].
    TooBig,
}

fn regex_config() -> meta::Config {
    // The same engine settings as `regex::Regex`, with our limits.
    meta::Config::new()
        .match_kind(MatchKind::LeftmostFirst)
        .utf8_empty(true)
        .nfa_size_limit(Some(PATTERN_SIZE_LIMIT))
        .hybrid_cache_capacity(MATCH_DFA_LIMIT)
}

fn compile_pattern(pattern: &str) -> Result<meta::Regex, PatternError> {
    meta::Builder::new()
        .configure(regex_config())
        .syntax(syntax::Config::new().utf8(true))
        .build(pattern)
        .map_err(|e| if e.size_limit().is_some() { PatternError::TooBig } else { PatternError::Syntax })
}

/// Checks a pattern a request wants to store, without caching it: a rejected
/// or rolled-back request must not leave a compiled program behind.
pub fn check_pattern(pattern: &str) -> Result<(), PatternError> {
    if REGEX_CACHE.read().is_ok_and(|c| matches!(c.entries.get(pattern), Some(Some(_)))) {
        return Ok(());
    }
    compile_pattern(pattern).map(drop)
}

/// Total size of the idle matching caches of every [`CachedRegex`].
static IDLE_MATCH_CACHE_BYTES: AtomicUsize = AtomicUsize::new(0);

/// A compiled validation pattern with its own bounded pool of matching caches,
/// so that the memory it holds does not grow with the number of threads.
pub struct CachedRegex {
    re: meta::Regex,
    idle: Mutex<Vec<(meta::Cache, usize)>>,
    last_used: AtomicU64,
}

impl CachedRegex {
    pub fn is_match(&self, haystack: &str) -> bool {
        let pooled = self.idle.lock().ok().and_then(|mut idle| idle.pop());
        let mut cache = match pooled {
            Some((cache, bytes)) => {
                IDLE_MATCH_CACHE_BYTES.fetch_sub(bytes, Ordering::Relaxed);
                cache
            }
            None => self.re.create_cache(),
        };
        let found = self.re.search_half_with(&mut cache, &Input::new(haystack).earliest(true)).is_some();
        self.keep_idle(cache);
        found
    }

    /// Keeps a cache for the next match if the pattern's pool and the global
    /// budget have room; otherwise it is freed.
    fn keep_idle(&self, cache: meta::Cache) {
        let bytes = cache.memory_usage() + size_of::<meta::Cache>();
        let Ok(mut idle) = self.idle.lock() else { return };
        if idle.len() >= IDLE_MATCH_CACHES {
            return;
        }
        let reserved = IDLE_MATCH_CACHE_BYTES.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |used| {
            (used + bytes <= IDLE_MATCH_CACHE_BUDGET).then_some(used + bytes)
        });
        if reserved.is_ok() {
            idle.push((cache, bytes));
        }
    }
}

impl Drop for CachedRegex {
    fn drop(&mut self) {
        let idle = self.idle.get_mut().map(|v| v.iter().map(|(_, b)| b).sum()).unwrap_or(0);
        IDLE_MATCH_CACHE_BYTES.fetch_sub(idle, Ordering::Relaxed);
    }
}

#[derive(Default)]
struct RegexCache {
    entries: HashMap<String, Option<Arc<CachedRegex>>>,
    /// Sum of the measured sizes of the cached programs.
    weight: usize,
}

impl RegexCache {
    fn weight_of(pattern: &str, compiled: Option<&Arc<CachedRegex>>) -> usize {
        pattern.len() + compiled.map_or(0, |c| c.re.memory_usage())
    }

    /// Evicts the least recently used entries until `incoming` more bytes fit.
    fn make_room(&mut self, incoming: usize) {
        if self.entries.len() < REGEX_CACHE_ENTRIES && self.weight + incoming <= REGEX_CACHE_BUDGET {
            return;
        }
        let mut by_age: Vec<(u64, String)> = self
            .entries
            .iter()
            .map(|(p, c)| (c.as_ref().map_or(0, |c| c.last_used.load(Ordering::Relaxed)), p.clone()))
            .collect();
        by_age.sort_unstable();
        for (_, pattern) in by_age {
            if self.entries.len() < REGEX_CACHE_ENTRIES && self.weight + incoming <= REGEX_CACHE_BUDGET {
                break;
            }
            if let Some(gone) = self.entries.remove(&pattern) {
                self.weight -= Self::weight_of(&pattern, gone.as_ref());
            }
        }
    }
}

static REGEX_CACHE: LazyLock<RwLock<RegexCache>> = LazyLock::new(Default::default);
static REGEX_CACHE_TICK: AtomicU64 = AtomicU64::new(1);

/// Compiled within [`PATTERN_SIZE_LIMIT`] and cached; `None` when the pattern is
/// not a valid regular expression or is too big. Use it for the API's own
/// patterns and for patterns already stored; validate new ones with
/// [`check_pattern`].
pub fn cached_regex(pattern: &str) -> Option<Arc<CachedRegex>> {
    let tick = REGEX_CACHE_TICK.fetch_add(1, Ordering::Relaxed);
    if let Some(hit) = REGEX_CACHE.read().ok().and_then(|c| c.entries.get(pattern).cloned()) {
        if let Some(re) = &hit {
            re.last_used.store(tick, Ordering::Relaxed);
        }
        return hit;
    }
    let compiled = compile_pattern(pattern);
    if matches!(compiled, Err(PatternError::TooBig)) {
        // Stored before the limit existed: the value is no longer checked against it.
        tracing::warn!(pattern, limit = PATTERN_SIZE_LIMIT, "validation pattern exceeds the size limit and is ignored");
    }
    let compiled =
        compiled.ok().map(|re| Arc::new(CachedRegex { re, idle: Mutex::default(), last_used: AtomicU64::new(tick) }));
    let weight = RegexCache::weight_of(pattern, compiled.as_ref());
    if let Ok(mut c) = REGEX_CACHE.write() {
        // Attribute patterns come from user data; keep the cache bounded in count and size.
        if !c.entries.contains_key(pattern) {
            c.make_room(weight);
            c.entries.insert(pattern.to_owned(), compiled.clone());
            c.weight += weight;
        }
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
    /// Inside an `x-multiline` field: line breaks, tabs and bidi controls are allowed.
    multiline: bool,
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
        // A multiline field, and everything below it, may hold line breaks,
        // tabs and bidi controls (GH#289).
        if schema.get("x-multiline") == Some(&Value::Bool(true)) && !self.multiline {
            self.multiline = true;
            self.walk(schema, value, path);
            self.multiline = false;
            return;
        }
        // Before any union, so every branch does not report it again (GH#289).
        if let Value::String(s) = value
            && let Some(message) = character_error(s, false)
        {
            self.push(path, message, "invalid_character");
            return;
        }
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
            let mut sub = Walker {
                location: self.location,
                components: self.components,
                errors: Vec::new(),
                multiline: self.multiline,
            };
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
        // Typed text outside a multiline field is one line. Search terms are
        // not stored, secrets (a `writeOnly` string, not a whole `writeOnly`
        // object) and new passwords are never shown, and untyped (free-form)
        // values are checked by their own rules, e.g. the attribute definition.
        let typed = schema
            .get("type")
            .is_some_and(|t| t == "string" || t.as_array().is_some_and(|a| a.contains(&"string".into())));
        if typed
            && !self.multiline
            && schema.get("format").and_then(Value::as_str) != Some("password")
            && schema.get("writeOnly") != Some(&Value::Bool(true))
            && self.location != FieldLocation::Query
            && let Some(message) = character_error(s, true)
        {
            self.push(path, message, "invalid_character");
            return;
        }
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

// ---------------------------------------------------------------------------
// Characters (GH#289)
// ---------------------------------------------------------------------------

/// Refused in every string and object key: C0 controls other than TAB, LF and
/// CR, DEL and the C1 controls. U+0000 cannot be stored by PostgreSQL; the
/// rest act as terminal escapes in logs and exports and have no place in
/// CMDB text. Refused, never stripped, so what is stored is what was sent.
pub fn refused_everywhere(c: char) -> bool {
    matches!(c, '\u{0}'..='\u{8}' | '\u{B}' | '\u{C}' | '\u{E}'..='\u{1F}' | '\u{7F}'..='\u{9F}')
}

/// Allowed only in multiline fields (schema extension `x-multiline`, or a
/// multiline text attribute): TAB, line breaks and the bidirectional
/// embedding, override and isolate controls (Trojan Source spoofing). The
/// implicit direction marks (U+200E, U+200F, U+061C) and zero-width joiners
/// stay allowed everywhere: right-to-left names and emoji need them.
fn multiline_only(c: char) -> Option<&'static str> {
    Some(match c {
        '\t' => "tab",
        '\n' | '\r' | '\u{2028}' | '\u{2029}' => "line break",
        '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' => "bidirectional control character",
        _ => return None,
    })
}

/// The problem with the first refused character in `s`, if any.
fn character_error(s: &str, single_line: bool) -> Option<String> {
    s.chars().find_map(|c| {
        if c == '\0' {
            Some("Must not contain the NUL character (U+0000)".to_owned())
        } else if refused_everywhere(c) {
            Some(format!("Must not contain the control character U+{:04X}", c as u32))
        } else if single_line {
            multiline_only(c)
                .map(|name| format!("Must not contain a {name} (U+{:04X}) in a single-line field", c as u32))
        } else {
            None
        }
    })
}

/// Every string and object key in `value` with a character refused
/// everywhere (see [`refused_everywhere`]), as `invalid_character`. Runs over
/// the whole body before the schema check, so it also covers free-form parts
/// no schema rule walks into (UI settings, attribute values). Which fields
/// may hold line breaks is up to the schema check.
pub fn character_errors(value: &Value, location: FieldLocation) -> Vec<FieldError> {
    fn scan(value: &Value, path: &mut Vec<String>, out: &mut Vec<FieldError>, location: FieldLocation) {
        let push = |out: &mut Vec<FieldError>, path: &[String], message: String| {
            let field = if path.is_empty() { "(root)".to_owned() } else { path.join(".") };
            out.push(FieldError { location, field, message, code: "invalid_character".into() });
        };
        match value {
            Value::String(s) => {
                if let Some(message) = character_error(s, false) {
                    push(out, path, message);
                }
            }
            Value::Array(items) => {
                for (i, item) in items.iter().enumerate() {
                    path.push(i.to_string());
                    scan(item, path, out, location);
                    path.pop();
                }
            }
            Value::Object(obj) => {
                for (key, v) in obj {
                    // The key itself is the offending field; show it without the character.
                    path.push(key.replace(refused_everywhere, "\u{FFFD}"));
                    if let Some(message) = character_error(key, false) {
                        push(out, path, message);
                    } else {
                        scan(v, path, out, location);
                    }
                    path.pop();
                }
            }
            _ => {}
        }
    }
    let mut out = Vec::new();
    scan(value, &mut Vec::new(), &mut out, location);
    out
}

/// Validate `value` against `schema`, returning every problem found.
pub fn check(
    schema: &Value,
    value: &Value,
    location: FieldLocation,
    components: Option<&Map<String, Value>>,
) -> Vec<FieldError> {
    let mut w = Walker { location, components, errors: Vec::new(), multiline: false };
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

/// Distinct query keys accepted beyond those a route declares; the excess is
/// reported as unrecognized up to this point, and refused outright past it.
const EXTRA_QUERY_KEYS: usize = 16;

/// Parses a raw query string against its documented parameters into a JSON
/// object ready for deserialisation: repeated keys are joined with commas,
/// numeric parameters are coerced from text, defaults are applied, unknown
/// keys are rejected.
pub fn parse_query(raw: Option<&str>, params: &[QueryParam]) -> Result<Value, Vec<FieldError>> {
    // GH#439: keys are merged through a hash index, and a query with far more
    // distinct keys than the route declares is refused before any of them is
    // checked, so a long run of junk keys costs linear time and one error.
    let max_keys = params.len() + EXTRA_QUERY_KEYS;
    let mut given: Vec<(String, String)> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    for (k, v) in url::form_urlencoded::parse(raw.unwrap_or_default().as_bytes()) {
        if let Some(&i) = index.get(k.as_ref()) {
            let existing = &mut given[i].1;
            existing.push(',');
            existing.push_str(&v);
            continue;
        }
        if given.len() == max_keys {
            return Err(vec![FieldError {
                location: FieldLocation::Query,
                field: "(root)".into(),
                message: format!("Too many query parameters: at most {max_keys} distinct keys are accepted"),
                code: "too_many_keys".into(),
            }]);
        }
        index.insert(k.clone().into_owned(), given.len());
        given.push((k.into_owned(), v.into_owned()));
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
        if out.contains_key(&p.name) || index.contains_key(&p.name) {
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

    fn cached(pattern: &str) -> bool {
        REGEX_CACHE.read().unwrap().entries.contains_key(pattern)
    }

    #[test]
    fn pattern_size_limit() {
        // GH#412: about 10 MiB each at the regex crate's default limit.
        assert_eq!(check_pattern(r"\w{200}"), Err(PatternError::TooBig));
        assert_eq!(check_pattern(r"\w{900}"), Err(PatternError::TooBig));
        assert!(cached_regex(r"\w{200}y").is_none());
        assert_eq!(check_pattern("(unclosed"), Err(PatternError::Syntax));
        for ok in [r"\w{20}", ".{0,500}", r"^[A-Za-z0-9]([A-Za-z0-9._-]{0,252})$", r"^\S+$"] {
            assert_eq!(check_pattern(ok), Ok(()), "{ok}");
            assert!(cached_regex(ok).is_some(), "{ok}");
        }
    }

    #[test]
    fn regex_limits_match_the_budget() {
        // GH#430: the documented worst case is computed from these.
        let config = regex_config();
        assert_eq!(config.get_nfa_size_limit(), Some(PATTERN_SIZE_LIMIT));
        assert_eq!(config.get_hybrid_cache_capacity(), MATCH_DFA_LIMIT);
        assert_eq!((PATTERN_SIZE_LIMIT, MATCH_DFA_LIMIT), (1 << 20, 256 << 10));
        assert_eq!((REGEX_CACHE_BUDGET, IDLE_MATCH_CACHE_BUDGET, IDLE_MATCH_CACHES), (64 << 20, 32 << 20, 4));
        // A match on a long non-ASCII value with the largest kind of pattern
        // that compiles stays within the documented 1 MiB per matching thread.
        let re = compile_pattern(r"\w{20}gh430").unwrap();
        let mut cache = re.create_cache();
        for value in ["ä".repeat(4000), "aé1".repeat(1400)] {
            re.search_half_with(&mut cache, &Input::new(&value).earliest(true));
        }
        assert!(cache.memory_usage() < 1 << 20, "{}", cache.memory_usage());
        let c = REGEX_CACHE.read().unwrap();
        assert_eq!(c.weight, c.entries.iter().map(|(p, re)| RegexCache::weight_of(p, re.as_ref())).sum::<usize>());
    }

    #[test]
    fn concurrent_matching_stays_within_the_cache_budget() {
        // GH#430: the regex crate kept a lazy-DFA cache per pattern and thread.
        let patterns: Vec<String> = (0..100).map(|i| format!(r"\wgh430z{i}")).collect();
        let value = "ä".repeat(4000);
        std::thread::scope(|s| {
            for _ in 0..16 {
                s.spawn(|| {
                    for p in &patterns {
                        let re = cached_regex(p).unwrap();
                        assert!(!re.is_match(&value));
                        assert!(re.is_match(&format!("{value}ä{}", &p[2..])));
                    }
                });
            }
        });
        assert!(IDLE_MATCH_CACHE_BYTES.load(Ordering::Relaxed) <= IDLE_MATCH_CACHE_BUDGET);
        let c = REGEX_CACHE.read().unwrap();
        assert!(c.weight <= REGEX_CACHE_BUDGET);
        let mut idle = 0;
        for re in c.entries.values().flatten() {
            let pool = re.idle.lock().unwrap();
            assert!(pool.len() <= IDLE_MATCH_CACHES);
            for (cache, bytes) in pool.iter() {
                assert!(cache.memory_usage() < *bytes);
                idle += bytes;
            }
        }
        assert!(idle <= IDLE_MATCH_CACHE_BUDGET, "{idle}");
    }

    #[test]
    fn ordinary_patterns_stay_cached() {
        // GH#430: these counted as 1 MiB each, so about 64 of them reset the cache.
        let patterns: Vec<String> = (0..100).map(|i| format!(r"^[\w.-]+@[\w.-]+\.gh430n{i}$")).collect();
        for round in 0..3 {
            for p in &patterns {
                assert!(cached_regex(p).is_some_and(|re| re.is_match("a.b@example.gh430n0") == p.ends_with("n0$")));
                if round > 0 {
                    assert!(cached(p), "{p} was evicted");
                }
            }
        }
    }

    #[test]
    fn least_recently_used_patterns_are_evicted_first() {
        let mut c = RegexCache::default();
        let entry = |p: &str, tick| {
            Some(Arc::new(CachedRegex {
                re: compile_pattern(p).unwrap(),
                idle: Mutex::default(),
                last_used: AtomicU64::new(tick),
            }))
        };
        for (p, tick) in [("gh430a", 3), ("gh430b", 1), ("gh430c", 2)] {
            let re = entry(p, tick);
            c.weight += RegexCache::weight_of(p, re.as_ref());
            c.entries.insert(p.into(), re);
        }
        c.make_room(REGEX_CACHE_BUDGET - c.weight + 1);
        assert_eq!(c.entries.len(), 2);
        assert!(!c.entries.contains_key("gh430b"));
        assert_eq!(c.weight, c.entries.iter().map(|(p, re)| RegexCache::weight_of(p, re.as_ref())).sum::<usize>());
    }

    #[test]
    fn checking_a_pattern_does_not_cache_it() {
        for p in [r"^gh412-fresh-[a-z]{3}$", r"\w{300}gh412", "(gh412"] {
            let _ = check_pattern(p);
            assert!(!cached(p), "{p}");
        }
        assert!(cached_regex(r"^gh412-fresh-[a-z]{3}$").is_some());
        assert!(cached(r"^gh412-fresh-[a-z]{3}$"));
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
        // GH#289: a NUL in a query string is refused, not sent to the database.
        let e = parse_query(Some("q=a%00b"), &params).unwrap_err();
        assert_eq!((e[0].field.as_str(), e[0].code.as_str()), ("q", "invalid_character"));
    }

    /// GH#439: distinct keys are capped at the declared ones plus a margin.
    #[test]
    fn query_key_cap() {
        let params = vec![QueryParam { name: "q".into(), required: false, schema: json!({"type": "string"}) }];
        let keys = |n: usize| (0..n).map(|i| format!("k{i}")).collect::<Vec<_>>().join("&");
        // At the cap, extra keys are still listed as unrecognized.
        let e = parse_query(Some(&keys(1 + EXTRA_QUERY_KEYS)), &params).unwrap_err();
        assert_eq!(e[0].code, "unrecognized_keys");
        // One more distinct key is refused as a whole, with a single error.
        let e = parse_query(Some(&keys(2 + EXTRA_QUERY_KEYS)), &params).unwrap_err();
        assert_eq!(e.len(), 1);
        assert_eq!((e[0].field.as_str(), e[0].code.as_str()), ("(root)", "too_many_keys"));
        // Repeating a declared key does not count against the cap.
        let repeated = vec!["q=a"; 1000].join("&");
        assert_eq!(parse_query(Some(&repeated), &params).unwrap()["q"].as_str().unwrap().len(), 1999);
        // 20k distinct keys are refused quickly rather than merged quadratically.
        let started = std::time::Instant::now();
        let e = parse_query(Some(&keys(20_000)), &params).unwrap_err();
        assert_eq!(e[0].code, "too_many_keys");
        assert!(started.elapsed() < std::time::Duration::from_millis(100));
    }

    /// GH#289: U+0000 is refused wherever it appears, with one error per value.
    #[test]
    fn nul_characters() {
        let text = json!({"type": "string", "maxLength": 100, "x-multiline": true});
        assert_eq!(codes(text.clone(), json!("abc\u{0}def")), vec![("(root)".into(), "invalid_character".into())]);
        assert!(codes(text, json!("tab\tand\nnewline")).is_empty());
        // Unions report it once, not once per branch.
        let ip = json!({"anyOf": [{"type": "string", "format": "ipv4"}, {"type": "string", "format": "ipv6"}]});
        assert_eq!(codes(ip, json!("10.0.0.1\u{0}")), vec![("(root)".into(), "invalid_character".into())]);

        let body = json!({"name": "ok", "attributes": {"note": "a\u{0}", "tags": ["x", "\u{0}"]}, "k\u{0}": 1});
        let got: Vec<_> = character_errors(&body, FieldLocation::Body).into_iter().map(|e| (e.field, e.code)).collect();
        assert_eq!(
            got,
            vec![
                ("attributes.note".to_owned(), "invalid_character".to_owned()),
                ("attributes.tags.1".to_owned(), "invalid_character".to_owned()),
                ("k\u{FFFD}".to_owned(), "invalid_character".to_owned()),
            ]
        );
        assert!(character_errors(&json!({"a": ["b", 1, null, {"c": "d"}]}), FieldLocation::Body).is_empty());
    }

    /// GH#289 policy: C0 (but TAB, LF, CR), DEL and C1 are refused everywhere;
    /// TAB, line breaks and bidi controls only outside multiline fields.
    #[test]
    fn control_characters() {
        let bad = |schema: &Value, v: &str| {
            codes(schema.clone(), json!(v)) == [("(root)".into(), "invalid_character".into())]
        };
        let single = json!({"type": "string", "maxLength": 100});
        let nullable = json!({"anyOf": [{"type": "string"}, {"type": "null"}]});
        let multi = json!({"anyOf": [{"type": "string"}, {"type": "null"}], "x-multiline": true});
        let write_only = json!({"type": "string", "writeOnly": true});
        let password = json!({"type": "string", "format": "password"});

        for s in ["a\u{1}b", "\u{8}", "\u{B}", "\u{C}", "\u{1B}[31m", "\u{1F}", "\u{7F}", "\u{85}", "\u{9B}", "\u{9F}"]
        {
            for schema in [&single, &multi, &write_only, &json!({})] {
                assert!(bad(schema, s), "{s:?} in {schema}");
            }
        }
        for s in
            ["a\tb", "a\nb", "a\rb", "a\u{2028}b", "a\u{2029}b", "\u{202A}", "\u{202E}evil", "\u{2066}", "\u{2069}"]
        {
            assert!(bad(&single, s), "{s:?}");
            assert!(bad(&nullable, s), "{s:?}");
            assert!(codes(multi.clone(), json!(s)).is_empty(), "{s:?}");
            assert!(codes(write_only.clone(), json!(s)).is_empty(), "{s:?}");
            // Free-form values (e.g. CI attributes at the body level) are checked by their own rules.
            assert!(codes(json!({}), json!(s)).is_empty(), "{s:?}");
        }
        assert!(codes(password, json!("correct\thorse battery")).is_empty());
        // Right-to-left text, direction marks, joiners and emoji are fine everywhere.
        for s in ["שלום עולם", "abc\u{200F}", "\u{200E}x\u{061C}", "a\u{200B}b\u{200C}c", "👩\u{200D}💻", "Größe"]
        {
            assert!(codes(single.clone(), json!(s)).is_empty(), "{s:?}");
        }
        // The message names the character.
        let e = check(&single, &json!("a\u{202E}b"), FieldLocation::Body, None);
        assert_eq!(e[0].message, "Must not contain a bidirectional control character (U+202E) in a single-line field");
        let e = check(&single, &json!("\u{1B}"), FieldLocation::Body, None);
        assert_eq!(e[0].message, "Must not contain the control character U+001B");
        // Nested: the marker covers what is below it.
        let obj = json!({"type": "object", "properties": {
            "name": {"type": "string"},
            "notes": {"anyOf": [{"type": "string"}, {"type": "null"}], "x-multiline": true}}});
        let got = codes(obj, json!({"name": "a\nb", "notes": "a\nb"}));
        assert_eq!(got, vec![("name".to_owned(), "invalid_character".to_owned())]);

        // `writeOnly` exempts only the secret string itself, not the text inside
        // a write-only object such as the deprecated v1 layout `panels` (SHAA-765).
        let components = json!({"Panel": {"type": "object", "properties": {"label": {"type": "string"}}}});
        let components = components.as_object();
        let panels = json!({"type": "object", "properties": {
            "panels": {"type": "array", "items": {"$ref": "#/components/schemas/Panel", "writeOnly": true}},
            "legacy": {"type": "object", "writeOnly": true, "properties": {"label": {"type": "string"}}},
            "password": {"type": "string", "writeOnly": true},
            "clientSecret": {"anyOf": [{"type": "string", "writeOnly": true}, {"type": "null"}]}}});
        for label in ["Ops\nTeam", "\u{202E}evil", "a\tb"] {
            let body = json!({"panels": [{"label": label}], "legacy": {"label": label}});
            let got: Vec<_> =
                check(&panels, &body, FieldLocation::Body, components).into_iter().map(|e| (e.field, e.code)).collect();
            let want = |f: &str| (f.to_owned(), "invalid_character".to_owned());
            assert_eq!(got, vec![want("panels.0.label"), want("legacy.label")], "{label:?}");
        }
        let secrets = json!({"password": "a\tb\u{202E}", "clientSecret": "line\nbreak"});
        assert!(check(&panels, &secrets, FieldLocation::Body, components).is_empty());

        // Search terms are not stored: only the characters refused everywhere.
        let params = vec![QueryParam { name: "q".into(), required: false, schema: single.clone() }];
        assert!(parse_query(Some("q=a%09b%0Ac%E2%80%AE"), &params).is_ok());
        let e = parse_query(Some("q=a%1Bb"), &params).unwrap_err();
        assert_eq!((e[0].field.as_str(), e[0].code.as_str()), ("q", "invalid_character"));

        // The body scan: keys and values, everywhere-refused characters only.
        let body = json!({"a": "x\ny\u{202E}", "b\u{1B}": 1, "c": ["\u{7F}"]});
        let got: Vec<_> = character_errors(&body, FieldLocation::Body).into_iter().map(|e| (e.field, e.code)).collect();
        assert_eq!(
            got,
            vec![
                ("b\u{FFFD}".to_owned(), "invalid_character".to_owned()),
                ("c.0".to_owned(), "invalid_character".to_owned())
            ]
        );
    }
}
