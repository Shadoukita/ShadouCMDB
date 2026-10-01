//! CSV reader (§5.2): RFC 4180 quoting, UTF-8 (a BOM is stripped),
//! Windows-1252 or ISO-8859-1, and the delimiter `,` `;` tab or `|`.
//!
//! The tokenizer is `csv-core` with buffers this module sizes itself, so one
//! record never takes more than [`MAX_RECORD_BYTES`] (an unterminated quote
//! cannot swallow the file) and never more than `max_columns` fields. Bytes
//! that are invalid in the chosen encoding are an error with their offset;
//! nothing is replaced. NUL bytes are refused.

use std::io::Read;

use csv_core::{ReadRecordResult, ReaderBuilder};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::{CellValue, Limits, OnRow, ParseError, Row, column_limit};

/// Most bytes of one record, after decoding.
pub const MAX_RECORD_BYTES: usize = 1024 * 1024;
const BLOCK: usize = 64 * 1024;
/// Delimiters the detection chooses from, in order of preference on a tie.
pub const DELIMITERS: [u8; 4] = *b",;\t|";

/// A CSV file's text encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[schema(as = ImportEncoding)]
pub enum Encoding {
    #[serde(rename = "utf-8")]
    Utf8,
    #[serde(rename = "windows-1252")]
    Windows1252,
    #[serde(rename = "iso-8859-1")]
    Iso88591,
}

/// What the reader found or was told.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Detected {
    pub encoding: Encoding,
    pub delimiter: u8,
    /// The file started with a UTF-8 byte order mark.
    pub bom: bool,
}

/// The encoding a file announces with a byte order mark: UTF-8, or an error
/// for UTF-16/32, which the importer does not read.
pub fn bom_encoding(head: &[u8]) -> Result<Option<Encoding>, ParseError> {
    if head.starts_with(&[0xEF, 0xBB, 0xBF]) {
        return Ok(Some(Encoding::Utf8));
    }
    if head.starts_with(&[0xFF, 0xFE]) || head.starts_with(&[0xFE, 0xFF]) {
        return Err(ParseError::new(
            "unsupported_encoding",
            "The file is UTF-16 encoded. Save it as CSV UTF-8, Windows-1252 or ISO-8859-1.",
        ));
    }
    Ok(None)
}

/// Turns bytes of the file's encoding into UTF-8, block by block.
struct Decoder {
    encoding: Encoding,
    /// Bytes of an incomplete UTF-8 sequence at the end of the last block.
    carry: Vec<u8>,
    /// Offset in the file of the next byte to decode.
    offset: u64,
    /// Windows-1252 bytes 0x80..=0xFF; None for the five it leaves undefined.
    high: [Option<char>; 128],
    bom_checked: bool,
    bom: bool,
}

fn invalid_byte(offset: u64, encoding: Encoding) -> ParseError {
    let name = match encoding {
        Encoding::Utf8 => "UTF-8",
        Encoding::Windows1252 => "Windows-1252",
        Encoding::Iso88591 => "ISO-8859-1",
    };
    ParseError::new(
        "invalid_encoding",
        format!(
            "The file is not valid {name}: invalid byte at offset {offset}. Choose the encoding the file was saved in."
        ),
    )
}

fn nul_byte(offset: u64) -> ParseError {
    ParseError::new("invalid_character", format!("The file contains a NUL byte at offset {offset}."))
}

impl Decoder {
    fn new(encoding: Encoding) -> Self {
        let mut high = [None; 128];
        if encoding == Encoding::Windows1252 {
            for (i, slot) in high.iter_mut().enumerate() {
                let byte = [0x80 + i as u8];
                let (text, had_errors) = encoding_rs::WINDOWS_1252.decode_without_bom_handling(&byte);
                let c = text.chars().next();
                // encoding_rs maps the five undefined bytes to C1 controls; they are refused.
                *slot = c.filter(|c| !had_errors && !('\u{80}'..='\u{9f}').contains(c));
            }
        }
        Decoder { encoding, carry: Vec::new(), offset: 0, high, bom_checked: false, bom: false }
    }

