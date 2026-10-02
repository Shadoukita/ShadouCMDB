//! XLSX reader (§5.2): a preflight over the raw ZIP, then calamine's
//! streaming cell reader.
//!
//! The preflight runs before any ZIP or XLSX library sees the file. It reads
//! the end of central directory, the central directory and every local header
//! itself, and refuses what those libraries would accept silently or pay for
//! in memory: more than [`MAX_ENTRIES`] entries (checked before anything is
//! allocated per entry), duplicate names (compared case-insensitively),
//! a local header that disagrees with the central directory, compression
//! other than stored or deflate, encryption, path tricks, and macros. It then
//! decompresses every entry through counters (the actual size, not the
//! declared one: [`MAX_ENTRY_BYTES`] per entry, [`MAX_TOTAL_BYTES`] in total,
//! at most 1,000:1 above 1 MiB) and reads every XML part as events: a
//! DOCTYPE, a UTF-16 part or a non-UTF-8 declaration is refused, so no DTD or
//! entity is ever processed.
//!
//! The checks only hold if calamine reads the archive the preflight read, so
//! nothing is left for two readers to disagree on: the end record must end
//! the file (no comment), part names are printable ASCII, the file is opened
//! with `zip` as well and must list the same entries at the same offsets, and
//! the relationships must place the workbook at `xl/workbook.xml` and every
//! sheet in a sheet folder.
//!
//! Only then is the file opened, with `calamine::Xlsx` (never
//! `open_workbook_auto`, so the XLS, XLSB and ODS parsers are unreachable),
//! and read cell by cell with `worksheet_cells_reader`. Nothing allocates from
//! the sheet's `<dimension>`: a cell's coordinates are checked against the
//! limits before its row is built. Formulas are never evaluated; their cached
//! value is used (I6).

use std::cell::Cell;
use std::collections::HashSet;
use std::io::{Read, Seek, SeekFrom};

use calamine::{DataRef, Reader, SheetType, SheetVisible, Xlsx};
use quick_xml::events::Event;

use super::{CellValue, Limits, OnRow, ParseError, Row, check_cell, column_limit, group_thousands};

pub const MAX_ENTRIES: u64 = 10_000;
pub const MAX_ENTRY_BYTES: u64 = 256 * 1024 * 1024;
pub const MAX_TOTAL_BYTES: u64 = 512 * 1024 * 1024;
/// Decompressed size of `xl/sharedStrings.xml`, which calamine holds in memory.
pub const MAX_SHARED_STRINGS_BYTES: u64 = 64 * 1024 * 1024;
/// Decompressed size of the other parts held in memory: the workbook, styles,
/// content types and every relationships part.
pub const MAX_SMALL_PART_BYTES: u64 = 16 * 1024 * 1024;
/// A compression ratio above this, for an entry over 1 MiB, is a zip bomb.
pub const MAX_RATIO: u64 = 1000;
/// The largest XML event (a tag, a text node, a comment, a CDATA section):
/// nothing in a real workbook comes near it (Excel caps a cell at 32,767
/// characters), and it bounds the XML readers' buffers, which hold one event.
/// Counted as the bytes read between two events (exact to one 64 KiB read),
/// so a `>` inside an attribute value or a `<` inside CDATA does not end the
/// count.
pub const MAX_XML_TOKEN: u64 = 4 * 1024 * 1024;
/// Most bytes of text in one row, as the CSV reader allows for a record.
pub const MAX_ROW_TEXT_BYTES: usize = super::csv::MAX_RECORD_BYTES;
/// The ceiling on [`Limits::max_text_bytes`]. A shared string is stored once
/// but read once per cell that refers to it, so a sheet is held to the text a
/// CSV file of the upload limit holds ([`Limits::new`]), and never more than this.
pub const MAX_SHEET_TEXT_BYTES: usize = 256 * 1024 * 1024;

const SPREADSHEET_MAIN: [&str; 2] = [
    "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml",
    "application/vnd.openxmlformats-officedocument.spreadsheetml.template.main+xml",
];
const MACRO_MAIN: [&str; 3] = [
    "application/vnd.ms-excel.sheet.macroenabled.main+xml",
    "application/vnd.ms-excel.template.macroenabled.main+xml",
    "application/vnd.ms-excel.addin.macroenabled.main+xml",
];
const BINARY_MAIN: &str = "application/vnd.ms-excel.sheet.binary.macroenabled.main";

/// What the first bytes of an upload say it is (§5.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sniffed {
    Zip,
    /// OLE compound file: an encrypted workbook or an `.xls`.
    Ole,
    Other,
}

pub fn sniff(head: &[u8]) -> Sniffed {
    if head.starts_with(b"PK\x03\x04") {
        Sniffed::Zip
    } else if head.starts_with(&[0xD0, 0xCF, 0x11, 0xE0]) {
        Sniffed::Ole
    } else {
        Sniffed::Other
    }
}

pub fn ole_error() -> ParseError {
    ParseError::new(
        "workbook_encrypted_or_xls",
        "Password-protected workbooks and old .xls files are not supported. Save the file as an unprotected .xlsx.",
    )
}

fn bad(code: &'static str, message: impl Into<String>) -> ParseError {
    ParseError::new(code, message)
}

fn not_a_workbook(detail: &str) -> ParseError {
    bad("not_a_workbook", format!("The file is not a valid .xlsx workbook ({detail})."))
}

fn io_err(e: std::io::Error) -> ParseError {
    io_err_ref(&e)
}

fn io_err_ref(e: &std::io::Error) -> ParseError {
    if e.kind() == std::io::ErrorKind::UnexpectedEof {
        not_a_workbook("it ends early")
    } else if let Some(p) = e.get_ref().and_then(|i| i.downcast_ref::<ParseErrorBox>()) {
        p.0.clone()
    } else {
        bad("read_failed", format!("The stored file could not be read: {e}"))
    }
}

/// A [`ParseError`] travelling through an `io::Error` (from the counting readers).
#[derive(Debug)]
struct ParseErrorBox(ParseError);
impl std::fmt::Display for ParseErrorBox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}
impl std::error::Error for ParseErrorBox {}

fn as_io(e: ParseError) -> std::io::Error {
    std::io::Error::other(ParseErrorBox(e))
}

fn u16_at(b: &[u8], i: usize) -> u16 {
    u16::from_le_bytes([b[i], b[i + 1]])
}
fn u32_at(b: &[u8], i: usize) -> u32 {
    u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]])
}
fn u64_at(b: &[u8], i: usize) -> u64 {
    u64::from_le_bytes(b[i..i + 8].try_into().unwrap_or([0; 8]))
}

fn read_exact_at<R: Read + Seek>(r: &mut R, offset: u64, buf: &mut [u8]) -> Result<(), ParseError> {
    r.seek(SeekFrom::Start(offset)).map_err(io_err)?;
    r.read_exact(buf).map_err(io_err)
}

/// One central directory entry, as the preflight needs it.
#[derive(Debug, Clone)]
struct Entry {
    name: String,
    method: u16,
    flags: u16,
    crc: u32,
    compressed: u64,
    size: u64,
    local_offset: u64,
}

/// Where the central directory is and how many entries it declares.
struct Directory {
    entries: u64,
    size: u64,
    offset: u64,
}

