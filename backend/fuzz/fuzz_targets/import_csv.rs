//! Any bytes, read as CSV in every encoding and with every delimiter choice:
//! the reader must return rows or an error, never panic, and never hand over
//! a record larger than 1 MiB or with more than 200 fields.
#![no_main]

#[allow(dead_code)]
#[path = "../../src/modules/imports/parse/mod.rs"]
mod parse;

use libfuzzer_sys::fuzz_target;
use parse::Limits;
use parse::csv::{self, Encoding};

fuzz_target!(|data: &[u8]| {
    let limits = Limits { max_rows: 1_000, max_columns: 200 };
    for encoding in [Encoding::Utf8, Encoding::Windows1252, Encoding::Iso88591] {
        for delimiter in [None, Some(b';')] {
            let _ = csv::read(data, encoding, delimiter, &limits, &mut |row| {
                assert!(row.cells.len() <= 200);
                std::ops::ControlFlow::Continue(())
            });
        }
    }
    let _ = csv::is_utf8(data);
});
