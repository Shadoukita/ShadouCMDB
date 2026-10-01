//! The one CSV writer for files people open in a spreadsheet program: the
//! import error report, the import template, the impact analysis export, the
//! business-service member export, and later the inventory export (SHAA-714
//! §5.1, requirement I8). No module writes CSV cells of its own (GH#388).
//!
//! - Every field is quoted (RFC 4180), an embedded `"` doubled.
//! - A field a spreadsheet could take for a formula gets a leading `'`: its
//!   first character is tab, CR or LF, or, after leading spaces, one of
//!   `=` `+` `-` `@` or their full-width forms `＝` `＋` `－` `＠`.
//! - A field that already starts with `'` gets another one, so that
//!   [`read_field`] is the exact inverse of [`write_field`] (CR5a).
//!
//! Files start with a UTF-8 byte order mark (so Excel detects the encoding)
//! and lines end with CRLF.

/// The UTF-8 byte order mark every file starts with.
pub const BOM: &str = "\u{feff}";
pub const LINE_END: &str = "\r\n";

const TRIGGERS: [char; 8] = ['=', '+', '-', '@', '＝', '＋', '－', '＠'];
const CONTROL_TRIGGERS: [char; 3] = ['\t', '\r', '\n'];

fn is_space(c: char) -> bool {
    matches!(c, ' ' | '\u{a0}' | '\u{3000}')
}

/// Whether a spreadsheet program could read the field as a formula.
pub fn is_dangerous(field: &str) -> bool {
    match field.chars().next() {
        Some(c) if CONTROL_TRIGGERS.contains(&c) => true,
        _ => field
            .trim_start_matches(is_space)
            .chars()
            .next()
            .is_some_and(|c| TRIGGERS.contains(&c) || CONTROL_TRIGGERS.contains(&c)),
    }
}

/// The field as it is written: neutralised, not yet quoted.
pub fn neutralise(field: &str) -> String {
    if field.starts_with('\'') || is_dangerous(field) { format!("'{field}") } else { field.to_owned() }
}

/// Appends one quoted, neutralised field.
pub fn write_field(out: &mut String, field: &str) {
    out.push('"');
    out.push_str(&neutralise(field).replace('"', "\"\""));
    out.push('"');
}

/// Appends a record: the fields separated by `delimiter`, then CRLF.
pub fn write_record<'a>(out: &mut String, delimiter: char, fields: impl IntoIterator<Item = &'a str>) {
    for (i, f) in fields.into_iter().enumerate() {
        if i > 0 {
            out.push(delimiter);
        }
        write_field(out, f);
    }
    out.push_str(LINE_END);
}

/// Undoes [`neutralise`] for a field read back from an error report: one
/// leading `'` goes when what follows it (after spaces) is `'` or a character
/// the writer neutralises. `'quoted` keeps its quote.
pub fn read_field(field: &str) -> &str {
    match field.strip_prefix('\'') {
        Some(rest) if rest.starts_with('\'') || is_dangerous(rest) => rest,
        _ => field,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DANGEROUS: [&str; 12] = [
        "=1+1",
        "@SUM(A1)",
        "+cmd|' /C calc'!A0",
        "-2+3",
        "\t=1",
        " =1",
        "＝1",
        "＋1",
        "－1",
        "＠SUM(A1)",
        "\r=1",
        "\n=1",
    ];
    const HARMLESS: [&str; 6] = ["a=b", "x-y", "web01", "", "1.5", "Müller"];

    #[test]
    fn dangerous_prefixes_are_neutralised() {
        for d in DANGEROUS {
            assert!(is_dangerous(d), "{d:?}");
            assert_eq!(neutralise(d), format!("'{d}"), "{d:?}");
        }
        for h in HARMLESS {
            assert!(!is_dangerous(h), "{h:?}");
            assert_eq!(neutralise(h), h);
        }
        // Leading spaces, including the non-breaking and ideographic ones.
        assert!(is_dangerous("\u{a0}=1") && is_dangerous("\u{3000}@x") && is_dangerous("   -1"));
    }

    #[test]
    fn every_field_is_quoted() {
        let mut out = String::new();
        write_record(&mut out, ';', ["a", "say \"hi\"", "=1", "x;y"]);
        assert_eq!(out, "\"a\";\"say \"\"hi\"\"\";\"'=1\";\"x;y\"\r\n");
    }

    #[test]
    fn write_then_read_is_the_identity() {
        let extra = ["'abc", "''", "'=1", "' =1", "'", "'-"];
        for field in DANGEROUS.iter().chain(HARMLESS.iter()).chain(extra.iter()) {
            assert_eq!(read_field(&neutralise(field)), *field, "{field:?}");
        }
        // A quote the writer did not add stays.
        assert_eq!(read_field("'quoted"), "'quoted");
    }
}
