//! Structure-aware: the fuzzer writes a worksheet part (and optionally the
//! shared strings), which is wrapped into an otherwise valid workbook, so the
//! cell reader and calamine's XML handling get the attention, not the ZIP.
#![no_main]

#[allow(dead_code)]
#[path = "../../src/modules/imports/parse/fixtures.rs"]
mod fixtures;
#[allow(dead_code)]
#[path = "../../src/modules/imports/parse/mod.rs"]
mod parse;

use libfuzzer_sys::fuzz_target;
use parse::{Limits, xlsx};

fuzz_target!(|data: &[u8]| {
    let (sheet, strings) = match data.iter().position(|&b| b == 0xFF) {
        Some(i) => (&data[..i], Some(&data[i + 1..])),
        None => (data, None),
    };
    let mut parts = fixtures::workbook_parts(&String::from_utf8_lossy(sheet), None, data.first() == Some(&b'1'), false);
    if let Some(s) = strings {
        parts.push(fixtures::Part::new("xl/sharedStrings.xml", s.to_vec()));
    }
    let mut cursor = std::io::Cursor::new(fixtures::zip(&parts));
    if xlsx::preflight(&mut cursor).is_ok()
        && let Ok((mut workbook, sheets)) = xlsx::open(cursor)
    {
        let limits = Limits { max_rows: 1_000, max_columns: 200 };
        for s in sheets {
            let _ = xlsx::read_sheet(&mut workbook, &s.name, &limits, &mut |row| {
                assert!(row.cells.len() <= 200);
                std::ops::ControlFlow::Continue(())
            });
        }
    }
});