    fn decode(&mut self, block: &[u8], out: &mut Vec<u8>) -> Result<(), ParseError> {
        let mut data: &[u8] = block;
        let joined;
        if !self.carry.is_empty() {
            joined = [self.carry.as_slice(), block].concat();
            self.offset -= self.carry.len() as u64;
            self.carry.clear();
            data = &joined;
        }
        if !self.bom_checked {
            if data.len() < 3 && !data.is_empty() && [0xEF, 0xBB, 0xBF].starts_with(data) {
                // Not enough bytes to tell yet.
                self.carry.extend_from_slice(data);
                self.offset += data.len() as u64;
                return Ok(());
            }
            self.bom_checked = true;
            if bom_encoding(data)?.is_some() {
                self.bom = true;
                data = &data[3..];
                self.offset += 3;
            }
        }
        if let Some(i) = memchr_nul(data) {
            return Err(nul_byte(self.offset + i as u64));
        }
        match self.encoding {
            Encoding::Utf8 => match std::str::from_utf8(data) {
                Ok(_) => out.extend_from_slice(data),
                Err(e) => {
                    let valid = e.valid_up_to();
                    if e.error_len().is_some() {
                        return Err(invalid_byte(self.offset + valid as u64, self.encoding));
                    }
                    out.extend_from_slice(&data[..valid]);
                    self.carry.extend_from_slice(&data[valid..]);
                }
            },
            Encoding::Windows1252 => {
                let mut buf = [0u8; 4];
                for (i, &b) in data.iter().enumerate() {
                    if b < 0x80 {
                        out.push(b);
                    } else {
                        let c = self.high[(b - 0x80) as usize]
                            .ok_or_else(|| invalid_byte(self.offset + i as u64, self.encoding))?;
                        out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
                    }
                }
            }
            Encoding::Iso88591 => {
                let mut buf = [0u8; 4];
                for &b in data {
                    out.extend_from_slice(char::from(b).encode_utf8(&mut buf).as_bytes());
                }
            }
        }
        self.offset += data.len() as u64;
        Ok(())
    }

    fn finish(&mut self) -> Result<(), ParseError> {
        if self.carry.is_empty() {
            return Ok(());
        }
        if !self.bom_checked {
            // A file shorter than a BOM.
            self.bom_checked = true;
            let rest = std::mem::take(&mut self.carry);
            self.offset -= rest.len() as u64;
            let mut out = Vec::new();
            return self.decode(&rest, &mut out).and_then(|_| {
                if self.carry.is_empty() { Ok(()) } else { Err(invalid_byte(self.offset, self.encoding)) }
            });
        }
        Err(invalid_byte(self.offset - self.carry.len() as u64, self.encoding))
    }
}

/// Puts a delimiter into every blank line outside quotes. csv-core skips blank
/// lines, but a spreadsheet counts them as rows, and row numbers must match
/// what the user sees. The blank record this makes is skipped by the caller.
///
/// Quotes are followed as csv-core reads them: a `"` opens a quoted field
/// only at the start of a field (elsewhere it is data), and `""` inside one
/// is an escaped quote.
struct BlankLines {
    delimiter: u8,
    field: Field,
    at_line_start: bool,
    prev_cr: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Field {
    Start,
    Unquoted,
    Quoted,
    /// A `"` inside a quoted field: the end of the quotes, or half of `""`.
    QuoteInQuoted,
}

impl BlankLines {
    fn new(delimiter: u8) -> Self {
        BlankLines { delimiter, field: Field::Start, at_line_start: true, prev_cr: false }
    }

