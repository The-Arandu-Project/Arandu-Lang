#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Tests for IEEE 754-2019 floating-point conformance in Arandu:
//! - NaN ordered and unordered comparisons
//! - AMIR lowering and optimization passes (SCCP, GVN) preserving float comparison semantics without illegal constant folding (e.g. `x == x` must not fold to `true`).

use arandu_middle::amir::{AmirConstant, AmirOperand, AmirRvalue, AmirStmt};
use arandu_middle::ops::BinaryOp;
use arandu_mir::optimize_amir_checked_with_level;
use arandu_mir::pass_manager::OptLevel;
use arandu_query::db::DatabaseImpl;
use arandu_query::passes::{lower_amir, type_check};

const FLOAT_CMP_SRC: &str = r#"
func test_eq(x: float): bool {
    return x == x
}

func test_ne(x: float): bool {
    return x != x
}

func test_lt(a: float, b: float): bool {
    return a < b
}

func test_le(a: float, b: float): bool {
    return a <= b
}

func test_gt(a: float, b: float): bool {
    return a > b
}

func test_ge(a: float, b: float): bool {
    return a >= b
}
"#;

#[test]
fn amir_lowering_preserves_float_comparison_ops() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file("float_cmp.aru".into(), FLOAT_CMP_SRC.into());

    let tc = type_check(&db, file);
    assert!(
        tc.diagnostics.is_empty(),
        "type check failed: {:?}",
        tc.diagnostics
    );

    let artifacts = lower_amir(&db, file);
    let amir = &artifacts.amir;
    let symbols = &artifacts.type_check.symbols;
    assert_eq!(amir.funcs.len(), 6);

    for func in &amir.funcs {
        let func_name = symbols.get(func.symbol).name.as_str();
        let expected_op = match func_name {
            "test_eq" => BinaryOp::Equal,
            "test_ne" => BinaryOp::NotEqual,
            "test_lt" => BinaryOp::Lt,
            "test_le" => BinaryOp::LtEqual,
            "test_gt" => BinaryOp::Gt,
            "test_ge" => BinaryOp::GtEqual,
            other => panic!("unexpected func name: {other}"),
        };

        let has_expected_binary_op = func.stmts.payloads.iter().any(|stmt| match stmt {
            AmirStmt::Assign {
                rhs: AmirRvalue::Binary { op, .. },
                ..
            } => *op == expected_op,
            _ => false,
        });

        assert!(
            has_expected_binary_op,
            "function {} must contain BinaryOp::{:?}",
            func_name, expected_op
        );
    }
}

#[test]
fn amir_optimizer_does_not_fold_float_self_equality_to_true() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file("float_self_eq.aru".into(), FLOAT_CMP_SRC.into());

    let artifacts = lower_amir(&db, file);
    let mut amir = artifacts.amir.clone();
    let symbols = artifacts.type_check.symbols.clone();
    let type_info = artifacts.type_check.type_info.clone();

    // Run aggressive optimization (O2) including SCCP and GVN
    optimize_amir_checked_with_level(&mut amir, &symbols, &type_info.type_interner, OptLevel::O2)
        .expect("AMIR optimization should succeed");

    // Under IEEE 754-2019, x == x is false if x is NaN.
    // Therefore, an optimizer MUST NOT replace x == x with constant true.
    let eq_func = amir
        .funcs
        .iter()
        .find(|f| symbols.get(f.symbol).name == "test_eq")
        .expect("test_eq function not found");

    let folded_to_true = eq_func.stmts.payloads.iter().any(|stmt| {
        matches!(
            stmt,
            AmirStmt::Assign {
                rhs: AmirRvalue::Use(AmirOperand::Constant(AmirConstant::Bool(true))),
                ..
            }
        )
    });
    assert!(
        !folded_to_true,
        "IEEE 754-2019 violation: x == x must NOT be folded to constant true"
    );

    // Similarly, x != x is true if x is NaN.
    // An optimizer MUST NOT replace x != x with constant false.
    let ne_func = amir
        .funcs
        .iter()
        .find(|f| symbols.get(f.symbol).name == "test_ne")
        .expect("test_ne function not found");

    let folded_to_false = ne_func.stmts.payloads.iter().any(|stmt| {
        matches!(
            stmt,
            AmirStmt::Assign {
                rhs: AmirRvalue::Use(AmirOperand::Constant(AmirConstant::Bool(false))),
                ..
            }
        )
    });
    assert!(
        !folded_to_false,
        "IEEE 754-2019 violation: x != x must NOT be folded to constant false"
    );
}
