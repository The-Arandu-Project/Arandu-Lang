#![allow(clippy::unwrap_used, clippy::expect_used)]

use arandu_middle::amir::{AmirConstant, AmirOperand, AmirRvalue, AmirStmt};
use arandu_mir::optimize_amir_checked_with_level;
use arandu_mir::pass_manager::OptLevel;
use arandu_query::db::DatabaseImpl;
use arandu_query::passes::{lower_amir, type_check};

const SOURCE: &str = r#"
func isHorizontalWhitespace(c: int): bool {
    return c == 32 || c == 9
}

func test_call(c: int): bool {
    return isHorizontalWhitespace(c)
}

func test_const(): bool {
    return isHorizontalWhitespace(32)
}
"#;

#[test]
fn leaf_function_is_inlined_at_o1() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file("inlining_test.aru".into(), SOURCE.into());

    let tc = type_check(&db, file);
    assert!(
        tc.diagnostics.is_empty(),
        "type check failed: {:?}",
        tc.diagnostics
    );

    let artifacts = lower_amir(&db, file);
    let symbols = artifacts.type_check.symbols.clone();
    let type_info = artifacts.type_check.type_info.clone();

    // At O0, test_call has a Call statement
    let amir_o0 = artifacts.amir.clone();
    let test_call_o0 = amir_o0
        .funcs
        .iter()
        .find(|f| symbols.get(f.symbol).name == "test_call")
        .expect("test_call not found");
    let has_call_o0 = test_call_o0
        .stmts
        .payloads
        .iter()
        .any(|s| matches!(s, AmirStmt::Call { .. }));
    assert!(has_call_o0, "At O0, Call statement must be preserved");

    // At O1, leaf inlining runs and eliminates the Call statement
    let mut amir_o1 = artifacts.amir.clone();
    optimize_amir_checked_with_level(
        &mut amir_o1,
        &symbols,
        &type_info.type_interner,
        OptLevel::O1,
    )
    .expect("AMIR optimization at O1 must succeed");

    let test_call_o1 = amir_o1
        .funcs
        .iter()
        .find(|f| symbols.get(f.symbol).name == "test_call")
        .expect("test_call not found");
    let has_call_o1 = test_call_o1
        .stmts
        .payloads
        .iter()
        .any(|s| matches!(s, AmirStmt::Call { .. }));
    assert!(
        !has_call_o1,
        "At O1, leaf function isHorizontalWhitespace must be inlined into test_call"
    );

    // In test_const, inlining + SCCP should fold the result to constant true
    let test_const_o1 = amir_o1
        .funcs
        .iter()
        .find(|f| symbols.get(f.symbol).name == "test_const")
        .expect("test_const not found");
    let has_call_const = test_const_o1
        .stmts
        .payloads
        .iter()
        .any(|s| matches!(s, AmirStmt::Call { .. }));
    assert!(
        !has_call_const,
        "test_const must not have any Call statements"
    );

    let folds_to_true = test_const_o1.stmts.payloads.iter().any(|s| {
        matches!(
            s,
            AmirStmt::Assign {
                rhs: AmirRvalue::Use(AmirOperand::Constant(AmirConstant::Bool(true))),
                ..
            }
        )
    });
    assert!(
        folds_to_true,
        "test_const() calling isHorizontalWhitespace(32) must fold to true"
    );
}