    fn transform(&mut self, input: &[u8], out: &mut Vec<u8>) {
        for &b in input {
            let line_end = (b == b'\r' || b == b'\n') && self.field != Field::Quoted;
            if line_end && self.at_line_start && !(b == b'\n' && self.prev_cr) {
                out.push(self.delimiter);
            }
            self.field = match (self.field, b) {
                (Field::Quoted, b'"') => Field::QuoteInQuoted,
                (Field::Quoted, _) => Field::Quoted,
                (Field::Start | Field::QuoteInQuoted, b'"') => Field::Quoted,
                _ if line_end || b == self.delimiter => Field::Start,
                _ => Field::Unquoted,
            };
            self.at_line_start = line_end;
            self.prev_cr = line_end && b == b'\r';
            out.push(b);
        }
    }
}

fn memchr_nul(data: &[u8]) -> Option<usize> {
    data.iter().position(|&b| b == 0)
}

/// The delimiter of a header line: the candidate found most often outside
/// quotes; `,` when there is none or on a tie with it.
pub fn detect_delimiter(text: &[u8]) -> u8 {
    let mut counts = [0usize; 4];
    let mut quoted = false;
    for &b in text {
        match b {
            b'"' => quoted = !quoted,
            b'\n' | b'\r' if !quoted => break,
            _ if !quoted => {
                if let Some(i) = DELIMITERS.iter().position(|d| *d == b) {
                    counts[i] += 1;
                }
            }
            _ => {}
        }
    }
    let mut best = 0;
    for i in 1..DELIMITERS.len() {
        if counts[i] > counts[best] {
            best = i;
        }
    }
    DELIMITERS[best]
}

/// Reads every record and hands it to `on_row` (blank rows included; the
/// caller decides). `delimiter` None detects it from the first line.
pub fn read<R: Read>(
    mut input: R,
    encoding: Encoding,
    delimiter: Option<u8>,
    limits: &Limits,
    on_row: OnRow<'_>,
) -> Result<Detected, ParseError> {
    let mut decoder = Decoder::new(encoding);
    let mut block = vec![0u8; BLOCK];
    let mut raw: Vec<u8> = Vec::with_capacity(BLOCK);
    let mut eof = false;

    let mut fill = |text: &mut Vec<u8>, decoder: &mut Decoder, eof: &mut bool| -> Result<(), ParseError> {
        loop {
            let n = read_some(&mut input, &mut block)?;
            if n == 0 {
                decoder.finish()?;
                *eof = true;
                return Ok(());
            }
            let before = text.len();
            decoder.decode(&block[..n], text)?;
            if text.len() > before {
                return Ok(());
            }
        }
    };

    // The first line decides the delimiter: read until it is complete (or 1 MiB).
    let delimiter = match delimiter {
        Some(d) => d,
        None => {
            while !eof && !raw.contains(&b'\n') && !raw.contains(&b'\r') && raw.len() < MAX_RECORD_BYTES {
                fill(&mut raw, &mut decoder, &mut eof)?;
            }
            detect_delimiter(&raw)
        }
    };
    let mut blanks = BlankLines::new(delimiter);
    let mut text: Vec<u8> = Vec::with_capacity(BLOCK + 64);
    blanks.transform(&raw, &mut text);
    raw.clear();

    let mut reader = ReaderBuilder::new().delimiter(delimiter).build();
    let mut out = vec![0u8; BLOCK];
    let mut ends = vec![0usize; 32];
    let (mut out_len, mut ends_len) = (0usize, 0usize);
    let mut pos = 0usize;
    let mut number: u32 = 0;
    let max_ends = limits.max_columns as usize + 1;

    loop {
        if pos == text.len() && !eof {
            text.clear();
            pos = 0;
            fill(&mut raw, &mut decoder, &mut eof)?;
            blanks.transform(&raw, &mut text);
            raw.clear();
            continue;
        }
        let (result, n_in, n_out, n_ends) =
            reader.read_record(&text[pos..], &mut out[out_len..], &mut ends[ends_len..]);
        pos += n_in;
        out_len += n_out;
        ends_len += n_ends;
        match result {
            ReadRecordResult::InputEmpty => {
                if eof && pos == text.len() {
                    // Tells csv-core the input ended, which completes a last record without a line end.
                    let (result, _, n_out, n_ends) =
                        reader.read_record(&[], &mut out[out_len..], &mut ends[ends_len..]);
                    out_len += n_out;
                    ends_len += n_ends;
                    match result {
                        ReadRecordResult::Record => {
                            number += 1;
                            if emit(number, &out[..out_len], &ends[..ends_len], limits, on_row)?.is_break() {
                                break;
                            }
                            out_len = 0;
                            ends_len = 0;
                            continue;
                        }
                        ReadRecordResult::End => break,
                        ReadRecordResult::OutputFull | ReadRecordResult::OutputEndsFull => {
                            grow(&mut out, &mut ends, result, number + 1, limits, max_ends)?;
                            continue;
                        }
                        ReadRecordResult::InputEmpty => break,
                    }
                }
            }
            ReadRecordResult::OutputFull | ReadRecordResult::OutputEndsFull => {
                grow(&mut out, &mut ends, result, number + 1, limits, max_ends)?;
            }
            ReadRecordResult::Record => {
                number += 1;
                if emit(number, &out[..out_len], &ends[..ends_len], limits, on_row)?.is_break() {
                    break;
                }
                out_len = 0;
                ends_len = 0;
            }
            ReadRecordResult::End => break,
        }
    }
    Ok(Detected { encoding, delimiter, bom: decoder.bom })
}

/// Whether the whole file is valid UTF-8 (a BOM allowed). Used to pick the
/// encoding when none is chosen; a NUL byte counts as valid here and is
/// reported by [`read`].
pub fn is_utf8<R: Read>(mut input: R) -> Result<bool, ParseError> {
    let mut decoder = Decoder::new(Encoding::Utf8);
    let mut block = vec![0u8; BLOCK];
    let mut sink = Vec::with_capacity(BLOCK + 4);
    loop {
        let n = read_some(&mut input, &mut block)?;
        let result = if n == 0 { decoder.finish() } else { decoder.decode(&block[..n], &mut sink) };
        match result {
            Ok(()) if n == 0 => return Ok(true),
            Ok(()) => sink.clear(),
            Err(e) if e.code == "invalid_encoding" => return Ok(false),
            Err(e) if e.code == "invalid_character" => return Ok(true),
            Err(e) => return Err(e),
        }
    }
}

fn read_some<R: Read>(input: &mut R, buf: &mut [u8]) -> Result<usize, ParseError> {
    loop {
        match input.read(buf) {
            Ok(n) => return Ok(n),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(ParseError::read_failed(&e)),
        }
    }
}

fn grow(
    out: &mut Vec<u8>,
    ends: &mut Vec<usize>,
    result: ReadRecordResult,
    row: u32,
    limits: &Limits,
    max_ends: usize,
) -> Result<(), ParseError> {
    if result == ReadRecordResult::OutputFull {
        if out.len() >= MAX_RECORD_BYTES {
            return Err(ParseError::new(
                "record_too_long",
                "A record is longer than 1 MiB. Check for a quote that is not closed.",
            )
            .at(Some(row), None));
        }
        out.resize((out.len() * 2).min(MAX_RECORD_BYTES), 0);
    } else {
        if ends.len() >= max_ends {
            return Err(column_limit(limits, row, limits.max_columns));
        }
        ends.resize((ends.len() * 2).min(max_ends), 0);
    }
    Ok(())
}

fn emit(
    number: u32,
    out: &[u8],
    ends: &[usize],
    limits: &Limits,
    on_row: OnRow<'_>,
) -> Result<super::Flow, ParseError> {
    if ends.len() > limits.max_columns as usize {
        return Err(column_limit(limits, number, limits.max_columns));
    }
    let mut cells = Vec::with_capacity(ends.len());
    let mut start = 0;
    for &end in ends {
        let field = std::str::from_utf8(&out[start..end])
            .map_err(|_| ParseError::new("invalid_encoding", "The file is not valid UTF-8.").at(Some(number), None))?;
        cells.push(if field.is_empty() { CellValue::Empty } else { CellValue::Text(field.to_owned()) });
        start = end;
    }
    Ok(on_row(Row { number, cells }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ops::ControlFlow;

    const LIMITS: Limits = Limits { max_rows: 100, max_columns: 200 };

    fn rows(
        bytes: &[u8],
        encoding: Encoding,
        delimiter: Option<u8>,
    ) -> Result<(Detected, Vec<Vec<String>>), ParseError> {
        let mut out = Vec::new();
        let detected = read(bytes, encoding, delimiter, &LIMITS, &mut |r: Row| {
            out.push(r.cells.iter().map(CellValue::display).collect());
            ControlFlow::Continue(())
        })?;
        Ok((detected, out))
    }

    #[test]
    fn quoting_newlines_and_the_bom() {
        let (d, r) =
            rows(b"\xEF\xBB\xBFName;Notes\r\nweb01;\"two\nlines; and \"\"quotes\"\"\"\nweb02;", Encoding::Utf8, None)
                .unwrap();
        assert_eq!((d.delimiter, d.bom), (b';', true));
        assert_eq!(r, [vec!["Name", "Notes"], vec!["web01", "two\nlines; and \"quotes\""], vec!["web02", ""]]);
    }

    #[test]
    fn delimiters_are_detected_from_the_header() {
        for (d, text) in [
            (b',', "a,b;c"),
            (b';', "a;b;c,d"),
            (b'\t', "a\tb\tc"),
            (b'|', "a|b|c"),
            (b',', "abc"),
            (b';', "\"x,y\";b"),
        ] {
            assert_eq!(detect_delimiter(text.as_bytes()), d, "{text}");
        }
    }

    #[test]
    fn encodings() {
        // Windows-1252: € is 0x80, ü is 0xFC.
        let (_, r) = rows(b"Name\n\x80 M\xFCller", Encoding::Windows1252, Some(b',')).unwrap();
        assert_eq!(r[1], ["€ Müller"]);
        let (_, r) = rows(b"Name\nM\xFCller \x80", Encoding::Iso88591, Some(b',')).unwrap();
        assert_eq!(r[1], ["Müller \u{80}"]);
        // Invalid UTF-8 names its offset; nothing is replaced.
        let err = rows(b"Name\nM\xFCller", Encoding::Utf8, None).unwrap_err();
        assert_eq!((err.code, err.message.contains("offset 6")), ("invalid_encoding", true), "{err}");
        // A byte Windows-1252 leaves undefined.
        assert_eq!(rows(b"a\n\x81", Encoding::Windows1252, None).unwrap_err().code, "invalid_encoding");
        // UTF-16 is refused.
        assert_eq!(rows(b"\xFF\xFEa\x00", Encoding::Utf8, None).unwrap_err().code, "unsupported_encoding");
        // A multi-byte character split over the read blocks is joined again.
        let mut long = "x".repeat(BLOCK - 1).into_bytes();
        long.extend_from_slice("ü\n".as_bytes());
        let (_, r) = rows(&long, Encoding::Utf8, Some(b',')).unwrap();
        assert!(r[0][0].ends_with('ü'));
        // A file cut inside a character.
        assert_eq!(rows(b"a\n\xC3", Encoding::Utf8, None).unwrap_err().code, "invalid_encoding");
    }

    #[test]
    fn row_numbers_count_blank_lines_like_a_spreadsheet() {
        let mut numbers = Vec::new();
        read(&b"h\r\na\r\n\r\n\"x\ny\"\n\nb\n\n"[..], Encoding::Utf8, None, &LIMITS, &mut |r: Row| {
            if !r.is_blank() {
                numbers.push((r.number, r.cells[0].display()));
            }
            ControlFlow::Continue(())
        })
        .unwrap();
        assert_eq!(numbers, [(1, "h".into()), (2, "a".into()), (4, "x\ny".into()), (6, "b".into())]);
    }

    #[test]
    fn a_quote_inside_an_unquoted_field_is_data() {
        // `a"b` opens no quotes, so the blank line inside "c\n\nd" stays as it is.
        let (_, r) = rows(b"h\na\"b,\"c\n\nd\"\n", Encoding::Utf8, Some(b',')).unwrap();
        assert_eq!(r, [vec!["h"], vec!["a\"b", "c\n\nd"]]);
        // Escaped quotes and a blank line after them still count as a row.
        let mut numbers = Vec::new();
        read(&b"h\na\"b\n\n\"x\"\"\ny\"\n\r\n\r\nz\n"[..], Encoding::Utf8, None, &LIMITS, &mut |r: Row| {
            if !r.is_blank() {
                numbers.push((r.number, r.cells[0].display()));
            }
            ControlFlow::Continue(())
        })
        .unwrap();
        assert_eq!(numbers, [(1, "h".into()), (2, "a\"b".into()), (4, "x\"\ny".into()), (7, "z".into())]);
    }

    #[test]
    fn nul_bytes_are_refused() {
        let err = rows(b"a,b\nx\x00y,z", Encoding::Utf8, None).unwrap_err();
        assert_eq!(
            (err.code, err.message.as_str()),
            ("invalid_character", "The file contains a NUL byte at offset 5.")
        );
    }

    #[test]
    fn an_unterminated_quote_stops_at_the_record_limit() {
        let mut bytes = b"a\n\"".to_vec();
        bytes.extend(std::iter::repeat_n(b'x', MAX_RECORD_BYTES + 10));
        let err = rows(&bytes, Encoding::Utf8, None).unwrap_err();
        assert_eq!((err.code, err.row), ("record_too_long", Some(2)));
        // Just under the limit is fine.
        let mut ok = b"a\n".to_vec();
        ok.extend(std::iter::repeat_n(b'x', MAX_RECORD_BYTES - 1));
        assert_eq!(rows(&ok, Encoding::Utf8, None).unwrap().1[1][0].len(), MAX_RECORD_BYTES - 1);
    }

    #[test]
    fn columns_are_capped() {
        let ok = vec!["c"; 200].join(",");
        assert_eq!(rows(ok.as_bytes(), Encoding::Utf8, None).unwrap().1[0].len(), 200);
        let over = vec!["c"; 201].join(",");
        let err = rows(over.as_bytes(), Encoding::Utf8, None).unwrap_err();
        assert_eq!((err.code, err.row), ("column_limit", Some(1)));
    }

    #[test]
    fn short_and_empty_files() {
        assert!(rows(b"", Encoding::Utf8, None).unwrap().1.is_empty());
        assert_eq!(rows(b"\xEF\xBB", Encoding::Utf8, None).unwrap_err().code, "invalid_encoding");
        assert_eq!(rows(b"ab", Encoding::Utf8, None).unwrap().1, [vec!["ab"]]);
        assert_eq!(rows(b"\xEF\xBB\xBF", Encoding::Utf8, None).unwrap().1.len(), 0);
    }

    #[test]
    fn the_callback_can_stop_reading() {
        let mut seen = 0;
        read(&b"a\nb\nc\n"[..], Encoding::Utf8, None, &LIMITS, &mut |_| {
            seen += 1;
            if seen == 2 { ControlFlow::Break(()) } else { ControlFlow::Continue(()) }
        })
        .unwrap();
        assert_eq!(seen, 2);
    }
}
