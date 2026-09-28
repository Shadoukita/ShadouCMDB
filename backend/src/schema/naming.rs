//! Technical names: the PostgreSQL identifiers of areas (schemas), types
//! (tables) and fields (columns).
//!
//! A technical name is derived from the display name ("Virtuelle Maschinen" ->
//! `virtuelle_maschinen`, "Größe" -> `groesse`), shown for review, editable when
//! the object is created and immutable afterwards. It must match
//! `^[a-z][a-z0-9_]{0,62}$`, so it never needs quoting in a report and can
//! never carry SQL; the DDL engine still quotes every identifier it emits
//! ([`quote_ident`]). Reserved words and the names the system uses itself are
//! rejected with the reason.

use std::fmt;

/// What a technical name is for; each has its own reserved names and length.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum NameKind {
    /// An area: a PostgreSQL schema
    Area,
    /// A type (CI class): a table in its area's schema
    Type,
    /// A field (attribute): a column of its type's table
    Field,
}

impl NameKind {
    pub fn as_str(self) -> &'static str {
        match self {
            NameKind::Area => "area",
            NameKind::Type => "type",
            NameKind::Field => "field",
        }
    }

    /// Longest allowed name: PostgreSQL's 63, minus the "v_" of a type's reporting view.
    pub fn max_len(self) -> usize {
        match self {
            NameKind::Type => 61,
            _ => 63,
        }
    }

    /// Prefix for a name whose display name starts with a digit ("2024 Assets" -> type_2024_assets).
    fn digit_prefix(self) -> &'static str {
        match self {
            NameKind::Area => "area_",
            NameKind::Type => "type_",
            NameKind::Field => "field_",
        }
    }
}

/// Keywords PostgreSQL reserves (fully, or except as function/type names): as
/// identifiers they would need quoting in every report query.
pub const RESERVED_WORDS: &[&str] = &[
    "all",
    "analyse",
    "analyze",
    "and",
    "any",
    "array",
    "as",
    "asc",
    "asymmetric",
    "authorization",
    "binary",
    "both",
    "case",
    "cast",
    "check",
    "collate",
    "collation",
    "column",
    "concurrently",
    "constraint",
    "create",
    "cross",
    "current_catalog",
    "current_date",
    "current_role",
    "current_schema",
    "current_time",
    "current_timestamp",
    "current_user",
    "default",
    "deferrable",
    "desc",
    "distinct",
    "do",
    "else",
    "end",
    "except",
    "false",
    "fetch",
    "for",
    "foreign",
    "freeze",
    "from",
    "full",
    "grant",
    "group",
    "having",
    "ilike",
    "in",
    "initially",
    "inner",
    "intersect",
    "into",
    "is",
    "isnull",
    "join",
    "lateral",
    "leading",
    "left",
    "like",
    "limit",
    "localtime",
    "localtimestamp",
    "natural",
    "not",
    "notnull",
    "null",
    "offset",
    "on",
    "only",
    "or",
    "order",
    "outer",
    "overlaps",
    "placing",
    "primary",
    "references",
    "returning",
    "right",
    "select",
    "session_user",
    "similar",
    "some",
    "symmetric",
    "system_user",
    "table",
    "tablesample",
    "then",
    "to",
    "trailing",
    "true",
    "union",
    "unique",
    "user",
    "using",
    "variadic",
    "verbose",
    "when",
    "where",
    "window",
    "with",
];

/// Schemas the system uses (areas can never take them).
pub const RESERVED_SCHEMAS: &[&str] = &["cmdb", "public", "information_schema", "drizzle"];

/// Columns of every reporting view that come from the registry; `id` is also
/// the primary key of every type table. A field cannot take these names.
pub const REGISTRY_VIEW_COLUMNS: &[&str] = &[
    "id",
    "ident",
    "label",
    "type",
    "valid_from",
    "valid_until",
    "active",
    "record_version",
    "created_at",
    "updated_at",
    "deleted_at",
];

/// Why a technical name cannot be used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NameProblem {
    /// Machine-readable: empty, invalid_format, too_long, reserved_word, reserved_name, reserved_prefix
    pub code: &'static str,
    pub message: String,
}

impl fmt::Display for NameProblem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

fn transliterate(c: char, out: &mut String) {
    let s = match c {
        'ä' => "ae",
        'ö' => "oe",
        'ü' => "ue",
        'ß' => "ss",
        'æ' => "ae",
        'œ' => "oe",
        'ø' => "o",
        'å' | 'à' | 'á' | 'â' | 'ã' | 'ā' | 'ą' => "a",
        'ç' | 'ć' | 'č' => "c",
        'ď' | 'đ' => "d",
        'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ę' | 'ě' => "e",
        'ì' | 'í' | 'î' | 'ï' | 'ī' => "i",
        'ł' | 'ľ' => "l",
        'ñ' | 'ń' | 'ň' => "n",
        'ò' | 'ó' | 'ô' | 'õ' | 'ō' | 'ő' => "o",
        'ř' => "r",
        'ś' | 'š' | 'ş' => "s",
        'ť' | 'ţ' => "t",
        'ù' | 'ú' | 'û' | 'ū' | 'ů' | 'ű' => "u",
        'ý' | 'ÿ' => "y",
        'ź' | 'ż' | 'ž' => "z",
        c if c.is_ascii_lowercase() || c.is_ascii_digit() => {
            out.push(c);
            return;
        }
        _ => "_",
    };
    out.push_str(s);
}