fn find_directory<R: Read + Seek>(r: &mut R, len: u64) -> Result<Directory, ParseError> {
    const EOCD: u32 = 0x0605_4b50;
    // The record ends the file, with no comment: spreadsheet programs write none,
    // and a comment is where a second end record would hide from one reader.
    if len < 22 {
        return Err(not_a_workbook("no end of central directory"));
    }
    let eocd_offset = len - 22;
    let mut rec = [0u8; 22];
    read_exact_at(r, eocd_offset, &mut rec)?;
    if u32_at(&rec, 0) != EOCD || u16_at(&rec, 20) != 0 {
        return Err(not_a_workbook("no end of central directory at the end of the file"));
    }
    let rec = &rec[..];
    if u16_at(rec, 4) != 0 || u16_at(rec, 6) != 0 {
        return Err(not_a_workbook("a multi-part archive"));
    }
    let mut dir =
        Directory { entries: u16_at(rec, 10) as u64, size: u32_at(rec, 12) as u64, offset: u32_at(rec, 16) as u64 };
    if dir.entries == 0xFFFF || dir.size == 0xFFFF_FFFF || dir.offset == 0xFFFF_FFFF {
        // ZIP64: the locator sits right before the end record, and the ZIP64
        // record, without extensible data, right before the locator.
        if eocd_offset < 20 + 56 {
            return Err(not_a_workbook("a broken ZIP64 directory"));
        }
        let mut loc = [0u8; 20];
        read_exact_at(r, eocd_offset - 20, &mut loc)?;
        if u32_at(&loc, 0) != 0x0706_4b50 || u32_at(&loc, 4) != 0 || u32_at(&loc, 16) != 1 {
            return Err(not_a_workbook("a broken ZIP64 directory"));
        }
        let rec_offset = u64_at(&loc, 8);
        if rec_offset != eocd_offset - 20 - 56 {
            return Err(not_a_workbook("a broken ZIP64 directory"));
        }
        let mut rec = [0u8; 56];
        read_exact_at(r, rec_offset, &mut rec)?;
        if u32_at(&rec, 0) != 0x0606_4b50 || u64_at(&rec, 4) != 44 {
            return Err(not_a_workbook("a broken ZIP64 directory"));
        }
        dir = Directory { entries: u64_at(&rec, 32), size: u64_at(&rec, 40), offset: u64_at(&rec, 48) };
    }
    if dir.entries > MAX_ENTRIES {
        return Err(bad(
            "zip_entries",
            format!(
                "The workbook has more than {} parts, which no spreadsheet needs.",
                super::group_thousands(MAX_ENTRIES)
            ),
        ));
    }
    if dir.offset.checked_add(dir.size).is_none_or(|end| end > len) {
        return Err(not_a_workbook("the central directory lies outside the file"));
    }
    Ok(dir)
}

/// Part names are printable ASCII without `\`: then the name the preflight
/// reads, the one `zip` decodes (UTF-8 or CP437) and the one calamine looks
/// up (lower case, `\` as `/`) are the same string. Spreadsheet programs
/// write nothing else.
fn check_name(name: &str) -> Result<(), ParseError> {
    let bad_name = || bad("zip_entry_name", "The workbook contains a part with an unsafe name.");
    if name.is_empty() || name.starts_with('/') || !name.bytes().all(|b| (0x20..=0x7e).contains(&b) && b != b'\\') {
        return Err(bad_name());
    }
    if name.len() >= 2 && name.as_bytes()[1] == b':' {
        return Err(bad_name());
    }
    if name.split('/').any(|seg| seg == "..") {
        return Err(bad_name());
    }
    Ok(())
}

fn read_directory<R: Read + Seek>(r: &mut R, dir: &Directory) -> Result<Vec<Entry>, ParseError> {
    let mut cd = vec![0u8; dir.size as usize];
    read_exact_at(r, dir.offset, &mut cd)?;
    let mut entries = Vec::with_capacity(dir.entries as usize);
    let mut seen: HashSet<String> = HashSet::with_capacity(dir.entries as usize);
    let mut i = 0usize;
    for _ in 0..dir.entries {
        if i + 46 > cd.len() || u32_at(&cd, i) != 0x0201_4b50 {
            return Err(not_a_workbook("a broken central directory"));
        }
        let h = &cd[i..i + 46];
        let (name_len, extra_len, comment_len) =
            (u16_at(h, 28) as usize, u16_at(h, 30) as usize, u16_at(h, 32) as usize);
        let end = i + 46 + name_len + extra_len + comment_len;
        if end > cd.len() {
            return Err(not_a_workbook("a broken central directory"));
        }
        let name = std::str::from_utf8(&cd[i + 46..i + 46 + name_len])
            .map_err(|_| bad("zip_entry_name", "The workbook contains a part whose name is not UTF-8."))?
            .to_owned();
        let mut e = Entry {
            name,
            method: u16_at(h, 10),
            flags: u16_at(h, 8),
            crc: u32_at(h, 16),
            compressed: u32_at(h, 20) as u64,
            size: u32_at(h, 24) as u64,
            local_offset: u32_at(h, 42) as u64,
        };
        // ZIP64 extra field: the values that were 0xFFFFFFFF, in order.
        let extra = &cd[i + 46 + name_len..i + 46 + name_len + extra_len];
        let mut j = 0;
        while j + 4 <= extra.len() {
            let (id, len) = (u16_at(extra, j), u16_at(extra, j + 2) as usize);
            if j + 4 + len > extra.len() {
                return Err(not_a_workbook("a broken extra field"));
            }
            // Info-ZIP Unicode path and comment: `zip` would read another name.
            if id == 0x7075 || id == 0x6375 {
                return Err(bad("zip_entry_name", "The workbook contains a part with an unsafe name."));
            }
            if id == 0x0001 {
                let data = &extra[j + 4..j + 4 + len];
                let mut k = 0;
                for field in [&mut e.size, &mut e.compressed, &mut e.local_offset] {
                    if *field == 0xFFFF_FFFF {
                        if k + 8 > data.len() {
                            return Err(not_a_workbook("a broken ZIP64 field"));
                        }
                        *field = u64_at(data, k);
                        k += 8;
                    }
                }
            }
            j += 4 + len;
        }
        check_name(&e.name)?;
        if !seen.insert(e.name.to_lowercase()) {
            return Err(bad("zip_duplicate_entry", "The workbook contains two parts with the same name."));
        }
        if e.flags & 0x0001 != 0 {
            return Err(ole_error());
        }
        if e.method != 0 && e.method != 8 {
            return Err(bad(
                "zip_compression",
                "The workbook uses a compression method other than deflate. Save it again from your spreadsheet program.",
            ));
        }
        entries.push(e);
        i = end;
    }
    Ok(entries)
}

/// The local header must agree with the central directory: a ZIP reader that
/// trusts one and a reader that trusts the other must see the same file.
fn check_local_header<R: Read + Seek>(r: &mut R, e: &Entry, len: u64) -> Result<u64, ParseError> {
    let mismatch = || bad("zip_header_mismatch", "The workbook's ZIP headers contradict each other.");
    if e.local_offset.checked_add(30).is_none_or(|end| end > len) {
        return Err(mismatch());
    }
    let mut h = [0u8; 30];
    read_exact_at(r, e.local_offset, &mut h)?;
    let (name_len, extra_len) = (u16_at(&h, 26) as u64, u16_at(&h, 28) as u64);
    if u32_at(&h, 0) != 0x0403_4b50 || u16_at(&h, 8) != e.method || name_len != e.name.len() as u64 {
        return Err(mismatch());
    }
    let mut name = vec![0u8; name_len as usize];
    r.read_exact(&mut name).map_err(io_err)?;
    if name != e.name.as_bytes() {
        return Err(mismatch());
    }
    let local_flags = u16_at(&h, 6);
    if (local_flags & 0x0008) == 0 {
        let (crc, comp, size) = (u32_at(&h, 14), u32_at(&h, 18) as u64, u32_at(&h, 22) as u64);
        let zip64 = comp == 0xFFFF_FFFF || size == 0xFFFF_FFFF;
        if crc != e.crc || (!zip64 && (comp != e.compressed || size != e.size)) {
            return Err(mismatch());
        }
    }
    let data = e.local_offset + 30 + name_len + extra_len;
    if data.checked_add(e.compressed).is_none_or(|end| end > len) {
        return Err(mismatch());
    }
    Ok(data)
}

