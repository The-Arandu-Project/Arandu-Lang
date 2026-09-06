//! Tests for std.alloc.bitset.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use arandu_query::db::DatabaseImpl;
use arandu_query::passes::{exported_symbols, parse};

const BITSET_ARU: &str = include_str!("../../../stdlib/alloc/bitset.aru");

#[test]
fn stdlib_bitset_parses_and_exports_expected_symbols() {
    let mut db = DatabaseImpl::default();
    let file = db.new_file(
        "stdlib/alloc/bitset.aru".to_string(),
        BITSET_ARU.to_string(),
    );
    match parse(&db, file).as_ref() {
        Ok(_) => {}
        Err(e) => panic!("bitset.aru must parse; got {e}"),
    }
    let exports = exported_symbols(&db, file);
    let expected = [
        "BitSet",
        "bitsetNew",
        "bitsetWithCapacity",
        "BitSet.contains",
        "BitSet.insert",
        "BitSet.remove",
        "BitSet.clear",
        "BitSet.unionWith",
        "BitSet.intersectWith",
        "BitSet.differenceWith",
        "BitSet.countOnes",
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
fn stdlib_bitset_usage_in_program() {
    let mut db = DatabaseImpl::default();
    let file = db.new_file(
        "test_bitset_usage.aru".to_string(),
        r#"
            module test_bitset_usage

            import std.alloc.bitset as bitset

            public func testBits(): uint {
                let mut bs = bitset.bitsetNew()
                bs.insert(1)
                bs.insert(65)
                bs.insert(128)
                return bs.countOnes()
            }
        "#
        .to_string(),
    );
    match parse(&db, file).as_ref() {
        Ok(_) => {}
        Err(e) => panic!("test_bitset_usage.aru must parse; got {e}"),
    }
    let exports = exported_symbols(&db, file);
    assert!(exports.symbols.contains_key("testBits"));
}
