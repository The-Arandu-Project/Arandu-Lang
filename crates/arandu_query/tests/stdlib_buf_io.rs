//! Tests for std.io buffered I/O.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use arandu_query::db::DatabaseImpl;
use arandu_query::file_ide_diagnostics;
use arandu_query::passes::{exported_symbols, parse};

const IO_ARU: &str = include_str!("../../../stdlib/std/io.aru");

#[test]
fn stdlib_io_parses_and_exports_expected_symbols() {
    let mut db = DatabaseImpl::default();
    let file = db.new_file("stdlib/std/io.aru".to_string(), IO_ARU.to_string());
    match parse(&db, file).as_ref() {
        Ok(_) => {}
        Err(e) => panic!("io.aru must parse; got {e}"),
    }
    let exports = exported_symbols(&db, file);
    let expected = [
        "IoErrorKind",
        "IoError",
        "Read",
        "Write",
        "BufRead",
        "BufReader",
        "newBufReader",
        "BufWriter",
        "newBufWriter",
    ];
    for key in expected {
        assert!(
            exports.symbols.contains_key(key),
            "expected exported symbol `{key}`, got {:?}",
            exports.symbols.keys().collect::<Vec<_>>()
        );
    }
}

#[test]
fn stdlib_buf_io_usage_in_program() {
    let mut db = DatabaseImpl::default();
    let io_file = db.new_file("std/io.aru".to_string(), IO_ARU.to_string());
    let main_src = r#"
import std.io as io

struct MockStream {
    count: int
}

func testBufReader(): int {
    let s = MockStream { count: 10 }
    let br = io.newBufReader<MockStream>(s)
    let bw = io.newBufWriter<MockStream>(s)
    if br.pos != 0 || bw.buffered_count != 0 {
        return 1
    }
    return 0
}

func main(): int {
    return testBufReader()
}
"#;
    let main_file = db.new_file("main.aru".to_string(), main_src.to_string());

    let diags_io = file_ide_diagnostics(&db, io_file);
    let diags_main = file_ide_diagnostics(&db, main_file);

    let error_diags_io: Vec<_> = diags_io.iter().filter(|d| d.severity == 1).collect();
    let error_diags_main: Vec<_> = diags_main.iter().filter(|d| d.severity == 1).collect();

    assert!(
        error_diags_io.is_empty(),
        "unexpected errors in io.aru: {error_diags_io:?}"
    );
    assert!(
        error_diags_main.is_empty(),
        "unexpected errors in main.aru: {error_diags_main:?}"
    );
}
