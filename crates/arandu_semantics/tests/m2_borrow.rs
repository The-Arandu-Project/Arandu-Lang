#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use arandu_semantics::{DiagCode, lower_to_amir, lower_to_hir, resolve_for_test, type_check};

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
