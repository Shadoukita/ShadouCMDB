//! From a cell to the JSON value the CI API expects (SHAA-714 §2.4). The
//! value is then validated by the same code as `POST`/`PATCH
//! /configuration-items` (D10); conversion only reads the cell. A cell that
//! cannot be read as its type is an error with the API's codes
//! (`invalid_type`, `invalid_format`).
//!
//! Date-times without an offset need a time zone; they come out as
//! [`Converted::Local`] and the planner turns them into instants in the
//! database (`AT TIME ZONE`), which knows every IANA zone.

use chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime};
use serde_json::{Value, json};
use unicode_normalization::UnicodeNormalization;

use super::parse::{CellValue, float_text};
use super::schemas::DateFormat;
use crate::modules::classes::AttributeDataType;

/// A conversion problem: code and message, as the API words them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    pub code: &'static str,
    pub message: String,
}

pub fn problem(code: &'static str, message: impl Into<String>) -> Problem {
    Problem { code, message: message.into() }
}

/// A converted cell.
#[derive(Debug, Clone, PartialEq)]
pub enum Converted {
    /// Empty after trimming (§2.3 decides what that means).
    Empty,
    Value(Value),
    /// A date-time without an offset, in the column's time zone.
    Local(NaiveDateTime),
}

/// How a column reads its cells.
#[derive(Debug, Clone)]
pub struct Reading<'a> {
    pub trim: bool,
    pub decimal_separator: char,
    pub date_format: DateFormat,
    pub enum_values: &'a [String],
}

/// NFC-normalised text of a cell (T10), trimmed when asked; `None` when blank.
pub fn text(cell: &CellValue, trim: bool) -> Option<String> {
    let raw = match cell {
        CellValue::Text(s) => {
            let s: String = s.nfc().collect();
            if trim { s.trim().to_owned() } else { s }
        }
        CellValue::Empty | CellValue::FormulaWithoutValue => String::new(),
        other => other.display(),
    };
    if raw.trim().is_empty() { None } else { Some(raw) }
}

fn invalid_type(expected: &str) -> Problem {
    problem("invalid_type", format!("Invalid input: expected {expected}"))
}

fn invalid_format(message: &str) -> Problem {
    problem("invalid_format", message)
}

/// A number written with `decimal` as the separator. Thousands separators
/// (space, `'`, the other separator) are refused rather than guessed.
pub fn number(text: &str, decimal: char) -> Result<f64, Problem> {
    let other = if decimal == ',' { '.' } else { ',' };
    let t = text.trim();
    if t.contains([' ', '\'', '\u{a0}', '\u{202f}']) || t.contains(other) {
        return Err(invalid_format("Not a number: remove the thousands separators"));
    }
    let normalised = if decimal == ',' { t.replace(',', ".") } else { t.to_owned() };
    let valid = !normalised.is_empty()
        && normalised.chars().all(|c| c.is_ascii_digit() || matches!(c, '.' | '-' | '+' | 'e' | 'E'))
        && normalised.chars().any(|c| c.is_ascii_digit());
    match normalised.parse::<f64>() {
        Ok(n) if valid && n.is_finite() => Ok(n),
        _ => Err(invalid_type("number")),
    }
}

fn json_number(n: f64, integer: bool) -> Result<Value, Problem> {
    if integer {
        if n.fract() != 0.0 || n.abs() > 9.007e15 {
            return Err(invalid_type("integer"));
        }
        return Ok(json!(n as i64));
    }
    serde_json::Number::from_f64(n).map(Value::Number).ok_or_else(|| invalid_type("number"))
}

fn date(text: &str, format: DateFormat) -> Result<NaiveDate, Problem> {
    let pattern = match format {
        DateFormat::Iso => "%Y-%m-%d",
        DateFormat::DayMonthYear => "%d.%m.%Y",
        DateFormat::MonthDayYear => "%m/%d/%Y",
    };
    NaiveDate::parse_from_str(text.trim(), pattern).map_err(|_| {
        let example = match format {
            DateFormat::Iso => "YYYY-MM-DD",
            DateFormat::DayMonthYear => "DD.MM.YYYY",
            DateFormat::MonthDayYear => "MM/DD/YYYY",
        };
        invalid_format(&format!("Not a date in the format {example}"))
    })
}

/// RFC 3339 with an offset, or a local date-time / date (then in the column's zone).
fn datetime(text: &str, format: DateFormat) -> Result<Converted, Problem> {
    let t = text.trim();
    if let Ok(dt) = DateTime::parse_from_rfc3339(t) {
        return Ok(Converted::Value(json!(dt.to_utc().to_rfc3339_opts(chrono::SecondsFormat::AutoSi, true))));
    }
    for pattern in ["%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%d %H:%M:%S%.f", "%Y-%m-%dT%H:%M", "%Y-%m-%d %H:%M"] {
        if let Ok(local) = NaiveDateTime::parse_from_str(t, pattern) {
            return Ok(Converted::Local(local));
        }
    }
    date(t, format)
        .or_else(|e| date(t, DateFormat::Iso).map_err(|_| e))
        .map(|d| Converted::Local(d.and_time(NaiveTime::MIN)))
        .map_err(|_| invalid_format("Not a date-time: use RFC 3339, e.g. 2026-09-30T14:00:00+02:00"))
}