/// Counts what a decompressor produces, refuses what is too large, too
/// compressed, or too long for one XML event, and keeps the CRC.
struct Counted<'a, R> {
    inner: R,
    name: String,
    produced: u64,
    compressed: u64,
    cap: u64,
    total_before: u64,
    /// Bytes read since [`scan_xml`] last finished an event; `None` for a
    /// part that is not XML.
    run: Option<&'a Cell<u64>>,
    crc: flate2::Crc,
}

impl<R: Read> Read for Counted<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.produced += n as u64;
        self.crc.update(&buf[..n]);
        if self.produced > self.cap {
            let code = if self.cap < MAX_ENTRY_BYTES { "part_too_large" } else { "zip_entry_too_large" };
            return Err(as_io(bad(code, format!("The workbook part {} is too large once unpacked.", self.name))));
        }
        if self.total_before + self.produced > MAX_TOTAL_BYTES {
            return Err(as_io(bad("zip_too_large", "The workbook is larger than 512 MiB once unpacked.")));
        }
        if self.produced > 1024 * 1024 && self.produced > self.compressed.saturating_mul(MAX_RATIO) {
            return Err(as_io(bad("zip_bomb", "The workbook is compressed suspiciously well and is refused.")));
        }
        if let Some(run) = self.run {
            run.set(run.get() + n as u64);
            if run.get() > MAX_XML_TOKEN {
                return Err(as_io(bad(
                    "xml_token_too_large",
                    format!("The workbook part {} holds an oversized XML token.", self.name),
                )));
            }
        }
        Ok(n)
    }
}

fn part_cap(name: &str) -> u64 {
    let lower = name.to_lowercase();
    if lower == "xl/sharedstrings.xml" {
        MAX_SHARED_STRINGS_BYTES
    } else if lower == "[content_types].xml"
        || lower == "xl/workbook.xml"
        || lower == "xl/styles.xml"
        || lower.ends_with(".rels")
    {
        MAX_SMALL_PART_BYTES
    } else {
        MAX_ENTRY_BYTES
    }
}

fn is_xml(name: &str) -> bool {
    let lower = name.to_lowercase();
    lower.ends_with(".xml") || lower.ends_with(".rels")
}

/// A cell reference, range or list of them as a worksheet writes it (`B2`,
/// `$A$1:XFD1048576`, `A:A`, `A1 C3`): at most 3 letters and 7 digits per
/// cell, and a range's first cell before its last. calamine's reference
/// parser overflows on longer runs and on backward ranges, so anything else
/// is refused before it gets there.
pub fn is_cell_reference(value: &[u8]) -> bool {
    /// (column, row) of one cell; either may be missing.
    fn cell(text: &[u8]) -> Option<(Option<u32>, Option<u32>)> {
        let text: Vec<u8> = text.iter().copied().filter(|&b| b != b'$').collect();
        let letters = text.iter().take_while(|b| b.is_ascii_alphabetic()).count();
        let digits = text[letters..].iter().take_while(|b| b.is_ascii_digit()).count();
        if letters > 3 || digits > 7 || letters + digits != text.len() {
            return None;
        }
        let col = text[..letters].iter().fold(0u32, |c, b| c * 26 + u32::from(b.to_ascii_uppercase() - b'A') + 1);
        let row = text[letters..].iter().fold(0u32, |r, b| r * 10 + u32::from(b - b'0'));
        Some(((letters > 0).then_some(col), (digits > 0).then_some(row)))
    }
    value.len() <= 1024
        && value.split(|&b| b == b' ').all(|range| {
            let cells: Option<Vec<_>> = range.split(|&b| b == b':').map(cell).collect();
            match cells.as_deref() {
                Some([_]) => true,
                Some([(c1, r1), (c2, r2)]) => {
                    c1.zip(*c2).is_none_or(|(a, b)| a <= b) && r1.zip(*r2).is_none_or(|(a, b)| a <= b)
                }
                _ => false,
            }
        })
}

/// The folders calamine may read a sheet from (§5.2).
const SHEET_FOLDERS: [&str; 3] = ["xl/worksheets/", "xl/chartsheets/", "xl/dialogsheets/"];

fn is_sheet_part(lower: &str) -> bool {
    SHEET_FOLDERS.iter().any(|f| lower.starts_with(f))
}

/// One `Relationship` element: every attribute named `Type` or `Target`, with
/// or without a prefix (calamine reads the unprefixed one).
#[derive(Debug, Default)]
struct Relationship {
    types: Vec<Vec<u8>>,
    targets: Vec<Vec<u8>>,
}

/// What [`scan_xml`] collects from the package parts.
#[derive(Default)]
struct Found {
    /// `ContentType` attributes of `[Content_Types].xml`.
    content_types: Vec<String>,
    /// `_rels/.rels`: calamine takes the workbook's folder from it.
    package_rels: Vec<Relationship>,
    /// `xl/_rels/workbook.xml.rels`: calamine takes the sheet paths from it.
    workbook_rels: Vec<Relationship>,
}

/// Reads an XML part as events: refuses a DOCTYPE and any encoding but UTF-8,
/// and an event longer than [`MAX_XML_TOKEN`] (`run` counts the bytes read
/// since the last event). Collects content types and relationships into
/// `found`. In a sheet, every `r`, `ref` and `sqref` must be a cell reference.
fn scan_xml<R: Read>(input: R, name: &str, run: &Cell<u64>, found: &mut Found) -> Result<(), ParseError> {
    let lower = name.to_ascii_lowercase();
    let sheet = is_sheet_part(&lower);
    let is_types = name == "[Content_Types].xml";
    let mut rels = match lower.as_str() {
        "_rels/.rels" => Some(&mut found.package_rels),
        "xl/_rels/workbook.xml.rels" => Some(&mut found.workbook_rels),
        _ => None,
    };
    let malformed = || not_a_workbook(&format!("the part {name} is not well-formed XML"));
    let mut input = std::io::BufReader::with_capacity(64 * 1024, input);
    let head = std::io::BufRead::fill_buf(&mut input).map_err(io_err)?;
    let encoding_error =
        || bad("xml_encoding", format!("The workbook part {name} is not UTF-8 encoded, which is not supported."));
    if head.starts_with(&[0xFF, 0xFE]) || head.starts_with(&[0xFE, 0xFF]) || head.iter().take(4).any(|&b| b == 0) {
        return Err(encoding_error());
    }
    let mut reader = quick_xml::Reader::from_reader(input);
    let mut buf = Vec::new();
    loop {
        let event = reader.read_event_into(&mut buf);
        run.set(0);
        match event {
            Ok(Event::Eof) => return Ok(()),
            Ok(Event::DocType(_)) => {
                return Err(bad(
                    "xml_doctype",
                    format!("The workbook part {name} contains a DOCTYPE, which is not allowed."),
                ));
            }
            Ok(Event::Decl(d)) => {
                if let Some(enc) = d.encoding() {
                    let enc = enc.map_err(|_| encoding_error())?;
                    let enc = String::from_utf8_lossy(&enc).to_lowercase();
                    if enc != "utf-8" && enc != "utf8" {
                        return Err(encoding_error());
                    }
                }
            }
            Ok(Event::Start(e) | Event::Empty(e)) => {
                if sheet {
                    for a in e.attributes() {
                        let a = a.map_err(|_| malformed())?;
                        if matches!(a.key.local_name().as_ref(), b"r" | b"ref" | b"sqref")
                            && !is_cell_reference(&a.value)
                        {
                            return Err(not_a_workbook(&format!("the part {name} has an invalid cell reference")));
                        }
                    }
                }
                if is_types {
                    for a in e.attributes().flatten() {
                        if a.key.local_name().as_ref() == b"ContentType" {
                            found.content_types.push(String::from_utf8_lossy(&a.value).to_lowercase());
                        }
                    }
                }
                if let Some(rels) = rels.as_mut()
                    && e.local_name().as_ref() == b"Relationship"
                {
                    let mut rel = Relationship::default();
                    for a in e.attributes() {
                        let a = a.map_err(|_| malformed())?;
                        match a.key.local_name().as_ref() {
                            b"Type" => rel.types.push(a.value.into_owned()),
                            b"Target" => rel.targets.push(a.value.into_owned()),
                            _ => {}
                        }
                    }
                    rels.push(rel);
                }
            }
            Ok(_) => {}
            Err(quick_xml::Error::Io(e)) => return Err(io_err_ref(&e)),
            Err(_) => return Err(malformed()),
        }
        buf.clear();
    }
}

