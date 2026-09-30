//! Any bytes through the XLSX preflight (the ZIP walk, the decompression
//! caps and the XML scan), and through calamine when it passes.
#![no_main]

#[allow(dead_code)]
#[path = "../../src/modules/imports/parse/mod.rs"]
mod parse;

use libfuzzer_sys::fuzz_target;
use parse::{Limits, xlsx};

fuzz_target!(|data: &[u8]| {
    let mut cursor = std::io::Cursor::new(data.to_vec());
    if xlsx::preflight(&mut cursor).is_ok()
        && let Ok((mut workbook, sheets)) = xlsx::open(cursor)
        && let Some(sheet) = sheets.first()
    {
        let limits = Limits { max_rows: 1_000, max_columns: 200 };
        let _ = xlsx::read_sheet(&mut workbook, &sheet.name, &limits, &mut |_| std::ops::ControlFlow::Continue(()));
    }
});