/// The technical name suggested for a display name. The result may still be
/// unusable (a reserved word, or empty for a name without letters or digits):
/// check it with [`validate`].
pub fn derive(display_name: &str, kind: NameKind) -> String {
    let mut raw = String::with_capacity(display_name.len() + 8);
    for c in display_name.trim().chars().flat_map(char::to_lowercase) {
        transliterate(c, &mut raw);
    }
    // Collapse runs of "_" and trim them at both ends.
    let mut name = String::with_capacity(raw.len());
    for c in raw.chars() {
        if c == '_' && (name.is_empty() || name.ends_with('_')) {
            continue;
        }
        name.push(c);
    }
    while name.ends_with('_') {
        name.pop();
    }
    if name.starts_with(|c: char| c.is_ascii_digit()) {
        name.insert_str(0, kind.digit_prefix());
    }
    if name.len() > kind.max_len() {
        name.truncate(kind.max_len());
        while name.ends_with('_') {
            name.pop();
        }
    }
    name
}

/// Checks a technical name for `kind`; the error says why it cannot be used.
pub fn validate(name: &str, kind: NameKind) -> Result<(), NameProblem> {
    let problem = |code, message: String| Err(NameProblem { code, message });
    if name.is_empty() {
        return problem(
            "empty",
            "Enter a technical name (the display name has no letters or digits to derive one from)".into(),
        );
    }
    if name.len() > kind.max_len() {
        return problem(
            "too_long",
            format!(
                "At most {} characters{} ({} given)",
                kind.max_len(),
                if kind == NameKind::Type { ": the reporting view adds \"v_\" to PostgreSQL's 63" } else { "" },
                name.len()
            ),
        );
    }
    let bytes = name.as_bytes();
    let well_formed = bytes[0].is_ascii_lowercase()
        && bytes.iter().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'_');
    if !well_formed {
        return problem(
            "invalid_format",
            "Use lower-case letters a-z, digits and \"_\", starting with a letter".into(),
        );
    }
    if RESERVED_WORDS.contains(&name) {
        return problem("reserved_word", format!("\"{name}\" is a reserved SQL keyword"));
    }
    if name.starts_with("pg_") {
        return problem("reserved_prefix", "Names starting with \"pg_\" are reserved by PostgreSQL".into());
    }
    match kind {
        NameKind::Area => {
            if RESERVED_SCHEMAS.contains(&name) {
                return problem("reserved_name", format!("\"{name}\" is a schema the system uses"));
            }
            if name.starts_with("cmdb_") {
                return problem("reserved_prefix", "Names starting with \"cmdb_\" are reserved for the system".into());
            }
            // The database roles (shadoucmdb_owner, _app, _maintenance): a schema
            // named after a role is first on that role's default search_path.
            if name.starts_with("shadoucmdb_") {
                return problem(
                    "reserved_prefix",
                    "Names starting with \"shadoucmdb_\" are reserved for the database roles".into(),
                );
            }
        }
        NameKind::Type => {
            if name.starts_with("v_") {
                return problem(
                    "reserved_prefix",
                    "Names starting with \"v_\" are reserved for reporting views".into(),
                );
            }
        }
        NameKind::Field => {
            if REGISTRY_VIEW_COLUMNS.contains(&name) {
                return problem(
                    "reserved_name",
                    format!("\"{name}\" is a column every asset already has (in the registry and the reporting views)"),
                );
            }
        }
    }
    Ok(())
}

/// A validated technical name, safe to put into SQL (quoted).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Ident(String);

impl Ident {
    /// Only for names read back from the metadata tables, whose CHECK
    /// constraints enforce the same format. Panics on anything else, so a
    /// malformed name can never reach a statement.
    pub fn trusted(name: &str) -> Ident {
        assert!(is_identifier(name), "not a technical name: {name:?}");
        Ident(name.to_owned())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Ident {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&quote_ident(&self.0))
    }
}

/// `^[a-z][a-z0-9_]{0,62}$`
pub fn is_identifier(name: &str) -> bool {
    let b = name.as_bytes();
    !b.is_empty()
        && b.len() <= 63
        && b[0].is_ascii_lowercase()
        && b.iter().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'_')
}

