//! Reading an uploaded file the same way for every phase: which sheet,
//! encoding and delimiter, the header row, and the data rows (§1.2 step 1).
//! The analysis phase reads the file once to describe it; the dry run and the
//! commit read it again through [`read_file`] with the options the analysis
//! settled on.

use std::io::{Read, Seek};
use std::ops::ControlFlow;

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::parse::csv::{self, Encoding};
use super::parse::{self, CellValue, Limits, MAX_HEADER_CHARS, ParseError, Row, column_name, xlsx};

/// Rows the step 1 preview shows.
pub const PREVIEW_ROWS: usize = 20;
/// Samples per column.
pub const SAMPLES: usize = 3;
/// Characters of a sample or preview cell.
pub const SAMPLE_CHARS: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum FileFormat {
    Csv,
    Xlsx,
}

impl FileFormat {
    pub fn as_str(self) -> &'static str {
        match self {
            FileFormat::Csv => "csv",
            FileFormat::Xlsx => "xlsx",
        }
    }
}

/// How to read the file; `None` is "detect" (§3.2 `updateImportFileOptions`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FileOptions {
    /// XLSX: the worksheet to read (default: the first visible one)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sheet: Option<String>,
    /// CSV: the encoding (default: from the byte order mark, else UTF-8 when valid, else Windows-1252)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encoding: Option<Encoding>,
    /// CSV: `,` `;` `\t` or `|` (default: detected from the first line)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(pattern = "^[,;\\t|]$")]
    pub delimiter: Option<String>,
    /// Whether the first row holds the column names (default true)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub has_header_row: Option<bool>,
}

impl FileOptions {
    pub fn has_header_row(&self) -> bool {
        self.has_header_row.unwrap_or(true)
    }

    pub fn delimiter_byte(&self) -> Option<u8> {
        match self.delimiter.as_deref() {
            Some("\t") => Some(b'\t'),
            Some(d) if d.len() == 1 && csv::DELIMITERS.contains(&d.as_bytes()[0]) => Some(d.as_bytes()[0]),
            _ => None,
        }
    }
}

/// One column of the file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ColumnInfo {
    /// 0-based
    pub index: u32,
    /// The header cell, or "Column A" … without a header row
    pub header: String,
    /// The first three non-empty values, each at most 200 characters
    pub samples: Vec<String>,
}

/// A data row as step 1 previews it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PreviewRow {
    /// Row number in the file (the header is row 1)
    pub row: u32,
    pub cells: Vec<String>,
}

/// What the analysis found (stored in `import_jobs.file_info`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileInfo {
    pub sheets: Vec<String>,
    pub hidden_sheets: Vec<String>,
    pub sheet: Option<String>,
    pub encoding: Option<Encoding>,
    pub delimiter: Option<String>,
    pub has_header_row: bool,
    pub row_count: u32,
    pub column_count: u32,
    pub columns: Vec<ColumnInfo>,
    pub preview_rows: Vec<PreviewRow>,
}

/// Opens the stored file again for each pass.
pub trait Source {
    type Reader: Read + Seek;
    fn open(&self) -> Result<Self::Reader, ParseError>;
}

/// An in-memory file (tests, fuzzing).
impl Source for Vec<u8> {
    type Reader = std::io::Cursor<Vec<u8>>;
    fn open(&self) -> Result<Self::Reader, ParseError> {
        Ok(std::io::Cursor::new(self.clone()))
    }
}

/// How a file was read: what was chosen or detected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    pub sheets: Vec<String>,
    pub hidden_sheets: Vec<String>,
    pub sheet: Option<String>,
    pub encoding: Option<Encoding>,
    pub delimiter: Option<u8>,
    /// The header row's cells (empty without a header row).
    pub header: Vec<String>,
}

fn header_cells(row: &Row) -> Result<Vec<String>, ParseError> {
    row.cells
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let text = c.display().trim().to_owned();
            if text.chars().count() > MAX_HEADER_CHARS {
                Err(ParseError::new(
                    "header_too_long",
                    format!("A column name is longer than {MAX_HEADER_CHARS} characters."),
                )
                .at(Some(row.number), Some(i as u32)))
            } else {
                Ok(text)
            }
        })
        .collect()
}

/// Takes the header row off, skips blank rows, counts data rows against the limit.
struct RowFilter<'l> {
    with_header: bool,
    header: Option<Vec<String>>,
    data_rows: u32,
    failure: Option<ParseError>,
    limits: &'l Limits,
}

impl RowFilter<'_> {
    fn reset(&mut self) {
        self.header = None;
        self.data_rows = 0;
        self.failure = None;
    }

    fn handle(&mut self, row: Row, on_data: parse::OnRow<'_>) -> parse::Flow {
        if row.is_blank() {
            return ControlFlow::Continue(());
        }
        if self.with_header && self.header.is_none() {
            match header_cells(&row) {
                Ok(h) => self.header = Some(h),
                Err(e) => {
                    self.failure = Some(e);
                    return ControlFlow::Break(());
                }
            }
            return ControlFlow::Continue(());
        }
        self.data_rows += 1;
        if self.data_rows > self.limits.max_rows {
            self.failure = Some(parse::row_limit(self.limits, row.number));
            return ControlFlow::Break(());
        }
        on_data(row)
    }
}