/// Checks a whole XLSX file (see the module docs). Leaves the reader anywhere.
pub fn preflight<R: Read + Seek>(r: &mut R) -> Result<(), ParseError> {
    let len = r.seek(SeekFrom::End(0)).map_err(io_err)?;
    let mut head = [0u8; 4];
    if len < 4 {
        return Err(not_a_workbook("it is too short"));
    }
    read_exact_at(r, 0, &mut head)?;
    match sniff(&head) {
        Sniffed::Zip => {}
        Sniffed::Ole => return Err(ole_error()),
        Sniffed::Other => return Err(not_a_workbook("it is not a ZIP archive")),
    }
    let dir = find_directory(r, len)?;
    let entries = read_directory(r, &dir)?;

    // Macros and binary workbooks, by part name.
    for e in &entries {
        let lower = e.name.to_lowercase();
        let segments: Vec<&str> = lower.split('/').collect();
        if segments.contains(&"activex") || segments.last() == Some(&"vbaproject.bin") {
            return Err(macros());
        }
        if segments.last() == Some(&"workbook.bin") {
            return Err(binary_workbook());
        }
    }

    let starts = entries.iter().map(|e| check_local_header(r, e, len)).collect::<Result<Vec<u64>, _>>()?;
    cross_check(r, &dir, &entries, &starts)?;

    let mut found = Found::default();
    let mut has_content_types = false;
    let mut total = 0u64;
    let run = Cell::new(0u64);
    for (e, &data) in entries.iter().zip(&starts) {
        r.seek(SeekFrom::Start(data)).map_err(io_err)?;
        let raw = (&mut *r).take(e.compressed);
        let decoded: Box<dyn Read + '_> =
            if e.method == 8 { Box::new(flate2::read::DeflateDecoder::new(raw)) } else { Box::new(raw) };
        let xml = is_xml(&e.name);
        run.set(0);
        let mut counted = Counted {
            inner: decoded,
            name: e.name.clone(),
            produced: 0,
            compressed: e.compressed,
            cap: part_cap(&e.name),
            total_before: total,
            run: xml.then_some(&run),
            crc: flate2::Crc::new(),
        };
        if xml {
            has_content_types |= e.name == "[Content_Types].xml";
            scan_xml(&mut counted, &e.name, &run, &mut found)?;
        }
        // Anything after the root element still counts.
        std::io::copy(&mut counted, &mut std::io::sink()).map_err(io_err)?;
        if counted.produced != e.size || counted.crc.sum() != e.crc {
            return Err(bad("zip_header_mismatch", "The workbook's ZIP headers contradict each other."));
        }
        total += counted.produced;
    }

    let content_types = &found.content_types;
    if !has_content_types {
        return Err(not_a_workbook("it has no content types part"));
    }
    if content_types.iter().any(|t| MACRO_MAIN.contains(&t.as_str())) {
        return Err(macros());
    }
    if content_types.iter().any(|t| t == BINARY_MAIN) {
        return Err(binary_workbook());
    }
    if !content_types.iter().any(|t| SPREADSHEET_MAIN.contains(&t.as_str())) {
        return Err(not_a_workbook("it holds no spreadsheet"));
    }
    check_relationships(&found)
}

/// Opens the file with `zip`, the reader calamine uses, and refuses any
/// difference from what the preflight read: the same entries in the same
/// order, with the same names, offsets, sizes, CRCs and methods. Everything
/// the preflight checked is then what calamine reads.
fn cross_check<R: Read + Seek>(
    r: &mut R,
    dir: &Directory,
    entries: &[Entry],
    starts: &[u64],
) -> Result<(), ParseError> {
    let mismatch = || bad("zip_header_mismatch", "The workbook's ZIP headers contradict each other.");
    r.seek(SeekFrom::Start(0)).map_err(io_err)?;
    let mut archive = zip::ZipArchive::new(&mut *r).map_err(|e| match e {
        zip::result::ZipError::Io(e) => io_err(e),
        _ => mismatch(),
    })?;
    if archive.len() != entries.len() || archive.offset() != 0 || archive.central_directory_start() != dir.offset {
        return Err(mismatch());
    }
    for (i, (e, &data)) in entries.iter().zip(starts).enumerate() {
        let f = archive.by_index_raw(i).map_err(|_| mismatch())?;
        let method = match f.compression() {
            zip::CompressionMethod::Stored => 0,
            zip::CompressionMethod::Deflated => 8,
            _ => return Err(mismatch()),
        };
        if f.name() != e.name
            || f.name_raw() != e.name.as_bytes()
            || method != e.method
            || f.encrypted()
            || f.header_start() != e.local_offset
            || f.data_start() != Some(data)
            || f.compressed_size() != e.compressed
            || f.size() != e.size
            || f.crc32() != e.crc
        {
            return Err(mismatch());
        }
    }
    Ok(())
}

/// calamine finds the workbook through `_rels/.rels` and its sheets through
/// `xl/_rels/workbook.xml.rels`. The preflight's part caps and sheet checks
/// assume the usual places, so anything else is refused: the main part must
/// be `xl/workbook.xml`, and every sheet an `.xml` part in a sheet folder.
fn check_relationships(found: &Found) -> Result<(), ParseError> {
    let main: Vec<&Relationship> = found
        .package_rels
        .iter()
        .filter(|r| r.types.iter().any(|t| t.ends_with(b"/relationships/officeDocument")))
        .collect();
    if main.is_empty()
        || main.iter().any(|r| r.targets.iter().any(|t| t != b"xl/workbook.xml" && t != b"/xl/workbook.xml"))
    {
        return Err(not_a_workbook("its main part is not xl/workbook.xml"));
    }
    let is_sheet =
        |t: &Vec<u8>| matches!(t.rsplit(|&b| b == b'/').next(), Some(b"worksheet" | b"chartsheet" | b"dialogsheet"));
    for rel in found.workbook_rels.iter().filter(|r| r.types.iter().any(is_sheet)) {
        if !rel.targets.iter().all(|t| is_sheet_target(t)) {
            return Err(not_a_workbook("a sheet lies outside the worksheets folder"));
        }
    }
    Ok(())
}

/// A sheet target as calamine resolves it (`/x` from the root, anything else
/// under `xl/`): printable ASCII without `\`, `&` or spaces, in a sheet
/// folder, ending in `.xml`, and with no `.`, `..` or empty segment.
fn is_sheet_target(target: &[u8]) -> bool {
    let path = match target.strip_prefix(b"/") {
        Some(p) => p.to_vec(),
        None => [b"xl/".as_slice(), target].concat(),
    };
    let Ok(lower) = String::from_utf8(path.to_ascii_lowercase()) else { return false };
    lower.bytes().all(|b| (0x21..=0x7e).contains(&b) && b != b'\\' && b != b'&')
        && is_sheet_part(&lower)
        && lower.ends_with(".xml")
        && !lower.split('/').any(|s| s.is_empty() || s == "." || s == "..")
}