/// Converts one cell for an attribute or core field of `data_type`.
/// Lookup and reference cells come back as their text (the planner resolves them).
pub fn convert(cell: &CellValue, data_type: AttributeDataType, r: &Reading<'_>) -> Result<Converted, Problem> {
    use AttributeDataType as T;
    if let CellValue::Error(e) = cell {
        return Err(problem("cell_error", format!("The cell holds the spreadsheet error {e}")));
    }
    if *cell == CellValue::FormulaWithoutValue {
        return Err(problem(
            "formula_without_value",
            "The cell holds a formula without a stored result. Open and save the file in your spreadsheet program.",
        ));
    }
    if cell.is_blank() {
        return Ok(Converted::Empty);
    }
    let v = match (data_type, cell) {
        (T::Integer | T::Number, CellValue::Int(i)) => json_number(*i as f64, data_type == T::Integer)?,
        (T::Integer | T::Number, CellValue::Float(f)) => json_number(*f, data_type == T::Integer)?,
        (T::Integer | T::Number, CellValue::Text(_)) => {
            let t = text(cell, true).unwrap_or_default();
            json_number(number(&t, r.decimal_separator)?, data_type == T::Integer)?
        }
        (T::Boolean, CellValue::Bool(b)) => json!(b),
        (T::Boolean, CellValue::Int(i)) if *i == 0 || *i == 1 => json!(*i == 1),
        (T::Boolean, CellValue::Float(f)) if *f == 0.0 || *f == 1.0 => json!(*f == 1.0),
        (T::Boolean, CellValue::Text(_)) => {
            let t = text(cell, true).unwrap_or_default().to_lowercase();
            match t.as_str() {
                "true" | "yes" | "1" => json!(true),
                "false" | "no" | "0" => json!(false),
                _ => return Err(invalid_type("true, false, yes, no, 1 or 0")),
            }
        }
        (T::Date, CellValue::Date(d)) => json!(d.format("%Y-%m-%d").to_string()),
        (T::Date, CellValue::DateTime(dt)) if dt.time() == NaiveTime::MIN => {
            json!(dt.date().format("%Y-%m-%d").to_string())
        }
        (T::Date, CellValue::Text(_)) => {
            json!(date(&text(cell, true).unwrap_or_default(), r.date_format)?.format("%Y-%m-%d").to_string())
        }
        (T::Datetime, CellValue::DateTime(dt)) => return Ok(Converted::Local(*dt)),
        (T::Datetime, CellValue::Date(d)) => return Ok(Converted::Local(d.and_time(NaiveTime::MIN))),
        (T::Datetime, CellValue::Text(_)) => return datetime(&text(cell, true).unwrap_or_default(), r.date_format),
        (T::Date | T::Datetime | T::Boolean, _) => return Err(invalid_type(type_name(data_type))),
        (T::Enum, _) => {
            let t = text(cell, r.trim).unwrap_or_default();
            if let Some(exact) = r.enum_values.iter().find(|v| **v == t) {
                json!(exact)
            } else {
                let lower = t.to_lowercase();
                let hits: Vec<&String> = r.enum_values.iter().filter(|v| v.to_lowercase() == lower).collect();
                match hits.as_slice() {
                    [one] => json!(one),
                    [] => json!(t),
                    _ => return Err(problem("ambiguous_value", "Several values of the list match regardless of case")),
                }
            }
        }
        (T::Text, CellValue::Text(_)) => json!(text(cell, r.trim).unwrap_or_default()),
        (T::Text, CellValue::Float(f)) => json!(float_text(*f)),
        // Numbers, dates and booleans read as text, and IP/CIDR, lookups and references as their text.
        (_, other) => json!(text(other, true).unwrap_or_default()),
    };
    Ok(Converted::Value(v))
}

