#![allow(clippy::unwrap_used, clippy::expect_used)]
//! TargetConfig drives target-dependent type checking (T038 integer literal
//! bounds) through the Salsa input instead of a host hardcode.

use arandu_middle::layout::DataLayout;
use arandu_middle::literal_pool::{parse_int_literal, AmirLiteralEntry};
use arandu_middle::DiagCode;
use arandu_middle::Severity;
use arandu_query::db::DatabaseImpl;
use arandu_query::passes::{lower_amir, type_check};

const SRC: &str = "func main(): int {\n    let x: uint = 4294967296\n    return 0\n}\n";

fn t038_count(db: &DatabaseImpl, file: arandu_query::SourceFile) -> usize {
    type_check(db, file)
        .diagnostics
        .iter()
        .filter(|d| d.code == DiagCode::T038IntegerLiteralOutOfRange)
        .count()
}

#[test]
fn host_target_config_accepts_u64_literal() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file("target_64.aru".into(), SRC.into());
    assert_eq!(
        t038_count(&db, file),
        0,
        "uint literal must fit on the host 64-bit layout"
    );
}

#[test]
fn target_config_32bit_flags_u64_literal() {
    let mut db = DatabaseImpl::new();
    db.set_target_config(DataLayout::ptr_width(4));
    let file = db.new_file("target_32.aru".into(), SRC.into());
    assert_eq!(
        t038_count(&db, file),
        1,
        "uint literal beyond u32::MAX must be rejected on a 32-bit layout"
    );
}

#[test]
fn target_config_32bit_update_revalidates() {
    let mut db = DatabaseImpl::new();
    db.set_target_config(DataLayout::ptr_width(4));
    let file = db.new_file("target_revalid.aru".into(), SRC.into());
    assert_eq!(t038_count(&db, file), 1);

    db.set_target_config(DataLayout::ptr_width(8));
    assert_eq!(
        t038_count(&db, file),
        0,
        "edit of TargetConfig must invalidate target-dependent diagnostics"
    );
}

// `std.core.mem` resolvable in a bare query DB: registered under its import key.
const MEM_MODULE: &str = "module std.core.mem\n\nextern \"arandu-intrinsic\" {\n    func sizeOf<T>() : uint\n    func alignOf<T>() : uint\n}\n";

const MAIN_WITH_MEM_INTRINSICS: &str =
    "import std.core.mem as mem\nfunc main(): uint {\n    let a = mem.sizeOf<uint>()\n    let b = mem.sizeOf<u64>()\n    let c = mem.alignOf<uint>()\n    return a + b + c\n}\n";

fn folded_int_literals(db: &DatabaseImpl, file: arandu_query::SourceFile) -> Vec<i128> {
    let tc = type_check(db, file);
    assert!(
        !tc.diagnostics.iter().any(|d| d.severity == Severity::Error),
        "type check failed: {:?}",
        tc.diagnostics
    );
    let artifacts = lower_amir(db, file);
    assert!(
        !artifacts.amir.funcs.is_empty(),
        "surface program did not reach AMIR: {:?}",
        artifacts.type_check.diagnostics
    );
    let mut values: Vec<i128> = artifacts
        .amir
        .literal_pool
        .entries
        .iter()
        .filter_map(|e| match e {
            AmirLiteralEntry::Int(s) => parse_int_literal(s.as_str()),
            _ => None,
        })
        .collect();
    values.sort_unstable();
    values
}

#[test]
fn mem_sizeof_alignof_fold_with_target_pointer_width() {
    let mut db = DatabaseImpl::new();
    db.new_file("stdlib/core/mem.aru".into(), MEM_MODULE.into());
    let file = db.new_file("main_mem.aru".into(), MAIN_WITH_MEM_INTRINSICS.into());

    // ILP32 natural layout: uint=4, u64=8, uint align=4.
    db.set_target_config(DataLayout::ptr_width(4));
    assert_eq!(
        folded_int_literals(&db, file),
        vec![4, 8],
        "mem.sizeOf/alignOf must fold to 32-bit layout constants"
    );

    db.set_target_config(DataLayout::ptr_width(8));
    assert_eq!(
        folded_int_literals(&db, file),
        vec![8],
        "mem.sizeOf/alignOf must fold to 64-bit layout constants"
    );
}