fn macros() -> ParseError {
    bad("macro_not_supported", "Workbooks with macros or ActiveX controls are not supported. Save the file as .xlsx.")
}

fn binary_workbook() -> ParseError {
    bad("unsupported_format", "Binary workbooks (.xlsb) are not supported. Save the file as .xlsx.")
}

/// A sheet of the workbook.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SheetInfo {
    pub name: String,
    pub hidden: bool,
}

/// Opens a file that passed [`preflight`] and lists its worksheets.
pub fn open<R: Read + Seek>(mut r: R) -> Result<(Xlsx<R>, Vec<SheetInfo>), ParseError> {
    r.seek(SeekFrom::Start(0)).map_err(io_err)?;
    let workbook: Xlsx<R> = Xlsx::new(r).map_err(calamine_error)?;
    let sheets = workbook
        .sheets_metadata()
        .iter()
        .filter(|s| s.typ == SheetType::WorkSheet)
        .map(|s| SheetInfo { name: s.name.clone(), hidden: s.visible != SheetVisible::Visible })
        .collect();
    Ok((workbook, sheets))
}

fn calamine_error(e: calamine::XlsxError) -> ParseError {
    match e {
        calamine::XlsxError::Io(e) => io_err(e),
        calamine::XlsxError::Password => ole_error(),
        _ => not_a_workbook("its content could not be read"),
    }
}

/// The text a cell holds before conversion, borrowed from the reader.
fn cell_text<'a>(v: &'a DataRef<'_>) -> Option<&'a str> {
    match v {
        DataRef::String(s) | DataRef::DateTimeIso(s) | DataRef::DurationIso(s) => Some(s),
        DataRef::SharedString(s) => Some(s),
        _ => None,
    }
}

fn cell_value(v: DataRef<'_>, has_formula: bool) -> CellValue {
    match v {
        DataRef::Empty if has_formula => CellValue::FormulaWithoutValue,
        DataRef::Empty => CellValue::Empty,
        DataRef::Int(i) => CellValue::Int(i),
        DataRef::Float(f) => CellValue::Float(f),
        DataRef::String(s) => CellValue::Text(s),
        DataRef::SharedString(s) => CellValue::Text(s.to_owned()),
        DataRef::Bool(b) => CellValue::Bool(b),
        DataRef::DateTime(dt) if dt.is_datetime() => match dt.as_datetime() {
            Some(t) if t.time() == chrono::NaiveTime::MIN => CellValue::Date(t.date()),
            Some(t) => CellValue::DateTime(t),
            None => CellValue::Float(dt.as_f64()),
        },
        DataRef::DateTime(dt) => CellValue::Float(dt.as_f64()),
        DataRef::DateTimeIso(s) => {
            if let Ok(d) = chrono::NaiveDate::parse_from_str(&s, "%Y-%m-%d") {
                CellValue::Date(d)
            } else if let Ok(t) = chrono::NaiveDateTime::parse_from_str(&s, "%Y-%m-%dT%H:%M:%S%.f") {
                CellValue::DateTime(t)
            } else {
                CellValue::Text(s)
            }
        }
        DataRef::DurationIso(s) => CellValue::Text(s),
        DataRef::Error(e) => CellValue::Error(e.to_string()),
    }
}

/// Reads one worksheet cell by cell and hands it over row by row (rows the
/// sheet leaves out are not produced). A non-empty cell at or beyond
/// `max_columns` stops the reading with `column_limit` before its row grows.
/// A cell's text is checked before it is copied: over `MAX_CELL_CHARS` is
/// `cell_too_long`, a row over [`MAX_ROW_TEXT_BYTES`] `record_too_long`, a
/// sheet over [`Limits::max_text_bytes`] `text_limit`.
pub fn read_sheet<R: Read + Seek>(
    workbook: &mut Xlsx<R>,
    sheet: &str,
    limits: &Limits,
    on_row: OnRow<'_>,
) -> Result<(), ParseError> {
    let mut cells = workbook.worksheet_cells_reader(sheet).map_err(calamine_error)?;
    let mut current: Option<Row> = None;
    let (mut text_row, mut row_bytes, mut sheet_bytes) = (0u32, 0usize, 0usize);
    let max_text = limits.max_text_bytes.min(MAX_SHEET_TEXT_BYTES);
    loop {
        let next = cells.next_cell_with_formula_metadata().map_err(calamine_error)?;
        let Some(cell) = next else { break };
        let (row, col) = cell.pos;
        let number = row.saturating_add(1);
        if current.as_ref().is_some_and(|r| r.number != number)
            && let Some(done) = current.take()
            && on_row(done).is_break()
        {
            return Ok(());
        }
        if let Some(text) = cell_text(&cell.value) {
            check_cell(text, number, col)?;
            if text_row != number {
                (text_row, row_bytes) = (number, 0);
            }
            row_bytes += text.len();
            sheet_bytes += text.len();
            if row_bytes > MAX_ROW_TEXT_BYTES {
                return Err(bad("record_too_long", "A row holds more than 1 MiB of text.").at(Some(number), None));
            }
            if sheet_bytes > max_text {
                return Err(text_limit(max_text).at(Some(number), None));
            }
        }
        let value = cell_value(cell.value, cell.formula.is_some());
        if value == CellValue::Empty {
            continue;
        }
        if col >= limits.max_columns {
            return Err(column_limit(limits, number, col));
        }
        let r = current.get_or_insert_with(|| Row { number, cells: Vec::new() });
        let col = col as usize;
        if r.cells.len() <= col {
            r.cells.resize(col + 1, CellValue::Empty);
        }
        r.cells[col] = value;
    }
    if let Some(done) = current {
        let _ = on_row(done);
    }
    Ok(())
}

