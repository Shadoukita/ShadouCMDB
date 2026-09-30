//! Test workbooks built byte by byte, including broken and hostile ones.

use std::io::Write;

/// One ZIP entry; the `local_*` fields make the local header disagree.
#[derive(Clone)]
pub struct Part {
    pub name: String,
    pub data: Vec<u8>,
    /// 0 stored, 8 deflate, anything else as given (unsupported methods).
    pub method: u16,
    pub local_name: Option<String>,
    pub local_method: Option<u16>,
    pub local_crc: Option<u32>,
    pub flags: u16,
}

impl Part {
    pub fn new(name: &str, data: impl Into<Vec<u8>>) -> Part {
        Part {
            name: name.into(),
            data: data.into(),
            method: 8,
            local_name: None,
            local_method: None,
            local_crc: None,
            flags: 0,
        }
    }
}

fn crc(data: &[u8]) -> u32 {
    let mut c = flate2::Crc::new();
    c.update(data);
    c.sum()
}

/// A ZIP archive of the parts, in order.
pub fn zip(parts: &[Part]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut central = Vec::new();
    for p in parts {
        let compressed = if p.method == 8 {
            let mut e = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::best());
            e.write_all(&p.data).unwrap();
            e.finish().unwrap()
        } else {
            p.data.clone()
        };
        let offset = out.len() as u32;
        let crc = crc(&p.data);
        let local_name = p.local_name.clone().unwrap_or_else(|| p.name.clone());
        out.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        out.extend_from_slice(&20u16.to_le_bytes());
        out.extend_from_slice(&p.flags.to_le_bytes());
        out.extend_from_slice(&p.local_method.unwrap_or(p.method).to_le_bytes());
        out.extend_from_slice(&[0, 0, 0, 0]);
        out.extend_from_slice(&p.local_crc.unwrap_or(crc).to_le_bytes());
        out.extend_from_slice(&(compressed.len() as u32).to_le_bytes());
        out.extend_from_slice(&(p.data.len() as u32).to_le_bytes());
        out.extend_from_slice(&(local_name.len() as u16).to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(local_name.as_bytes());
        out.extend_from_slice(&compressed);

        central.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
        central.extend_from_slice(&20u16.to_le_bytes());
        central.extend_from_slice(&20u16.to_le_bytes());
        central.extend_from_slice(&p.flags.to_le_bytes());
        central.extend_from_slice(&p.method.to_le_bytes());
        central.extend_from_slice(&[0, 0, 0, 0]);
        central.extend_from_slice(&crc.to_le_bytes());
        central.extend_from_slice(&(compressed.len() as u32).to_le_bytes());
        central.extend_from_slice(&(p.data.len() as u32).to_le_bytes());
        central.extend_from_slice(&(p.name.len() as u16).to_le_bytes());
        central.extend_from_slice(&[0; 4]);
        central.extend_from_slice(&[0; 4]);
        central.extend_from_slice(&[0; 4]);
        central.extend_from_slice(&offset.to_le_bytes());
        central.extend_from_slice(p.name.as_bytes());
    }
    let cd_offset = out.len() as u32;
    out.extend_from_slice(&central);
    out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    let n = parts.len().min(0xFFFF) as u16;
    out.extend_from_slice(&n.to_le_bytes());
    out.extend_from_slice(&n.to_le_bytes());
    out.extend_from_slice(&(central.len() as u32).to_le_bytes());
    out.extend_from_slice(&cd_offset.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out
}

pub const CONTENT_TYPES: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/><Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/><Override PartName="/xl/worksheets/sheet2.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/><Override PartName="/xl/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.styles+xml"/></Types>"#;

const RELS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>"#;

const WORKBOOK_RELS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet2.xml"/><Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/></Relationships>"#;

/// Style 1 is a date (numFmt 14), style 2 a date and time (numFmt 22).
const STYLES: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><fonts count="1"><font/></fonts><fills count="1"><fill/></fills><borders count="1"><border/></borders><cellStyleXfs count="1"><xf/></cellStyleXfs><cellXfs count="3"><xf numFmtId="0"/><xf numFmtId="14" applyNumberFormat="1"/><xf numFmtId="22" applyNumberFormat="1"/></cellXfs></styleSheet>"#;

/// A worksheet cell for [`sheet_xml`].
pub enum C<'a> {
    S(&'a str),
    N(&'a str),
    B(bool),
    /// A serial date with style 1.
    Date(&'a str),
    /// A serial date-time with style 2.
    DateTime(&'a str),
    /// A formula with a cached value (or none).
    F(&'a str, Option<&'a str>),
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// Worksheet XML with cells at explicit references: `(\"A1\", C::S(\"Hostname\"))`.
pub fn sheet_xml(cells: &[(&str, C<'_>)]) -> String {
    let mut rows: Vec<(u32, Vec<String>)> = Vec::new();
    for (r, c) in cells {
        let row: u32 = r.trim_start_matches(|ch: char| ch.is_ascii_alphabetic()).parse().unwrap();
        let xml = match c {
            C::S(s) => format!(r#"<c r="{r}" t="inlineStr"><is><t>{}</t></is></c>"#, escape(s)),
            C::N(n) => format!(r#"<c r="{r}"><v>{n}</v></c>"#),
            C::B(b) => format!(r#"<c r="{r}" t="b"><v>{}</v></c>"#, u8::from(*b)),
            C::Date(n) => format!(r#"<c r="{r}" s="1"><v>{n}</v></c>"#),
            C::DateTime(n) => format!(r#"<c r="{r}" s="2"><v>{n}</v></c>"#),
            C::F(f, Some(v)) => format!(r#"<c r="{r}"><f>{}</f><v>{v}</v></c>"#, escape(f)),
            C::F(f, None) => format!(r#"<c r="{r}"><f>{}</f></c>"#, escape(f)),
        };
        match rows.iter_mut().find(|(n, _)| *n == row) {
            Some((_, v)) => v.push(xml),
            None => rows.push((row, vec![xml])),
        }
    }
    let body: String = rows.iter().map(|(n, cs)| format!(r#"<row r="{n}">{}</row>"#, cs.join(""))).collect();
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData>{body}</sheetData></worksheet>"#
    )
}

/// The parts of a workbook with two sheets; the second is hidden when asked.
pub fn workbook_parts(sheet1: &str, sheet2: Option<&str>, date1904: bool, second_hidden: bool) -> Vec<Part> {
    let pr = if date1904 { r#"<workbookPr date1904="1"/>"# } else { "" };
    let second = if sheet2.is_some() {
        let state = if second_hidden { r#" state="hidden""# } else { "" };
        format!(r#"<sheet name="Notes" sheetId="2"{state} r:id="rId2"/>"#)
    } else {
        String::new()
    };
    let workbook = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">{pr}<sheets><sheet name="Servers" sheetId="1" r:id="rId1"/>{second}</sheets></workbook>"#
    );
    let mut parts = vec![
        Part::new("[Content_Types].xml", CONTENT_TYPES),
        Part::new("_rels/.rels", RELS),
        Part::new("xl/workbook.xml", workbook),
        Part::new("xl/_rels/workbook.xml.rels", WORKBOOK_RELS),
        Part::new("xl/styles.xml", STYLES),
        Part::new("xl/worksheets/sheet1.xml", sheet1),
    ];
    parts.push(Part::new("xl/worksheets/sheet2.xml", sheet2.map(str::to_owned).unwrap_or_else(|| sheet_xml(&[]))));
    parts
}

/// A plain one-sheet workbook.
pub fn workbook(cells: &[(&str, C<'_>)]) -> Vec<u8> {
    zip(&workbook_parts(&sheet_xml(cells), None, false, false))
}
