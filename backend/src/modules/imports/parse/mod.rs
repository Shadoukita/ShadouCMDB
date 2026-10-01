//! Readers for uploaded files (SHAA-714 §5.2). They take untrusted bytes and
//! must never panic on them, never allocate in proportion to a number the file
//! claims (a coordinate, a declared size), and never report a cell's content
//! in an error: errors carry a code, a message and a position only (G4).
//!
//! Both readers hand rows to a callback, one at a time, so a file of any
//! length is read in bounded memory.

pub mod csv;
#[cfg(test)]
pub mod fixtures;
pub mod xlsx;

use std::ops::ControlFlow;

use chrono::{NaiveDate, NaiveDateTime};

/// Most characters in a header cell (§3.5).
pub const MAX_HEADER_CHARS: usize = 200;

/// One cell as the file holds it, before any conversion to an attribute value.
#[derive(Debug, Clone, PartialEq)]
pub enum CellValue {
    Empty,
    Text(String),
    Int(i64),
    Float(f64),
    Bool(bool),
    Date(NaiveDate),
    DateTime(NaiveDateTime),
    /// An XLSX error cell (`#N/A`, `#DIV/0!`, …).
    Error(String),
    /// An XLSX formula cell that has no cached value; formulas are never evaluated (I6).
    FormulaWithoutValue,
}

impl CellValue {
    /// Empty after trimming (§2.3).
    pub fn is_blank(&self) -> bool {
        match self {
            CellValue::Empty => true,
            CellValue::Text(s) => s.trim().is_empty(),
            _ => false,
        }
    }

    /// The cell as text, as the report and the preview show it. Numbers use
    /// their shortest decimal form (`12345`, not `12345.0`), dates ISO 8601.
    pub fn display(&self) -> String {
        match self {
            CellValue::Empty | CellValue::FormulaWithoutValue => String::new(),
            CellValue::Text(s) | CellValue::Error(s) => s.clone(),
            CellValue::Int(i) => i.to_string(),
            CellValue::Float(f) => float_text(*f),
            CellValue::Bool(b) => if *b { "TRUE" } else { "FALSE" }.to_owned(),
            CellValue::Date(d) => d.format("%Y-%m-%d").to_string(),
            CellValue::DateTime(dt) => dt.format("%Y-%m-%dT%H:%M:%S").to_string(),
        }
    }
}

/// The shortest decimal form of a number: integral values without a fraction.
pub fn float_text(f: f64) -> String {
    if f.is_finite() && f.fract() == 0.0 && f.abs() < 1e15 { format!("{}", f as i64) } else { format!("{f}") }
}

/// One data or header row. `number` counts like the spreadsheet does: the
/// first row of the file is 1.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub number: u32,
    pub cells: Vec<CellValue>,
}

impl Row {
    pub fn is_blank(&self) -> bool {
        self.cells.iter().all(CellValue::is_blank)
    }
}

/// Why a file cannot be read. The message never quotes the file's content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub code: &'static str,
    pub message: String,
    /// 1-based row, when the problem is in one.
    pub row: Option<u32>,
    /// 0-based column, when the problem is in one.
    pub column: Option<u32>,
}

impl ParseError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        ParseError { code, message: message.into(), row: None, column: None }
    }

    pub fn at(mut self, row: Option<u32>, column: Option<u32>) -> Self {
        self.row = row;
        self.column = column;
        self
    }
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({})", self.message, self.code)?;
        if let Some(r) = self.row {
            write!(f, " at row {r}")?;
        }
        if let Some(c) = self.column {
            write!(f, ", column {}", column_name(c))?;
        }
        Ok(())
    }
}

/// Spreadsheet column letters for a 0-based index: 0 → A, 26 → AA.
pub fn column_name(index: u32) -> String {
    let mut n = index as u64 + 1;
    let mut out = Vec::new();
    while n > 0 {
        let rem = ((n - 1) % 26) as u8;
        out.push(b'A' + rem);
        n = (n - 1) / 26;
    }
    out.reverse();
    String::from_utf8(out).unwrap_or_default()
}

/// Bounds checked while a file is read (§3.5).
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// Data rows, the header row not counted.
    pub max_rows: u32,
    pub max_columns: u32,
}

/// What the callback returns for each row: go on, or stop reading.
pub type Flow = ControlFlow<()>;

/// Rows are handed over with their original column count; this keeps the
/// callback independent of the format.
pub type OnRow<'a> = &'a mut dyn FnMut(Row) -> Flow;

/// Error for a data row count above the limit.
pub fn row_limit(limits: &Limits, row: u32) -> ParseError {
    ParseError::new(
        "row_limit",
        format!(
            "The file has more than {} data rows. Split the file and import it in parts.",
            group_thousands(limits.max_rows as u64)
        ),
    )
    .at(Some(row), None)
}

/// Error for a column beyond the limit.
pub fn column_limit(limits: &Limits, row: u32, column: u32) -> ParseError {
    ParseError::new(
        "column_limit",
        format!("The file has more than {} columns. Remove the columns you do not import.", limits.max_columns),
    )
    .at(Some(row), Some(column))
}

/// Refuses a cell longer than [`MAX_CELL_CHARS`](super::MAX_CELL_CHARS)
/// characters with `cell_too_long` (§3.5). Called before the text is copied,
/// so a long shared string referenced from many cells is never multiplied.
pub fn check_cell(text: &str, row: u32, column: u32) -> Result<(), ParseError> {
    let max = super::MAX_CELL_CHARS as usize;
    // A character takes at least one byte: most cells need no count.
    if text.len() > max && text.chars().count() > max {
        return Err(ParseError::new(
            "cell_too_long",
            format!("A cell is longer than {} characters.", group_thousands(max as u64)),
        )
        .at(Some(row), Some(column)));
    }
    Ok(())
}

/// 100000 → "100,000".
pub fn group_thousands(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helpers() {
        assert_eq!(column_name(0), "A");
        assert_eq!(column_name(25), "Z");
        assert_eq!(column_name(26), "AA");
        assert_eq!(column_name(16383), "XFD");
        assert_eq!(group_thousands(100_000), "100,000");
        assert_eq!(group_thousands(999), "999");
        assert_eq!(float_text(12345.0), "12345");
        assert_eq!(float_text(1.5), "1.5");
        assert!(CellValue::Text("  ".into()).is_blank());
        assert!(!CellValue::Int(0).is_blank());
    }

    #[test]
    fn cells_are_capped_in_characters_not_bytes() {
        assert!(check_cell(&"x".repeat(10_000), 2, 0).is_ok());
        // 10,000 characters of two bytes each.
        assert!(check_cell(&"ü".repeat(10_000), 2, 0).is_ok());
        let err = check_cell(&"ü".repeat(10_001), 7, 3).unwrap_err();
        assert_eq!((err.code, err.row, err.column), ("cell_too_long", Some(7), Some(3)));
        assert_eq!(err.message, "A cell is longer than 10,000 characters.");
    }
}