/// The sheet holds more text than the import reads from one file.
fn text_limit(max: usize) -> ParseError {
    const MIB: usize = 1024 * 1024;
    let size = if max >= MIB && max.is_multiple_of(MIB) {
        format!("{} MiB", group_thousands((max / MIB) as u64))
    } else {
        format!("{} bytes", group_thousands(max as u64))
    };
    bad(
        "text_limit",
        format!(
            "The sheet holds more than {size} of text, counting each cell that refers to a shared string. \
             Split the file and import it in parts."
        ),
    )
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use std::ops::ControlFlow;

    use super::super::fixtures::{C, Part, sheet_xml, with_shared_strings, workbook, workbook_parts, zip};
    use super::*;

    const LIMITS: Limits = Limits { max_rows: 100_000, max_columns: 200, max_text_bytes: MAX_SHEET_TEXT_BYTES };

    fn check(bytes: Vec<u8>) -> Result<(), ParseError> {
        preflight(&mut Cursor::new(bytes))
    }

    #[track_caller]
    fn code(bytes: Vec<u8>) -> &'static str {
        check(bytes).expect_err("refused").code
    }

    fn read(bytes: Vec<u8>, sheet: Option<&str>) -> Result<(Vec<SheetInfo>, Vec<Row>), ParseError> {
        let mut c = Cursor::new(bytes);
        preflight(&mut c)?;
        let (mut wb, sheets) = open(c)?;
        let name = sheet.map(str::to_owned).unwrap_or_else(|| sheets[0].name.clone());
        let mut rows = Vec::new();
        read_sheet(&mut wb, &name, &LIMITS, &mut |r| {
            rows.push(r);
            ControlFlow::Continue(())
        })?;
        Ok((sheets, rows))
    }

    fn parts_with(extra: Part) -> Vec<u8> {
        let mut parts = workbook_parts(&sheet_xml(&[("A1", C::S("x"))]), None, false, false);
        parts.push(extra);
        zip(&parts)
    }

    #[test]
    fn a_plain_workbook_reads_typed_cells() {
        let bytes = workbook(&[
            ("A1", C::S("Hostname")),
            ("B1", C::S("Bought")),
            ("C1", C::S("Cores")),
            ("A2", C::S("web01")),
            ("B2", C::Date("45292")),
            ("C2", C::N("8")),
            ("D2", C::B(true)),
            ("E2", C::F("C2*2", Some("16"))),
            ("F2", C::F("C2*3", None)),
            ("G2", C::DateTime("45292.5")),
            ("A4", C::S("db01")),
        ]);
        let (sheets, rows) = read(bytes, None).unwrap();
        assert_eq!(sheets[0], SheetInfo { name: "Servers".into(), hidden: false });
        assert_eq!(rows.len(), 3, "row 3 is not in the sheet");
        assert_eq!(rows[1].number, 2);
        let cells = &rows[1].cells;
        assert_eq!(cells[0], CellValue::Text("web01".into()));
        assert_eq!(cells[1], CellValue::Date(chrono::NaiveDate::from_ymd_opt(2024, 1, 1).unwrap()));
        assert!(matches!(cells[2], CellValue::Float(f) if f == 8.0) || cells[2] == CellValue::Int(8));
        assert_eq!(cells[3], CellValue::Bool(true));
        assert_eq!(cells[4].display(), "16", "the cached value of a formula");
        assert_eq!(cells[5], CellValue::FormulaWithoutValue);
        assert_eq!(cells[6].display(), "2024-01-01T12:00:00");
        assert_eq!(rows[2].number, 4);
    }

    #[test]
    fn the_1904_date_system_and_hidden_sheets() {
        let parts =
            workbook_parts(&sheet_xml(&[("A1", C::Date("0"))]), Some(&sheet_xml(&[("A1", C::S("note"))])), true, true);
        let (sheets, rows) = read(zip(&parts), None).unwrap();
        assert_eq!(rows[0].cells[0], CellValue::Date(chrono::NaiveDate::from_ymd_opt(1904, 1, 1).unwrap()));
        assert_eq!(sheets[1], SheetInfo { name: "Notes".into(), hidden: true });
        let (_, notes) = read(zip(&parts), Some("Notes")).unwrap();
        assert_eq!(notes[0].cells[0], CellValue::Text("note".into()));
    }

    #[test]
    fn nul_through_an_escape_arrives_as_a_character() {
        let (_, rows) = read(workbook(&[("A1", C::S("a_x0000_b"))]), None).unwrap();
        assert_eq!(rows[0].cells[0], CellValue::Text("a\0b".into()), "refused later as invalid_character");
    }

    #[test]
    fn a_far_cell_is_refused_without_growing_a_row() {
        let err = read(workbook(&[("A1", C::S("x")), ("XFD1048576", C::S("far"))]), None).unwrap_err();
        assert_eq!((err.code, err.row, err.column), ("column_limit", Some(1_048_576), Some(16_383)));
        // GR is the 200th column, the last allowed; GS the first refused.
        assert!(read(workbook(&[("GR1", C::S("x"))]), None).is_ok());
        assert_eq!(read(workbook(&[("GS1", C::S("x"))]), None).unwrap_err().code, "column_limit");
    }

    /// A sheet whose row 1 is a header and whose row 2 refers to shared string 0 from `cells` columns.
    fn shared_row(strings: &[&str], cells: u32) -> Vec<u8> {
        let refs: Vec<String> = (0..cells).map(|c| format!("{}2", super::super::column_name(c))).collect();
        let mut sheet: Vec<(&str, C<'_>)> = vec![("A1", C::S("Hostname"))];
        sheet.extend(refs.iter().map(|r| (r.as_str(), C::Shared(0))));
        let mut parts = workbook_parts(&sheet_xml(&sheet), None, false, false);
        with_shared_strings(&mut parts, strings);
        zip(&parts)
    }

    #[test]
    fn a_long_shared_string_is_refused_before_it_is_copied_into_cells() {
        // GH#403: one 20,000-character string, referenced by 200 cells.
        let long = "x".repeat(20_000);
        let err = read(shared_row(&[&long], 200), None).unwrap_err();
        assert_eq!((err.code, err.row, err.column), ("cell_too_long", Some(2), Some(0)));
        assert!(!err.message.contains('x'), "the message never quotes the cell");
        // An inline string is held to the same limit.
        let err = read(workbook(&[("A1", C::S("h")), ("C3", C::S(&"y".repeat(10_001)))]), None).unwrap_err();
        assert_eq!((err.code, err.row, err.column), ("cell_too_long", Some(3), Some(2)));
        // 10,000 characters are accepted.
        let ok = "é".repeat(10_000);
        let (_, rows) = read(shared_row(&[&ok], 3), None).unwrap();
        assert_eq!(rows[1].cells.len(), 3);
        assert_eq!(rows[1].cells[2], CellValue::Text(ok.clone()));
    }

    #[test]
    fn rows_are_capped_in_text() {
        // 200 cells of 10,000 two-byte characters: each cell is allowed, the row is not.
        let wide = "é".repeat(10_000);
        let err = read(shared_row(&[&wide], 200), None).unwrap_err();
        assert_eq!((err.code, err.row), ("record_too_long", Some(2)));
        // 52 such cells (1,040,000 bytes) fit in a row.
        assert!(read(shared_row(&[&wide], 52), None).is_ok());
    }

    /// A sheet of `rows` rows, each referring to one shared string of
    /// 10,000 two-byte characters from 52 columns (1,040,000 bytes a row).
    fn wide_rows(rows: usize) -> Vec<u8> {
        let wide = "é".repeat(10_000);
        let mut refs = Vec::new();
        for r in 1..=rows {
            for c in 0..52u32 {
                refs.push(format!("{}{r}", super::super::column_name(c)));
            }
        }
        let sheet: Vec<(&str, C<'_>)> = refs.iter().map(|r| (r.as_str(), C::Shared(0))).collect();
        let mut parts = workbook_parts(&sheet_xml(&sheet), None, false, false);
        with_shared_strings(&mut parts, &[&wide]);
        zip(&parts)
    }

    fn read_with(bytes: Vec<u8>, limits: &Limits) -> (Result<(), ParseError>, u32) {
        let mut c = Cursor::new(bytes);
        preflight(&mut c).unwrap();
        let (mut wb, _) = open(c).unwrap();
        let mut seen = 0u32;
        let result = read_sheet(&mut wb, "Servers", limits, &mut |_| {
            seen += 1;
            ControlFlow::Continue(())
        });
        (result, seen)
    }

    #[test]
    fn a_sheet_holds_no_more_text_than_the_upload_limit() {
        // GH#446: with a 3 MiB upload limit, the fourth row of ~1 MiB crosses it,
        // although the file itself is a few kilobytes.
        let mib = 1024 * 1024;
        let limits = Limits::new(100_000, 200, 3 * mib);
        assert_eq!(limits.max_text_bytes, 3 * mib as usize);
        let file = wide_rows(5);
        assert!(file.len() < 64 * 1024, "the file is small: {} bytes", file.len());
        let (result, seen) = read_with(file, &limits);
        let err = result.unwrap_err();
        assert_eq!((err.code, err.row, seen), ("text_limit", Some(4), 3), "refused in the row that crosses the limit");
        assert!(err.message.contains("3 MiB"), "names the configured limit: {}", err.message);
        // Three rows fit.
        let (result, seen) = read_with(wide_rows(3), &limits);
        assert!(result.is_ok() && seen == 3);
        // The default upload limit (50 MiB) is the cap, not the 256 MiB ceiling.
        assert_eq!(Limits::new(1, 1, 50 * mib).max_text_bytes, 50 * mib as usize);
        // The ceiling holds whatever the configuration says.
        assert_eq!(Limits::new(1, 1, u64::MAX).max_text_bytes, MAX_SHEET_TEXT_BYTES);
    }

    #[test]
    fn cell_references_are_checked_before_calamine_parses_them() {
        for ok in ["A1", "XFD1048576", "$A$1:$C$9", "A:A", "1:1", "A1 B2:C3", ""] {
            assert!(is_cell_reference(ok.as_bytes()), "{ok}");
        }
        for bad in ["AAAA1", "A12345678", "A1B", "Adimension", "A-1", "A1;B2", "Q1:C2", "A9:A1", "A1:B2:C3"] {
            assert!(!is_cell_reference(bad.as_bytes()), "{bad}");
        }
        let sheet = r#"<worksheet><dimension ref="Adimension"/><sheetData/></worksheet>"#;
        let mut parts = workbook_parts(sheet, None, false, false);
        parts.truncate(parts.len() - 1);
        parts.push(Part::new("xl/worksheets/sheet2.xml", sheet_xml(&[])));
        assert_eq!(code(zip(&parts)), "not_a_workbook");
    }

    #[test]
    fn magic_bytes() {
        assert_eq!(code(vec![0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1, 0, 0]), "workbook_encrypted_or_xls");
        assert_eq!(code(b"Name,Host\n".to_vec()), "not_a_workbook");
    }

    #[test]
    fn macros_activex_and_binary_workbooks() {
        assert_eq!(code(parts_with(Part::new("xl/vbaProject.bin", vec![1u8; 10]))), "macro_not_supported");
        assert_eq!(code(parts_with(Part::new("xl/activeX/activeX1.xml", "<ax/>"))), "macro_not_supported");
        assert_eq!(code(parts_with(Part::new("xl/workbook.bin", vec![1u8; 10]))), "unsupported_format");
        let mut parts = workbook_parts(&sheet_xml(&[]), None, false, false);
        parts[0].data = super::super::fixtures::CONTENT_TYPES
            .replace("officedocument.spreadsheetml.sheet.main+xml", "")
            .replace(
                "application/vnd.openxmlformats-",
                "application/vnd.ms-excel.sheet.macroEnabled.main+xml\" X=\"application/vnd.openxmlformats-",
            )
            .into_bytes();
        assert_eq!(code(zip(&parts)), "macro_not_supported", ".xlsm content type");
    }

    #[test]
    fn xml_doctype_and_encodings() {
        let doctype = r#"<?xml version="1.0"?><!DOCTYPE x [<!ENTITY a "aaaa">]><x>&a;</x>"#;
        assert_eq!(code(parts_with(Part::new("xl/extra.xml", doctype))), "xml_doctype");
        // UTF-16 with a DOCTYPE: a byte search for "<!DOCTYPE" would miss it.
        let utf16: Vec<u8> =
            [0xFF, 0xFE].into_iter().chain(doctype.encode_utf16().flat_map(|u| u.to_le_bytes())).collect();
        let c = code(parts_with(Part::new("xl/extra.xml", utf16.clone())));
        assert!(c == "xml_encoding" || c == "xml_doctype", "{c}");
        // Without the BOM as well.
        assert_eq!(code(parts_with(Part::new("xl/extra.xml", utf16[2..].to_vec()))), "xml_encoding");
        let latin = r#"<?xml version="1.0" encoding="ISO-8859-1"?><x/>"#;
        assert_eq!(code(parts_with(Part::new("xl/extra.xml", latin))), "xml_encoding");
    }

    #[test]
    fn zip_structure() {
        // 10,000 entries pass, 10,001 do not (refused before reading any entry).
        let mut many = workbook_parts(&sheet_xml(&[]), None, false, false);
        let base = many.len();
        for i in 0..(10_000 - base) {
            let mut p = Part::new(&format!("xl/media/p{i}.bin"), vec![]);
            p.method = 0;
            many.push(p);
        }
        assert!(check(zip(&many)).is_ok());
        let mut p = Part::new("xl/media/one-more.bin", vec![]);
        p.method = 0;
        many.push(p);
        assert_eq!(code(zip(&many)), "zip_entries");

        // Names differing only in case.
        assert_eq!(code(parts_with(Part::new("XL/WORKBOOK.XML", "<x/>"))), "zip_duplicate_entry");
        assert_eq!(code(parts_with(Part::new("xl/../evil.xml", "<x/>"))), "zip_entry_name");
        assert_eq!(code(parts_with(Part::new("/etc/evil.xml", "<x/>"))), "zip_entry_name");

        // Local header disagreeing with the central directory.
        let mut p = Part::new("xl/extra.xml", "<x/>");
        p.local_name = Some("xl/other.xml".into());
        assert_eq!(code(parts_with(p)), "zip_header_mismatch");
        let mut p = Part::new("xl/extra.xml", "<x/>");
        p.local_crc = Some(1);
        assert_eq!(code(parts_with(p)), "zip_header_mismatch");
        let mut p = Part::new("xl/extra.xml", "<x/>");
        p.local_method = Some(0);
        assert_eq!(code(parts_with(p)), "zip_header_mismatch");

        // Compression other than stored or deflate (bzip2 = 12).
        let mut p = Part::new("xl/extra.bin", vec![1u8; 10]);
        p.method = 12;
        assert_eq!(code(parts_with(p)), "zip_compression");
        // Encrypted entry.
        let mut p = Part::new("xl/extra.bin", vec![1u8; 10]);
        p.flags = 1;
        assert_eq!(code(parts_with(p)), "workbook_encrypted_or_xls");
    }

    #[test]
    fn zip_bombs_and_part_caps() {
        // 300 MiB of zeros compress about 1,000:1 ... beyond 1,000:1 is refused.
        let bomb = vec![0u8; 20 * 1024 * 1024];
        assert_eq!(code(parts_with(Part::new("xl/media/bomb.bin", bomb))), "zip_bomb");
        // A 17 MiB styles.xml (above the 16 MiB cap), not compressible enough to be a bomb.
        let mut parts = workbook_parts(&sheet_xml(&[]), None, false, false);
        let mut styles = String::from("<styleSheet>");
        let mut seed: u64 = 1;
        while styles.len() < 17 * 1024 * 1024 {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            styles.push_str(&format!("<x a=\"{seed:x}\"/>"));
        }
        styles.push_str("</styleSheet>");
        let i = parts.iter().position(|p| p.name == "xl/styles.xml").unwrap();
        parts[i] = Part::new("xl/styles.xml", styles);
        assert_eq!(code(zip(&parts)), "part_too_large");
        // A single oversized XML text run.
        let long = format!("<x>{}</x>", "a".repeat((MAX_XML_TOKEN + 1) as usize));
        let c = code(parts_with(Part::new("xl/extra.xml", long)));
        assert!(c == "xml_token_too_large" || c == "zip_bomb", "{c}");
    }

    /// Stored, so the ratio check stays out of the way.
    fn stored(name: &str, data: String) -> Part {
        let mut p = Part::new(name, data);
        p.method = 0;
        p
    }

    #[test]
    fn an_event_is_capped_whatever_it_contains() {
        // One start tag: an attribute value over 4 MiB with a `>` every KiB. (The
        // count is exact to one 64 KiB read, so 4 MiB + 1 KiB could still pass.)
        let kib = (MAX_XML_TOKEN / 1024 + 128) as usize;
        let chunk = format!("{}>", "a".repeat(1023));
        let value = chunk.repeat(kib);
        let tag = format!(r#"<worksheet><sheetData><row r="1" x="{value}"/></sheetData></worksheet>"#);
        let mut parts = workbook_parts(&sheet_xml(&[]), None, false, false);
        let i = parts.iter().position(|p| p.name == "xl/worksheets/sheet1.xml").unwrap();
        parts[i] = stored("xl/worksheets/sheet1.xml", tag);
        assert_eq!(code(zip(&parts)), "xml_token_too_large");
        // CDATA and comments with a `<` every KiB.
        let lt = format!("{}<", "a".repeat(1023)).repeat(kib);
        for xml in [format!("<x><![CDATA[{lt}]]></x>"), format!("<x><!--{}--></x>", lt.replace('-', "_"))] {
            assert_eq!(code(parts_with(stored("xl/extra.xml", xml))), "xml_token_too_large");
        }
        // Many events adding up to more than the cap are fine.
        let many = format!("<x>{}</x>", "<y>aaaa</y>".repeat((MAX_XML_TOKEN / 8) as usize));
        assert!(check(parts_with(stored("xl/extra.xml", many))).is_ok());
    }

    #[test]
    fn the_end_record_must_end_the_file() {
        let good = workbook(&[("A1", C::S("x"))]);
        assert!(check(good.clone()).is_ok());
        // A comment after the end record.
        let mut commented = good.clone();
        let n = commented.len();
        commented[n - 2..].copy_from_slice(&4u16.to_le_bytes());
        commented.extend_from_slice(b"note");
        assert_eq!(code(commented), "not_a_workbook");
        // A second end record in the comment of the first, whose own comment
        // length does not fit (zip skips it and falls back to the first).
        let mut hidden = good.clone();
        let mut fake = hidden[n - 22..].to_vec();
        fake[20..].copy_from_slice(&1u16.to_le_bytes());
        hidden[n - 2..].copy_from_slice(&23u16.to_le_bytes());
        hidden.extend_from_slice(&fake);
        hidden.push(b'x');
        assert_eq!(code(hidden), "not_a_workbook");
        // Bytes after the end record.
        let mut trailing = good;
        trailing.extend_from_slice(&[0; 4]);
        assert_eq!(code(trailing), "not_a_workbook");
    }

    /// `bytes` rewritten with a ZIP64 end: `gap` bytes between the ZIP64
    /// record and its locator.
    fn as_zip64(bytes: &[u8], gap: usize) -> Vec<u8> {
        let n = bytes.len();
        let eocd = &bytes[n - 22..];
        let entries = u16_at(eocd, 10) as u64;
        let (size, offset) = (u32_at(eocd, 12) as u64, u32_at(eocd, 16) as u64);
        let mut out = bytes[..n - 22].to_vec();
        let rec_offset = out.len() as u64;
        out.extend_from_slice(&0x0606_4b50u32.to_le_bytes());
        out.extend_from_slice(&44u64.to_le_bytes());
        out.extend_from_slice(&[45, 0, 45, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        for v in [entries, entries, size, offset] {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.extend(std::iter::repeat_n(0u8, gap));
        out.extend_from_slice(&0x0706_4b50u32.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&rec_offset.to_le_bytes());
        out.extend_from_slice(&1u32.to_le_bytes());
        let mut end = eocd.to_vec();
        end[8..12].copy_from_slice(&[0xFF; 4]);
        out.extend_from_slice(&end);
        out
    }

    #[test]
    fn a_zip64_record_must_sit_right_before_its_locator() {
        let good = workbook(&[("A1", C::S("x"))]);
        assert!(read(as_zip64(&good, 0), None).is_ok());
        assert_eq!(code(as_zip64(&good, 4)), "not_a_workbook");
    }

    #[test]
    fn part_names_are_printable_ascii() {
        for name in ["xl\\worksheets\\sheet3.xml", "xl/caf\u{e9}.xml", "xl/a\tb.xml"] {
            assert_eq!(code(parts_with(Part::new(name, "<x/>"))), "zip_entry_name", "{name:?}");
        }
        // The Info-ZIP Unicode path field would give `zip` another name.
        for id in [0x7075u16, 0x6375] {
            let mut p = Part::new("xl/extra.xml", "<x/>");
            p.extra = [&id.to_le_bytes()[..], &5u16.to_le_bytes(), &[1, 0, 0, 0, 0]].concat();
            assert_eq!(code(parts_with(p)), "zip_entry_name", "{id:#x}");
        }
        // A space is printable and allowed.
        assert!(check(parts_with(Part::new("xl/media/image 1.png", vec![1u8; 10]))).is_ok());
    }

    #[test]
    fn the_cross_check_refuses_any_difference_from_zip() {
        let bytes = workbook(&[("A1", C::S("x"))]);
        let mut r = Cursor::new(bytes);
        let len = r.get_ref().len() as u64;
        let dir = find_directory(&mut r, len).unwrap();
        let entries = read_directory(&mut r, &dir).unwrap();
        let starts: Vec<u64> = entries.iter().map(|e| check_local_header(&mut r, e, len).unwrap()).collect();
        assert!(cross_check(&mut r, &dir, &entries, &starts).is_ok());
        type Change = fn(&mut Vec<Entry>, &mut Vec<u64>);
        let changes: [Change; 6] = [
            |e, _| e[1].name = "_rels/other.rels".into(),
            |e, _| e[1].local_offset += 1,
            |_, s| s[1] += 1,
            |e, _| e[1].size += 1,
            |e, _| e[1].crc ^= 1,
            |e, s| {
                e.pop();
                s.pop();
            },
        ];
        for (i, change) in changes.iter().enumerate() {
            let (mut e, mut s) = (entries.clone(), starts.clone());
            change(&mut e, &mut s);
            assert_eq!(cross_check(&mut r, &dir, &e, &s).unwrap_err().code, "zip_header_mismatch", "change {i}");
        }
    }

    #[test]
    fn the_workbook_and_its_sheets_must_be_where_the_preflight_looked() {
        use super::super::fixtures::{RELS, WORKBOOK_RELS};
        let with = |name: &str, data: String| {
            let mut parts = workbook_parts(&sheet_xml(&[("A1", C::S("x"))]), None, false, false);
            let i = parts.iter().position(|p| p.name == name).unwrap();
            parts[i] = Part::new(name, data);
            zip(&parts)
        };
        let package = |target: &str| with("_rels/.rels", RELS.replace("\"xl/workbook.xml\"", &format!("\"{target}\"")));
        assert!(read(package("/xl/workbook.xml"), None).is_ok());
        for target in ["xl2/workbook.xml", "XL/workbook.xml", "workbook.xml"] {
            assert_eq!(code(package(target)), "not_a_workbook", "{target}");
        }
        // No officeDocument relationship at all.
        let no_main = with("_rels/.rels", RELS.replace("officeDocument\"", "other\""));
        assert_eq!(code(no_main), "not_a_workbook");

        let sheet = |target: &str| {
            with(
                "xl/_rels/workbook.xml.rels",
                WORKBOOK_RELS.replace("\"worksheets/sheet1.xml\"", &format!("\"{target}\"")),
            )
        };
        assert!(read(sheet("/xl/worksheets/sheet1.xml"), None).is_ok());
        for target in [
            "sharedStrings.xml",
            "worksheets/../sharedStrings.xml",
            "worksheets/./sheet1.xml",
            "worksheets//sheet1.xml",
            "worksheets/sheet1.bin",
            "worksheets\\sheet1.xml",
            "worksheets/a&amp;b.xml",
            "/xl/media/sheet1.xml",
            "../xl/worksheets/sheet1.xml",
        ] {
            assert_eq!(code(sheet(target)), "not_a_workbook", "{target}");
        }
    }
}
