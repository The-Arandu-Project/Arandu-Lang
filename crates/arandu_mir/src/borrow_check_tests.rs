#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;
use crate::SymbolId;
use crate::amir::{AmirBasicBlock, AmirLocal, AmirPlace, AmirStmtTable, AmirTemp, AmirTerminator};
use crate::cfg::compute_cfg_edges;
use crate::layout::DenseRange;
use crate::ops::UnaryOp;
use crate::types::{ArType, Primitive, TypeInterner};
use smallvec::smallvec;

fn intern(ty: ArType) -> crate::types::TypeId {
    TypeInterner::new().intern(ty)
}

fn empty_symbols() -> SymbolTable {
    SymbolTable::new(0)
}

fn place(l: usize) -> AmirPlace {
    AmirPlace {
        local: LocalId::from_usize(l),
        projections: smallvec![],
    }
}

fn local(i: usize, ty: crate::types::TypeId) -> AmirLocal {
    AmirLocal {
        id: LocalId::from_usize(i),
        ty,
        is_memory: true,
        symbol: None,
        span: Span::new(0, 0, 1 + i as u32),
        use_span: None,
    }
}

fn temp(i: usize, ty: crate::types::TypeId) -> AmirTemp {
    AmirTemp {
        id: TempId::from_usize(i),
        ty,
        is_copy: true,
        is_nullable: false,
        span: Span::new(0, 10 + i as u32, 11 + i as u32),
    }
}

/// `&mut x` then `&x` while first loan live → O003.
#[test]
fn o003_shared_while_exclusive() {
    let int = intern(ArType::Primitive(Primitive::Int));
    let mut stmts = AmirStmtTable::new();
    stmts.push(AmirStmt::Assign {
        lhs: TempId::from_usize(0),
        rhs: AmirRvalue::BorrowMut(place(0)),
    });
    // Keep t0 live by using it after the second borrow attempt... actually
    // second borrow at stmt 1 while t0 still live-out if used later.
    stmts.push(AmirStmt::Assign {
        lhs: TempId::from_usize(1),
        rhs: AmirRvalue::Borrow(place(0)),
    });
    stmts.push(AmirStmt::Assign {
        lhs: TempId::from_usize(2),
        rhs: AmirRvalue::Unary {
            op: UnaryOp::Deref,
            operand: AmirOperand::Copy(TempId::from_usize(0)),
        },
    });
    let block = AmirBasicBlock {
        id: BlockId::from_usize(0),
        statements: DenseRange::new(0, 3),
        params: vec![],
        terminator: AmirTerminator::Return,
    };
    let blocks = vec![block];
    let cfg = compute_cfg_edges(&blocks);
    let func = AmirFunc {
        symbol: SymbolId::new(0, 0),
        return_type: int,
        receiver: None,
        params: vec![],
        locals: vec![local(0, int)],
        temps: vec![
            temp(0, intern(ArType::RefMut(int))),
            temp(1, intern(ArType::Ref(int))),
            temp(2, int),
        ],
        blocks,
        stmts,
        cfg,
    };
    let diags = check_borrows(&func, &empty_symbols());
    assert!(
        diags
            .iter()
            .any(|d| d.code == DiagCode::O003MutableBorrowConflict),
        "expected O003, got {diags:?}"
    );
}