/// PostgreSQL's quote_ident: double quotes around the name, embedded quotes doubled.
pub fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derives_from_display_names() {
        assert_eq!(derive("Bestand", NameKind::Area), "bestand");
        assert_eq!(derive("Netzwerk", NameKind::Type), "netzwerk");
        assert_eq!(derive("Virtuelle Maschinen", NameKind::Type), "virtuelle_maschinen");
        assert_eq!(derive("Größe", NameKind::Field), "groesse");
        assert_eq!(derive("ÄÖÜ äöü ß", NameKind::Field), "aeoeue_aeoeue_ss");
        assert_eq!(derive("  IP-Adresse (v4)  ", NameKind::Field), "ip_adresse_v4");
        assert_eq!(derive("Café Crème", NameKind::Field), "cafe_creme");
        assert_eq!(derive("2024 Assets", NameKind::Type), "type_2024_assets");
        assert_eq!(derive("___", NameKind::Area), "");
        assert_eq!(derive("日本", NameKind::Area), "");
        let long = derive(&"x".repeat(100), NameKind::Type);
        assert_eq!(long.len(), 61);
        assert_eq!(derive(&"y".repeat(100), NameKind::Field).len(), 63);
        // A cut that ends in "_" loses it.
        let cut = derive(&format!("{} b", "a".repeat(62)), NameKind::Field);
        assert_eq!(cut, "a".repeat(62));
    }

    #[test]
    fn accepts_ordinary_names() {
        for (n, k) in [
            ("bestand", NameKind::Area),
            ("virtuelle_maschinen", NameKind::Type),
            ("groesse", NameKind::Field),
            ("cpu_cores", NameKind::Field),
            ("version", NameKind::Field),
            ("server", NameKind::Type),
        ] {
            assert_eq!(validate(n, k), Ok(()), "{n}");
        }
    }

    #[test]
    fn rejects_injection_attempts_and_reserved_names() {
        let code = |n: &str, k| validate(n, k).unwrap_err().code;
        // Quotes, semicolons, whitespace, comments and upper case never pass the format check.
        for n in [
            "bestand\"; DROP SCHEMA cmdb CASCADE; --",
            "x'; DELETE FROM cmdb.users; --",
            "a;b",
            "a b",
            "a--b",
            "a/*b*/",
            "Bestand",
            "\"quoted\"",
            "a.b",
            "_leading",
            "9lives",
            "ä",
        ] {
            assert_eq!(code(n, NameKind::Area), "invalid_format", "{n}");
            assert_eq!(code(n, NameKind::Field), "invalid_format", "{n}");
        }
        assert_eq!(code("", NameKind::Type), "empty");
        assert_eq!(code(&"a".repeat(64), NameKind::Field), "too_long");
        assert_eq!(code(&"a".repeat(62), NameKind::Type), "too_long");
        assert_eq!(validate(&"a".repeat(63), NameKind::Field), Ok(()));
        assert_eq!(validate(&"a".repeat(61), NameKind::Type), Ok(()));
        for k in [NameKind::Area, NameKind::Type, NameKind::Field] {
            assert_eq!(code("pg_catalog", k), "reserved_prefix");
            assert_eq!(code("pg_x", k), "reserved_prefix");
            assert_eq!(code("select", k), "reserved_word");
            assert_eq!(code("table", k), "reserved_word");
        }
        for n in ["cmdb", "public", "information_schema"] {
            assert_eq!(code(n, NameKind::Area), "reserved_name", "{n}");
        }
        assert_eq!(code("cmdb_reporting", NameKind::Area), "reserved_prefix");
        for n in ["shadoucmdb_owner", "shadoucmdb_app", "shadoucmdb_maintenance", "shadoucmdb_x"] {
            assert_eq!(code(n, NameKind::Area), "reserved_prefix", "{n}");
        }
        assert_eq!(validate("shadoucmdb", NameKind::Area), Ok(()));
        assert_eq!(code("v_netzwerk", NameKind::Type), "reserved_prefix");
        assert_eq!(code("id", NameKind::Field), "reserved_name");
        assert_eq!(code("ident", NameKind::Field), "reserved_name");
        assert_eq!(code("label", NameKind::Field), "reserved_name");
        assert_eq!(code("valid_until", NameKind::Field), "reserved_name");
        // Former registry columns are ordinary fields since SHAA-267.
        assert_eq!(validate("name", NameKind::Field), Ok(()));
        assert_eq!(validate("hostname", NameKind::Field), Ok(()));
        assert_eq!(code("deleted_at", NameKind::Field), "reserved_name");
    }

    #[test]
    fn quoting() {
        assert_eq!(quote_ident("bestand"), "\"bestand\"");
        assert_eq!(quote_ident("a\"b"), "\"a\"\"b\"");
        assert_eq!(Ident::trusted("netzwerk").to_string(), "\"netzwerk\"");
        assert!(std::panic::catch_unwind(|| Ident::trusted("x\"; drop")).is_err());
    }
}
