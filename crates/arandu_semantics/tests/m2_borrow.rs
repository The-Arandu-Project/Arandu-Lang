#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use arandu_semantics::{
    DiagCode, Severity, lower_to_amir, lower_to_amir_with_interfaces, lower_to_hir,
    resolve_for_test, type_check,
};

fn ownership_codes(src: &str) -> Vec<DiagCode> {
    let program = arandu_parser::parse(src).expect("parse");
    let resolution = resolve_for_test(0, &program);
    let mut tc = type_check(
        resolution,
        &program,
        arandu_semantics::TargetInfo { pointer_width: 64 },
    );
    let type_errors: Vec<_> = tc
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.severity == Severity::Error)
        .collect();
    assert!(type_errors.is_empty(), "typeck errors: {type_errors:?}");
    let hir = lower_to_hir(&mut tc, &program).expect("hir");
    let diagnostics = match lower_to_amir_with_interfaces(&mut tc, &hir, 64) {
        Ok((_, diagnostics)) | Err(diagnostics) => diagnostics,
    };
    let mut codes: Vec<_> = diagnostics
        .into_iter()
        .filter_map(|diagnostic| match diagnostic.code {
            code @ (DiagCode::O002MoveWhileBorrowed
            | DiagCode::O003MutableBorrowConflict
            | DiagCode::O004GenerationalFallback
            | DiagCode::O005DoubleFree
            | DiagCode::O006DestroyWhileBorrowed
            | DiagCode::O007InconsistentMoveBetweenBranches
            | DiagCode::O008UseBeforeInit
            | DiagCode::O010EscapeOfBorrowedValue) => Some(code),
            _ => None,
        })
        .collect();
    codes.sort_by_key(|code| code.as_str());
    codes.dedup();
    codes
}

fn assert_deterministic(case: &str, source: &str, expected: &[DiagCode]) {
    let first = ownership_codes(source);
    assert_eq!(first, ownership_codes(source), "{case}: result changed");
    assert_eq!(
        first, expected,
        "{case}: unexpected ownership classification"
    );
}

fn assert_cfg_equivalent(case: &str, left: &str, right: &str, expected: &[DiagCode]) {
    let first = ownership_codes(left);
    let repeated = ownership_codes(left);
    let transformed = ownership_codes(right);
    assert_eq!(first, repeated, "{case}: result was not deterministic");
    assert_eq!(
        first, transformed,
        "{case}: equivalent CFG shapes disagreed"
    );
    assert_eq!(
        first, expected,
        "{case}: unexpected ownership classification"
    );
}

#[test]
fn o003_conflicting_borrows_end_to_end() {
    let src = r#"
func use_both(a: &mut int, b: &int): int {
    return *a
}
func main(): int {
    let n = 1
    let a = &mut n
    let b = &n
    return use_both(a, b)
}
"#;
    let program = arandu_parser::parse(src).expect("parse");
    let resolution = resolve_for_test(0, &program);
    let mut tc = type_check(
        resolution,
        &program,
        arandu_semantics::TargetInfo { pointer_width: 64 },
    );
    let errors: Vec<_> = tc
        .diagnostics
        .iter()
        .filter(|d| d.severity == arandu_semantics::Severity::Error)
        .collect();
    assert!(errors.is_empty(), "typeck errors: {errors:?}");
    let hir = lower_to_hir(&mut tc, &program).expect("hir");
    let result = lower_to_amir(&tc, &hir, 64);
    match result {
        Ok(_) => panic!("expected O003 from lower_to_amir"),
        Err(diags) => {
            assert!(
                diags
                    .iter()
                    .any(|d| d.code == DiagCode::O003MutableBorrowConflict),
                "expected O003, got {diags:?}"
            );
        }
    }
}

#[test]
fn return_ref_to_local_is_o010() {
    let src = r#"
func bad(): &int {
    let x = 1
    return &x
}
"#;
    let program = arandu_parser::parse(src).expect("parse");
    let resolution = resolve_for_test(0, &program);
    let mut tc = type_check(
        resolution,
        &program,
        arandu_semantics::TargetInfo { pointer_width: 64 },
    );
    let errors: Vec<_> = tc
        .diagnostics
        .iter()
        .filter(|d| d.severity == arandu_semantics::Severity::Error)
        .collect();
    assert!(errors.is_empty(), "typeck errors: {errors:?}");
    let hir = lower_to_hir(&mut tc, &program).expect("hir");
    let result = lower_to_amir(&tc, &hir, 64);
    match result {
        Ok(_) => panic!("expected O010 from lower_to_amir"),
        Err(diags) => {
            assert!(
                diags
                    .iter()
                    .any(|d| d.code == DiagCode::O010EscapeOfBorrowedValue),
                "expected O010, got {diags:?}"
            );
            assert!(
                diags
                    .iter()
                    .any(|d| d.code == DiagCode::O004GenerationalFallback),
                "expected O004 note, got {diags:?}"
            );
        }
    }
}