/// Move of local while `&` live → O002.
#[test]
fn o002_move_while_borrowed() {
    let int = intern(ArType::Primitive(Primitive::Int));
    // Non-copy local of a "struct" so Move matters — use Named non-copy.
    let named = intern(ArType::Named(SymbolId::new(0, 1), vec![]));
    let mut stmts = AmirStmtTable::new();
    // t0 = &s0
    stmts.push(AmirStmt::Assign {
        lhs: TempId::from_usize(0),
        rhs: AmirRvalue::Borrow(place(0)),
    });
    // t1 = Load s0 (non-copy)
    stmts.push(AmirStmt::Assign {
        lhs: TempId::from_usize(1),
        rhs: AmirRvalue::Load(place(0)),
    });
    // consume Move(t1) while t0 still live
    stmts.push(AmirStmt::Assign {
        lhs: TempId::from_usize(2),
        rhs: AmirRvalue::Use(AmirOperand::Move(TempId::from_usize(1))),
    });
    // keep t0 live
    stmts.push(AmirStmt::Assign {
        lhs: TempId::from_usize(3),
        rhs: AmirRvalue::Unary {
            op: UnaryOp::Deref,
            operand: AmirOperand::Copy(TempId::from_usize(0)),
        },
    });
    let block = AmirBasicBlock {
        id: BlockId::from_usize(0),
        statements: DenseRange::new(0, 4),
        params: vec![],
        terminator: AmirTerminator::Return,
    };
    let blocks = vec![block];
    let cfg = compute_cfg_edges(&blocks);
    let mut t1 = temp(1, named);
    t1.is_copy = false;
    let func = AmirFunc {
        symbol: SymbolId::new(0, 0),
        return_type: int,
        receiver: None,
        params: vec![],
        locals: vec![local(0, named)],
        temps: vec![
            temp(0, intern(ArType::Ref(named))),
            t1,
            {
                let mut t = temp(2, named);
                t.is_copy = false;
                t
            },
            temp(3, named),
        ],
        blocks,
        stmts,
        cfg,
    };
    let diags = check_borrows(&func, &empty_symbols());
    assert!(
        diags
            .iter()
            .any(|d| d.code == DiagCode::O002MoveWhileBorrowed),
        "expected O002, got {diags:?}"
    );
}

/// Destroy while borrow live → O006.
#[test]
fn o006_destroy_while_borrowed() {
    let int = intern(ArType::Primitive(Primitive::Int));
    let mut stmts = AmirStmtTable::new();
    stmts.push(AmirStmt::Assign {
        lhs: TempId::from_usize(0),
        rhs: AmirRvalue::Borrow(place(0)),
    });
    stmts.push(AmirStmt::Destroy(place(0)));
    stmts.push(AmirStmt::Assign {
        lhs: TempId::from_usize(1),
        rhs: AmirRvalue::Unary {
            op: UnaryOp::Deref,
            operand: AmirOperand::Copy(TempId::from_usize(0)),
        },
    });
    let block = AmirBasicBlock {
        id: BlockId::from_usize(0),
        statements: DenseRange::new(0, 3),
        params: vec![],
        terminator: AmirTerminator::Return,
    };
    let blocks = vec![block];
    let cfg = compute_cfg_edges(&blocks);
    let func = AmirFunc {
        symbol: SymbolId::new(0, 0),
        return_type: int,
        receiver: None,
        params: vec![],
        locals: vec![local(0, int)],
        temps: vec![temp(0, intern(ArType::Ref(int))), temp(1, int)],
        blocks,
        stmts,
        cfg,
    };
    let diags = check_borrows(&func, &empty_symbols());
    assert!(
        diags
            .iter()
            .any(|d| d.code == DiagCode::O006DestroyWhileBorrowed),
        "expected O006, got {diags:?}"
    );
}

/// Shared + shared is OK.
#[test]
fn shared_shared_ok() {
    let int = intern(ArType::Primitive(Primitive::Int));
    let mut stmts = AmirStmtTable::new();
    stmts.push(AmirStmt::Assign {
        lhs: TempId::from_usize(0),
        rhs: AmirRvalue::Borrow(place(0)),
    });
    stmts.push(AmirStmt::Assign {
        lhs: TempId::from_usize(1),
        rhs: AmirRvalue::Borrow(place(0)),
    });
    stmts.push(AmirStmt::Assign {
        lhs: TempId::from_usize(2),
        rhs: AmirRvalue::Unary {
            op: UnaryOp::Deref,
            operand: AmirOperand::Copy(TempId::from_usize(0)),
        },
    });
    stmts.push(AmirStmt::Assign {
        lhs: TempId::from_usize(3),
        rhs: AmirRvalue::Unary {
            op: UnaryOp::Deref,
            operand: AmirOperand::Copy(TempId::from_usize(1)),
        },
    });
    let block = AmirBasicBlock {
        id: BlockId::from_usize(0),
        statements: DenseRange::new(0, 4),
        params: vec![],
        terminator: AmirTerminator::Return,
    };
    let blocks = vec![block];
    let cfg = compute_cfg_edges(&blocks);
    let ref_ty = intern(ArType::Ref(int));
    let func = AmirFunc {
        symbol: SymbolId::new(0, 0),
        return_type: int,
        receiver: None,
        params: vec![],
        locals: vec![local(0, int)],
        temps: vec![temp(0, ref_ty), temp(1, ref_ty), temp(2, int), temp(3, int)],
        blocks,
        stmts,
        cfg,
    };
    let diags = check_borrows(&func, &empty_symbols());
    assert!(
        diags.is_empty(),
        "shared+shared should be allowed, got {diags:?}"
    );
}

