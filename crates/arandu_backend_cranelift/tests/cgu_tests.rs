#![cfg(target_pointer_width = "64")]
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use arandu_backend_cranelift::{AotOptimization, compile_cgu, partition_program};
use arandu_semantics::{lower_to_amir, lower_to_hir, resolve_for_test, type_check};
use cranelift_object::object::{self, Object, ObjectSymbol};
use std::sync::Arc;
use target_lexicon::Triple;

const TEST_TOOLCHAIN: &str = "test-toolchain-v1";

fn compile_amir(
    src: &str,
) -> (
    arandu_semantics::amir::AmirProgram,
    arandu_semantics::SymbolTable,
    arandu_semantics::TypeInfo,
) {
    let program = arandu_parser::parse(src).expect("parse failed");
    let resolution = resolve_for_test(0, &program);
    let mut tc = type_check(
        resolution,
        &program,
        arandu_semantics::TargetInfo { pointer_width: 64 },
    );
    let hir = lower_to_hir(&mut tc, &program).expect("HIR lowering failed");
    let amir = lower_to_amir(&tc, &hir, 64).expect("AMIR lowering failed");
    let symbols = Arc::unwrap_or_clone(tc.symbols);
    let type_info = Arc::unwrap_or_clone(tc.type_info);
    (amir, symbols, type_info)
}

#[test]
fn partitions_functions_into_discrete_cgus() {
    let src = r#"
func alpha(): int {
    return 10;
}

func beta(): int {
    return 20;
}
"#;
    let (amir, symbols, type_info) = compile_amir(src);
    let target = Triple::host();
    let units = partition_program(
        &amir,
        &symbols,
        &type_info,
        &target,
        AotOptimization::Baseline,
        TEST_TOOLCHAIN,
    );

    assert_eq!(units.len(), 2, "expected 2 CGUs for 2 functions");
    assert!(units.iter().any(|u| u.name == "alpha"));
    assert!(units.iter().any(|u| u.name == "beta"));
    assert_ne!(
        units[0].hash, units[1].hash,
        "different functions must have distinct hashes"
    );
}

#[test]
fn editing_one_function_preserves_sibling_cgu_hash() {
    let src_v1 = r#"
func helper(): int {
    return 100;
}

func calculate(): int {
    return helper() + 1;
}
"#;
    let (amir_v1, syms_v1, ti_v1) = compile_amir(src_v1);
    let target = Triple::host();
    let units_v1 = partition_program(
        &amir_v1,
        &syms_v1,
        &ti_v1,
        &target,
        AotOptimization::Baseline,
        TEST_TOOLCHAIN,
    );
    let helper_v1 = units_v1.iter().find(|u| u.name == "helper").unwrap();
    let calc_v1 = units_v1.iter().find(|u| u.name == "calculate").unwrap();

    // Now edit the body of `calculate()`, keeping `helper()` untouched
    let src_v2 = r#"
func helper(): int {
    return 100;
}

func calculate(): int {
    return helper() + 999;
}
"#;
    let (amir_v2, syms_v2, ti_v2) = compile_amir(src_v2);
    let units_v2 = partition_program(
        &amir_v2,
        &syms_v2,
        &ti_v2,
        &target,
        AotOptimization::Baseline,
        TEST_TOOLCHAIN,
    );
    let helper_v2 = units_v2.iter().find(|u| u.name == "helper").unwrap();
    let calc_v2 = units_v2.iter().find(|u| u.name == "calculate").unwrap();

    // `helper` was NOT modified -> its hash MUST be identical!
    assert_eq!(
        helper_v1.hash, helper_v2.hash,
        "untouched sibling function must have identical CGU hash"
    );

    // `calculate` was modified -> its hash MUST differ!
    assert_ne!(
        calc_v1.hash, calc_v2.hash,
        "modified function must have different CGU hash"
    );
}

#[test]
fn compile_cgu_emits_valid_relocatable_objects() {
    let src = r#"
func add(a: int, b: int): int {
    return a + b;
}

func sub(a: int, b: int): int {
    return a - b;
}
"#;
    let (amir, symbols, type_info) = compile_amir(src);
    let target = Triple::host();

    let units = partition_program(
        &amir,
        &symbols,
        &type_info,
        &target,
        AotOptimization::Baseline,
        TEST_TOOLCHAIN,
    );
    assert_eq!(units.len(), 2);

    for unit in &units {
        let bytes = compile_cgu(
            unit,
            &amir,
            &symbols,
            &type_info,
            &target,
            AotOptimization::Baseline,
        )
        .expect("compile_cgu should succeed");

        let file = object::File::parse(bytes.as_slice()).expect("valid object file");
        assert!(
            file.symbols().any(|s| s.is_definition() && s.is_global()),
            "object must contain at least one defined global symbol"
        );
    }
}

#[test]
fn cgu_hash_ignores_source_spans_but_tracks_toolchain_identity() {
    let compact = "func main(): int { return 7; }\n";
    let shifted = "\n\nfunc main(): int {\n    return 7;\n}\n";
    let (first_program, first_symbols, first_types) = compile_amir(compact);
    let (second_program, second_symbols, second_types) = compile_amir(shifted);
    let target = Triple::host();
    let first = partition_program(
        &first_program,
        &first_symbols,
        &first_types,
        &target,
        AotOptimization::Baseline,
        TEST_TOOLCHAIN,
    );
    let shifted = partition_program(
        &second_program,
        &second_symbols,
        &second_types,
        &target,
        AotOptimization::Baseline,
        TEST_TOOLCHAIN,
    );
    let different_toolchain = partition_program(
        &first_program,
        &first_symbols,
        &first_types,
        &target,
        AotOptimization::Baseline,
        "test-toolchain-v2",
    );

    assert_eq!(first[0].hash, shifted[0].hash);
    assert_ne!(first[0].hash, different_toolchain[0].hash);
}