#[test]
fn sequential_borrows_ok() {
    let src = r#"
func main(): int {
    let n = 5
    let a = &n
    let x = *a
    let b = &mut n
    let y = *b
    return x
}
"#;
    let program = arandu_parser::parse(src).expect("parse");
    let resolution = resolve_for_test(0, &program);
    let mut tc = type_check(
        resolution,
        &program,
        arandu_semantics::TargetInfo { pointer_width: 64 },
    );
    assert!(
        !tc.diagnostics
            .iter()
            .any(|d| d.severity == arandu_semantics::Severity::Error),
        "typeck: {:?}",
        tc.diagnostics
    );
    let hir = lower_to_hir(&mut tc, &program).expect("hir");
    let amir = lower_to_amir(&tc, &hir, 64).expect("sequential borrows should lower");
    assert!(!amir.funcs.is_empty());
}

#[test]
fn ownership_classification_is_stable_across_equivalent_cfg_shapes() {
    let conflict_linear = r#"
func useBoth(a: mut ref int, b: ref int): int { return *a + *b }
func main(): int {
    let mut value = 1
    let exclusive = &mut value
    let loan = &value
    return useBoth(exclusive, loan)
}
"#;
    let conflict_diamond = r#"
func useBoth(a: mut ref int, b: ref int): int { return *a + *b }
func main(): int {
    let mut value = 1
    let exclusive = &mut value
    if value > 0 {
        let loan = &value
        return useBoth(exclusive, loan)
    } else {
        let loan = &value
        return useBoth(exclusive, loan)
    }
}
"#;
    assert_cfg_equivalent(
        "exclusive/shared diamond",
        conflict_linear,
        conflict_diamond,
        &[DiagCode::O003MutableBorrowConflict],
    );

    let valid_linear = r#"
func main(): int {
    let value = 7
    let loan = &value
    return *loan
}
"#;
    let valid_diamond = r#"
func main(): int {
    let value = 7
    let loan = &value
    let mut result = 0
    if value > 0 { result = *loan } else { result = *loan }
    return result
}
"#;
    assert_cfg_equivalent("valid shared diamond", valid_linear, valid_diamond, &[]);

    let mutation_linear = r#"
func main(): int {
    let mut value = 7
    let loan = &value
    value = 8
    return *loan
}
"#;
    let mutation_diamond = r#"
func main(): int {
    let mut value = 7
    let loan = &value
    if value > 0 { value = 8 } else { value = 8 }
    return *loan
}
"#;
    assert_cfg_equivalent(
        "mutation diamond",
        mutation_linear,
        mutation_diamond,
        &[DiagCode::O003MutableBorrowConflict],
    );

    let mutation_loop = r#"
func main(): int {
    let mut value = 7
    let loan = &value
    let mut once = true
    while once {
        value = 8
        once = false
    }
    return *loan
}
"#;
    assert_cfg_equivalent(
        "mutation loop",
        mutation_linear,
        mutation_loop,
        &[DiagCode::O003MutableBorrowConflict],
    );

    let mutation_match = r#"
func main(): int {
    let mut value = 7
    let loan = &value
    match value {
        7 => { value = 8 }
        _ => { value = 8 }
    }
    return *loan
}
"#;
    assert_cfg_equivalent(
        "mutation match",
        mutation_linear,
        mutation_match,
        &[DiagCode::O003MutableBorrowConflict],
    );

    let mutation_struct_carrier = r#"
struct Holder { item: ref int }
func main(): int {
    let mut value = 7
    let holder = Holder { item: &value }
    value = 8
    return *holder.item
}
"#;
    assert_deterministic(
        "struct carrier",
        mutation_struct_carrier,
        &[DiagCode::O004GenerationalFallback],
    );

    let mutation_call_result = r#"
func borrowValue(value: ref int): ref int { return value }
func main(): int {
    let mut value = 7
    let loan = borrowValue(value)
    value = 8
    return *loan
}
"#;
    assert_cfg_equivalent(
        "call result",
        mutation_linear,
        mutation_call_result,
        &[DiagCode::O003MutableBorrowConflict],
    );

    let partial_move_diamond = r#"
struct Box { value: str }
func consume(own item: Box): int { return 7 }
func main(): int {
    let item = Box { value: "x" }
    let mut result = 0
    if result == 0 { result = consume(item) } else { result = 1 }
    return consume(item) + result
}
"#;
    let partial_move_match = r#"
struct Box { value: str }
func consume(own item: Box): int { return 7 }
func main(): int {
    let item = Box { value: "x" }
    let mut result = 0
    match result {
        0 => { result = consume(item) }
        _ => { result = 1 }
    }
    return consume(item) + result
}
"#;
    assert_cfg_equivalent(
        "partial move join",
        partial_move_diamond,
        partial_move_match,
        &[DiagCode::O007InconsistentMoveBetweenBranches],
    );
}