/// Mirror CLI: &mut then & then call using both holders.
#[test]
fn o003_two_loans_then_call() {
    let int = intern(ArType::Primitive(Primitive::Int));
    let mut stmts = AmirStmtTable::new();
    stmts.push(AmirStmt::Store {
        lhs: place(0),
        rhs: AmirOperand::Constant(crate::amir::AmirConstant::Pool(
            crate::literal_pool::LiteralId(0),
        )),
    });
    stmts.push(AmirStmt::Assign {
        lhs: TempId::from_usize(0),
        rhs: AmirRvalue::BorrowMut(place(0)),
    });
    stmts.push(AmirStmt::Assign {
        lhs: TempId::from_usize(1),
        rhs: AmirRvalue::Borrow(place(0)),
    });
    stmts.push(AmirStmt::Call {
        lhs: Some(TempId::from_usize(2)),
        callee: AmirOperand::FunctionRef(SymbolId::new(0, 99)),
        args: smallvec::smallvec![
            AmirOperand::Copy(TempId::from_usize(0)),
            AmirOperand::Copy(TempId::from_usize(1)),
        ],
        return_borrow: None,
    });
    let block = AmirBasicBlock {
        id: BlockId::from_usize(0),
        statements: DenseRange::new(0, 4),
        params: vec![],
        terminator: AmirTerminator::Return,
    };
    let blocks = vec![block];
    let cfg = compute_cfg_edges(&blocks);
    let func = AmirFunc {
        symbol: SymbolId::new(0, 0),
        return_type: int,
        receiver: None,
        params: vec![],
        locals: vec![local(0, int)],
        temps: vec![
            temp(0, intern(ArType::RefMut(int))),
            temp(1, intern(ArType::Ref(int))),
            temp(2, int),
        ],
        blocks,
        stmts,
        cfg,
    };
    let diags = check_borrows(&func, &empty_symbols());
    assert!(
        diags
            .iter()
            .any(|d| d.code == DiagCode::O003MutableBorrowConflict),
        "expected O003, got {diags:?}"
    );
}

/// Mutating a projected field `s0.f` while `s0` is borrowed → O003.
#[test]
fn o003_store_to_projected_field_while_borrowed() {
    let int = intern(ArType::Primitive(Primitive::Int));
    let mut stmts = AmirStmtTable::new();
    // t0 = &s0
    stmts.push(AmirStmt::Assign {
        lhs: TempId::from_usize(0),
        rhs: AmirRvalue::Borrow(place(0)),
    });
    // Store s0.f = 42 (projected place) while t0 is still live!
    let mut projected_place = place(0);
    projected_place
        .projections
        .push(crate::amir::AmirProjection::Field(SymbolId::new(0, 10)));
    stmts.push(AmirStmt::Store {
        lhs: projected_place,
        rhs: AmirOperand::Constant(crate::amir::AmirConstant::Bool(true)),
    });
    // Use t0 after store
    stmts.push(AmirStmt::Assign {
        lhs: TempId::from_usize(1),
        rhs: AmirRvalue::Unary {
            op: UnaryOp::Deref,
            operand: AmirOperand::Copy(TempId::from_usize(0)),
        },
    });
    let block = AmirBasicBlock {
        id: BlockId::from_usize(0),
        statements: DenseRange::new(0, 3),
        params: vec![],
        terminator: AmirTerminator::Return,
    };
    let blocks = vec![block];
    let cfg = compute_cfg_edges(&blocks);
    let func = AmirFunc {
        symbol: SymbolId::new(0, 0),
        return_type: int,
        receiver: None,
        params: vec![],
        locals: vec![local(0, int)],
        temps: vec![temp(0, intern(ArType::Ref(int))), temp(1, int)],
        blocks,
        stmts,
        cfg,
    };
    let diags = check_borrows(&func, &empty_symbols());
    assert!(
        diags
            .iter()
            .any(|d| d.code == DiagCode::O003MutableBorrowConflict),
        "expected O003 for store to field while borrowed, got {diags:?}"
    );
}