fn type_name(t: AttributeDataType) -> &'static str {
    use AttributeDataType as T;
    match t {
        T::Boolean => "true or false",
        T::Date => "a date",
        T::Datetime => "a date-time",
        _ => "a value",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use AttributeDataType as T;

    fn r() -> Reading<'static> {
        Reading { trim: true, decimal_separator: '.', date_format: DateFormat::Iso, enum_values: &[] }
    }
    fn txt(s: &str) -> CellValue {
        CellValue::Text(s.into())
    }
    fn ok(c: &CellValue, t: AttributeDataType, r: &Reading<'_>) -> Value {
        match convert(c, t, r) {
            Ok(Converted::Value(v)) => v,
            other => panic!("{c:?} as {t:?}: {other:?}"),
        }
    }
    fn err(c: &CellValue, t: AttributeDataType, r: &Reading<'_>) -> &'static str {
        convert(c, t, r).expect_err("refused").code
    }

    #[test]
    fn text_is_trimmed_and_nfc_and_numbers_keep_their_shortest_form() {
        assert_eq!(ok(&txt("  Mu\u{308}ller "), T::Text, &r()), json!("Müller"));
        let keep = Reading { trim: false, ..r() };
        assert_eq!(ok(&txt(" a "), T::Text, &keep), json!(" a "));
        assert_eq!(ok(&CellValue::Float(12345.0), T::Text, &r()), json!("12345"));
        assert_eq!(
            ok(&CellValue::Date(NaiveDate::from_ymd_opt(2026, 1, 2).unwrap()), T::Text, &r()),
            json!("2026-01-02")
        );
        assert_eq!(convert(&txt("   "), T::Text, &r()), Ok(Converted::Empty));
    }

    #[test]
    fn numbers_follow_the_decimal_separator_and_refuse_thousands_separators() {
        assert_eq!(ok(&txt("1.5"), T::Number, &r()), json!(1.5));
        let comma = Reading { decimal_separator: ',', ..r() };
        assert_eq!(ok(&txt("1,5"), T::Number, &comma), json!(1.5));
        for bad in ["1.000,5", "1 000", "1'000"] {
            assert_eq!(err(&txt(bad), T::Number, &comma), "invalid_format", "{bad}");
        }
        assert_eq!(err(&txt("1,000.5"), T::Number, &r()), "invalid_format");
        assert_eq!(err(&txt("abc"), T::Number, &r()), "invalid_type");
        assert_eq!(ok(&txt("8"), T::Integer, &r()), json!(8));
        assert_eq!(ok(&CellValue::Float(8.0), T::Integer, &r()), json!(8));
        assert_eq!(err(&txt("8.5"), T::Integer, &r()), "invalid_type");
        assert_eq!(err(&txt("inf"), T::Number, &r()), "invalid_type");
    }

    #[test]
    fn booleans_dates_and_date_times() {
        for (s, b) in [("TRUE", true), ("yes", true), ("1", true), ("False", false), ("no", false), ("0", false)] {
            assert_eq!(ok(&txt(s), T::Boolean, &r()), json!(b), "{s}");
        }
        assert_eq!(err(&txt("maybe"), T::Boolean, &r()), "invalid_type");
        assert_eq!(ok(&CellValue::Bool(true), T::Boolean, &r()), json!(true));
        let de = Reading { date_format: DateFormat::DayMonthYear, ..r() };
        assert_eq!(ok(&txt("30.09.2026"), T::Date, &de), json!("2026-09-30"));
        let us = Reading { date_format: DateFormat::MonthDayYear, ..r() };
        assert_eq!(ok(&txt("09/30/2026"), T::Date, &us), json!("2026-09-30"));
        assert_eq!(err(&txt("2026-09-31"), T::Date, &r()), "invalid_format");
        assert_eq!(err(&CellValue::Float(45000.0), T::Date, &r()), "invalid_type", "a plain number is not a date");
        assert_eq!(ok(&txt("2026-09-30T12:00:00+02:00"), T::Datetime, &r()), json!("2026-09-30T10:00:00Z"));
        assert_eq!(
            convert(&txt("2026-09-30 12:00"), T::Datetime, &r()),
            Ok(Converted::Local(NaiveDate::from_ymd_opt(2026, 9, 30).unwrap().and_hms_opt(12, 0, 0).unwrap()))
        );
        assert_eq!(
            convert(&txt("30.09.2026"), T::Datetime, &de),
            Ok(Converted::Local(NaiveDate::from_ymd_opt(2026, 9, 30).unwrap().and_time(NaiveTime::MIN)))
        );
    }

    #[test]
    fn enums_formulas_and_error_cells() {
        let values = ["Gold".to_owned(), "Silver".to_owned()];
        let e = Reading { enum_values: &values, ..r() };
        assert_eq!(ok(&txt("Gold"), T::Enum, &e), json!("Gold"));
        assert_eq!(ok(&txt("gold"), T::Enum, &e), json!("Gold"));
        assert_eq!(ok(&txt("Bronze"), T::Enum, &e), json!("Bronze"), "left to validation");
        let twice = ["Gold".to_owned(), "GOLD".to_owned()];
        let e = Reading { enum_values: &twice, ..r() };
        assert_eq!(err(&txt("gold"), T::Enum, &e), "ambiguous_value");
        assert_eq!(err(&CellValue::FormulaWithoutValue, T::Text, &r()), "formula_without_value");
        assert_eq!(err(&CellValue::Error("#N/A".into()), T::Text, &r()), "cell_error");
    }
}