/// Reads the file: the header row (when there is one) is taken off, blank
/// rows are skipped, every data row goes to `on_data`. More than
/// `limits.max_rows` data rows is `row_limit`.
pub fn read_file<S: Source>(
    source: &S,
    format: FileFormat,
    options: &FileOptions,
    limits: &Limits,
    on_data: parse::OnRow<'_>,
) -> Result<Layout, ParseError> {
    let mut filter =
        RowFilter { with_header: options.has_header_row(), header: None, data_rows: 0, failure: None, limits };
    let mut layout = match format {
        FileFormat::Csv => {
            let encoding = match options.encoding {
                Some(e) => e,
                None => {
                    let mut head = [0u8; 4];
                    let n = read_head(&mut source.open()?, &mut head)?;
                    match csv::bom_encoding(&head[..n])? {
                        Some(e) => e,
                        // Valid UTF-8 is read as UTF-8, anything else with the Windows default.
                        None if csv::is_utf8(source.open()?)? => Encoding::Utf8,
                        None => Encoding::Windows1252,
                    }
                }
            };
            let detected = csv::read(source.open()?, encoding, options.delimiter_byte(), limits, &mut |row| {
                filter.handle(row, on_data)
            })?;
            Layout {
                sheets: Vec::new(),
                hidden_sheets: Vec::new(),
                sheet: None,
                encoding: Some(detected.encoding),
                delimiter: Some(detected.delimiter),
                header: Vec::new(),
            }
        }
        FileFormat::Xlsx => {
            let mut reader = source.open()?;
            xlsx::preflight(&mut reader)?;
            let (mut workbook, sheets) = xlsx::open(reader)?;
            let chosen =
                match &options.sheet {
                    Some(name) => sheets.iter().find(|s| &s.name == name).cloned().ok_or_else(|| {
                        ParseError::new("sheet_not_found", "The workbook has no worksheet of this name.")
                    })?,
                    None => sheets
                        .iter()
                        .find(|s| !s.hidden)
                        .or(sheets.first())
                        .cloned()
                        .ok_or_else(|| ParseError::new("no_worksheet", "The workbook has no worksheet."))?,
                };
            xlsx::read_sheet(&mut workbook, &chosen.name, limits, &mut |row| filter.handle(row, on_data))?;
            Layout {
                sheets: sheets.iter().map(|s| s.name.clone()).collect(),
                hidden_sheets: sheets.iter().filter(|s| s.hidden).map(|s| s.name.clone()).collect(),
                sheet: Some(chosen.name),
                encoding: None,
                delimiter: None,
                header: Vec::new(),
            }
        }
    };
    if let Some(e) = filter.failure {
        return Err(e);
    }
    layout.header = filter.header.unwrap_or_default();
    Ok(layout)
}

fn read_head<R: Read>(r: &mut R, buf: &mut [u8]) -> Result<usize, ParseError> {
    let mut n = 0;
    while n < buf.len() {
        match r.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(ParseError::new("read_failed", format!("The stored file could not be read: {e}"))),
        }
    }
    Ok(n)
}

fn clip(s: &str) -> String {
    if s.chars().count() > SAMPLE_CHARS { s.chars().take(SAMPLE_CHARS).collect() } else { s.to_owned() }
}

/// Describes a file: its sheets or encoding and delimiter, columns with
/// samples, the first rows and the row count. `progress` gets the rows read so
/// far and may stop the analysis (cancel, lost lease).
pub fn analyse<S: Source>(
    source: &S,
    format: FileFormat,
    options: &FileOptions,
    limits: &Limits,
    progress: &mut dyn FnMut(u32) -> parse::Flow,
) -> Result<FileInfo, ParseError> {
    let mut rows: u32 = 0;
    let mut width: usize = 0;
    let mut samples: Vec<Vec<String>> = Vec::new();
    let mut preview: Vec<PreviewRow> = Vec::new();
    let mut stopped = false;
    let layout = read_file(source, format, options, limits, &mut |row: Row| {
        rows += 1;
        width = width.max(row.cells.len());
        if samples.len() < row.cells.len() {
            samples.resize(row.cells.len(), Vec::new());
        }
        for (i, c) in row.cells.iter().enumerate() {
            if samples[i].len() < SAMPLES && !c.is_blank() {
                samples[i].push(clip(c.display().trim()));
            }
        }
        if preview.len() < PREVIEW_ROWS {
            preview.push(PreviewRow { row: row.number, cells: row.cells.iter().map(|c| clip(&c.display())).collect() });
        }
        if rows % 1000 == 0 && progress(rows).is_break() {
            stopped = true;
            return ControlFlow::Break(());
        }
        ControlFlow::Continue(())
    })?;
    if stopped {
        return Err(ParseError::new("stopped", "The analysis was stopped."));
    }
    let width = width.max(layout.header.len());
    samples.resize(width, Vec::new());
    let columns: Vec<ColumnInfo> = (0..width)
        .map(|i| {
            let header = layout.header.get(i).filter(|h| !h.is_empty()).cloned();
            ColumnInfo {
                index: i as u32,
                header: header.unwrap_or_else(|| format!("Column {}", column_name(i as u32))),
                samples: std::mem::take(&mut samples[i]),
            }
        })
        .collect();
    for p in &mut preview {
        p.cells.resize(width, String::new());
    }
    Ok(FileInfo {
        sheets: layout.sheets,
        hidden_sheets: layout.hidden_sheets,
        sheet: layout.sheet,
        encoding: layout.encoding,
        delimiter: layout.delimiter.map(|d| (d as char).to_string()),
        has_header_row: options.has_header_row(),
        row_count: rows,
        column_count: width as u32,
        columns,
        preview_rows: preview,
    })
}

/// A cell at `column` of a row, empty beyond its end.
pub fn cell(row: &Row, column: u32) -> &CellValue {
    row.cells.get(column as usize).unwrap_or(&CellValue::Empty)
}
