//! Tests for std.alloc.arena.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use arandu_query::db::DatabaseImpl;
use arandu_query::file_ide_diagnostics;
use arandu_query::passes::{exported_symbols, parse};

const ARENA_ARU: &str = include_str!("../../../stdlib/alloc/arena.aru");

#[test]
fn stdlib_arena_parses_and_exports_expected_symbols() {
    let mut db = DatabaseImpl::default();
    let file = db.new_file("stdlib/alloc/arena.aru".to_string(), ARENA_ARU.to_string());
    match parse(&db, file).as_ref() {
        Ok(_) => {}
        Err(e) => panic!("arena.aru must parse; got {e}"),
    }
    let exports = exported_symbols(&db, file);
    let expected = ["Arena", "new"];
    for key in expected {
        assert!(
            exports.symbols.contains_key(key),
            "expected exported symbol `{key}`, got {:?}",
            exports.symbols.keys().collect::<Vec<_>>()
        );
    }
}

#[test]
fn stdlib_arena_usage_in_program() {
    let mut db = DatabaseImpl::default();
    let arena_file = db.new_file("std/alloc/arena.aru".to_string(), ARENA_ARU.to_string());
    let main_src = r#"
import std.alloc.arena as arena

func testArena(): int {
    let mut a = arena.new(1024)
    let p1 = a.alloc(32, 8)
    if p1 is Option.None {
        return 1
    }
    let used = a.allocatedBytes()
    if used < 32 {
        return 2
    }
    a.reset()
    if a.allocatedBytes() != 0 {
        return 3
    }
    a.free()
    return 0
}

func main(): int {
    return testArena()
}
"#;
    let main_file = db.new_file("main.aru".to_string(), main_src.to_string());

    let diags_arena = file_ide_diagnostics(&db, arena_file);
    let diags_main = file_ide_diagnostics(&db, main_file);

    let error_diags_arena: Vec<_> = diags_arena.iter().filter(|d| d.severity == 1).collect();
    let error_diags_main: Vec<_> = diags_main.iter().filter(|d| d.severity == 1).collect();

    assert!(
        error_diags_arena.is_empty(),
        "unexpected errors in arena.aru: {error_diags_arena:?}"
    );
    assert!(
        error_diags_main.is_empty(),
        "unexpected errors in main.aru: {error_diags_main:?}"
    );
}
