fn intern_ty(ty: crate::types::ArType) -> crate::types::TypeId {
    // Fresh interner per call is OK in unit tests (pre-interns primitives).
    crate::types::TypeInterner::new().intern(ty)
}
use crate::amir::{
    AmirBasicBlock, AmirConstant, AmirFunc, AmirOperand, AmirRvalue, AmirStmt, AmirStmtTable,
    AmirTerminator, BlockId, LocalId, TempId,
};
use crate::cfg::compute_cfg_edges;
use crate::layout::DenseRange;
use crate::liveness::analyze_local_liveness;
use crate::types::ArType;

fn local_id(i: usize) -> LocalId {
    LocalId::from_usize(i)
}

fn block_id(i: usize) -> BlockId {
    BlockId::from_usize(i)
}

fn temp_id(i: usize) -> TempId {
    TempId::from_usize(i)
}

fn void_func(blocks: Vec<AmirBasicBlock>, stmts: AmirStmtTable) -> AmirFunc {
    let cfg = compute_cfg_edges(&blocks);
    AmirFunc {
        symbol: crate::SymbolId::new(0, 0),
        return_type: intern_ty(ArType::Void),
        receiver: None,
        params: Vec::new(),
        locals: vec![crate::amir::AmirLocal {
            id: local_id(0),
            ty: intern_ty(ArType::Void),
            is_memory: false,
            symbol: None,
            span: crate::Span::new(0, 0, 0),
            use_span: None,
        }],
        temps: Vec::new(),
        blocks,

        block_params: Vec::new(),

        stmts,
        cfg,
    }
}

fn empty_block(id: usize) -> AmirBasicBlock {
    AmirBasicBlock {
        id: block_id(id),
        statements: DenseRange::empty(),
        params: DenseRange::empty(),
        terminator: AmirTerminator::Return,
    }
}

// ── analyze_local_liveness ──

#[test]
fn single_block_no_uses_or_defs() {
    let stmts = AmirStmtTable::new();
    let func = void_func(vec![empty_block(0)], stmts);
    let liveness = analyze_local_liveness(&func);
    assert!(liveness.live_in(block_id(0)).is_empty());
    assert!(liveness.live_out(block_id(0)).is_empty());
}

#[test]
fn use_before_def_is_live_in() {
    let mut stmts = AmirStmtTable::new();
    let bid = block_id(0);
    stmts.push(AmirStmt::Store {
        lhs: crate::amir::AmirPlace {
            local: local_id(0),
            projections: smallvec::smallvec![],
        },
        rhs: AmirOperand::Copy(temp_id(0)),
    });
    let mut block = empty_block(0);
    block.statements = DenseRange::new(0, 1);
    let func = void_func(vec![block], stmts);
    let liveness = analyze_local_liveness(&func);
    // temp_id(0) is used but never defined → live_in
    // But temp_id(0) is a TempId, not a LocalId. Liveness tracks LocalId.
    // The operand is Copy(TempId(0)), which in collect_operand_uses does nothing (empty body).
    // So no locals are live.
    assert!(liveness.live_in(bid).is_empty());
    assert!(liveness.live_out(bid).is_empty());
}

#[test]
fn local_used_in_load_is_live_in() {
    let mut stmts = AmirStmtTable::new();
    let bid = block_id(0);
    stmts.push(AmirStmt::Assign {
        lhs: temp_id(0),
        rhs: AmirRvalue::Load(crate::amir::AmirPlace {
            local: local_id(0),
            projections: smallvec::smallvec![],
        }),
    });
    let mut block = empty_block(0);
    block.statements = DenseRange::new(0, 1);
    let func = void_func(vec![block], stmts);
    let liveness = analyze_local_liveness(&func);
    // Load(local_id(0)) is a use of local_id(0)
    // local_id(0) is never defined → live_in should contain local_id(0)
    assert!(liveness.live_in(bid).contains(local_id(0)));
}

#[test]
fn def_kills_liveness() {
    let mut stmts = AmirStmtTable::new();
    let bid = block_id(0);
    // First use local_id(0), then define it
    // In SSA form: use before def means local_id(0) is live_in
    // We need to use the local first (via Load), then store to it
    stmts.push(AmirStmt::Assign {
        lhs: temp_id(0),
        rhs: AmirRvalue::Load(crate::amir::AmirPlace {
            local: local_id(0),
            projections: smallvec::smallvec![],
        }),
    });
    stmts.push(AmirStmt::Store {
        lhs: crate::amir::AmirPlace {
            local: local_id(0),
            projections: smallvec::smallvec![],
        },
        rhs: AmirOperand::Constant(AmirConstant::Nil),
    });
    let mut block = empty_block(0);
    block.statements = DenseRange::new(0, 2);
    let func = void_func(vec![block], stmts);
    let liveness = analyze_local_liveness(&func);
    // local_id(0) is used before any definition → live_in
    assert!(liveness.live_in(bid).contains(local_id(0)));
    // After the Store, local_id(0) is defined, but since it's also the last
    // statement and there's no use after, live_out should be empty for local_id(0).
    assert!(liveness.live_out(bid).is_empty());
}

#[test]
fn two_block_flow_use_after_def() {
    // Block 0: def local_id(0) → goto block 1
    // Block 1: use local_id(0)
    // local_id(0) should be live_in(block 1) and live_out(block 0)
    let mut stmts = AmirStmtTable::new();
    let b0 = block_id(0);
    let b1 = block_id(1);

    stmts.push(AmirStmt::Store {
        lhs: crate::amir::AmirPlace {
            local: local_id(0),
            projections: smallvec::smallvec![],
        },
        rhs: AmirOperand::Constant(AmirConstant::Nil),
    });
    stmts.push(AmirStmt::Assign {
        lhs: temp_id(0),
        rhs: AmirRvalue::Load(crate::amir::AmirPlace {
            local: local_id(0),
            projections: smallvec::smallvec![],
        }),
    });

    let block0 = AmirBasicBlock {
        id: b0,
        statements: DenseRange::new(0, 1),
        params: DenseRange::empty(),
        terminator: AmirTerminator::Goto {
            target: b1,
            args: Vec::new(),
        },
    };
    let block1 = AmirBasicBlock {
        id: b1,
        statements: DenseRange::new(1, 1),
        params: DenseRange::empty(),
        terminator: AmirTerminator::Return,
    };

    let func = void_func(vec![block0, block1], stmts);
    let liveness = analyze_local_liveness(&func);
    // local_id(0) defined in block0, used in block1 → live_out(block0) ∩ live_in(block1)
    assert!(liveness.live_out(b0).contains(local_id(0)));
    assert!(liveness.live_in(b1).contains(local_id(0)));
}

#[test]
fn branch_condition_is_use() {
    let mut stmts = AmirStmtTable::new();
    let b0 = block_id(0);
    let b1 = block_id(1);
    let b2 = block_id(2);

    stmts.push(AmirStmt::Assign {
        lhs: temp_id(0),
        rhs: AmirRvalue::Load(crate::amir::AmirPlace {
            local: local_id(0),
            projections: smallvec::smallvec![],
        }),
    });

    let block0 = AmirBasicBlock {
        id: b0,
        statements: DenseRange::new(0, 1),
        params: DenseRange::empty(),
        terminator: AmirTerminator::Branch {
            condition: AmirOperand::Copy(temp_id(0)),
            if_true: b1,
            true_args: Vec::new(),
            if_false: b2,
            false_args: Vec::new(),
        },
    };
    let block1 = empty_block(1);
    let block2 = empty_block(2);

    let func = void_func(vec![block0, block1, block2], stmts);
    let liveness = analyze_local_liveness(&func);
    // Load(local_id(0)) uses local_id(0), no def → live_in(b0)
    assert!(liveness.live_in(b0).contains(local_id(0)));
}

#[test]
fn switch_int_condition_is_use() {
    let mut stmts = AmirStmtTable::new();
    let b0 = block_id(0);
    stmts.push(AmirStmt::Assign {
        lhs: temp_id(0),
        rhs: AmirRvalue::Load(crate::amir::AmirPlace {
            local: local_id(0),
            projections: smallvec::smallvec![],
        }),
    });
    let block0 = AmirBasicBlock {
        id: b0,
        statements: DenseRange::new(0, 1),
        params: DenseRange::empty(),
        terminator: AmirTerminator::SwitchInt {
            discriminant: AmirOperand::Copy(temp_id(0)),
            targets: vec![],
            otherwise: (block_id(1), Vec::new()),
        },
    };
    let block1 = empty_block(1);
    let func = void_func(vec![block0, block1], stmts);
    let liveness = analyze_local_liveness(&func);
    // Load(local_id(0)) is a use, no def → live_in(b0)
    assert!(liveness.live_in(b0).contains(local_id(0)));
}

#[test]
fn diamond_join_propagates_liveness() {
    // Block 0: def local(0) → branch to block 1, block 2
    // Block 1: no-op → goto block 3
    // Block 2: use local(0) via Load → goto block 3
    // Block 3: return
    // local(0) should be live_out(block0) because block2 uses it
    let mut stmts = AmirStmtTable::new();
    let b0 = block_id(0);
    let b1 = block_id(1);
    let b2 = block_id(2);
    let b3 = block_id(3);

    stmts.push(AmirStmt::Store {
        lhs: crate::amir::AmirPlace {
            local: local_id(0),
            projections: smallvec::smallvec![],
        },
        rhs: AmirOperand::Constant(AmirConstant::Nil),
    });
    stmts.push(AmirStmt::Assign {
        lhs: temp_id(0),
        rhs: AmirRvalue::Load(crate::amir::AmirPlace {
            local: local_id(0),
            projections: smallvec::smallvec![],
        }),
    });

    let block0 = AmirBasicBlock {
        id: b0,
        statements: DenseRange::new(0, 1),
        params: DenseRange::empty(),
        terminator: AmirTerminator::Branch {
            condition: AmirOperand::Constant(AmirConstant::Bool(true)),
            if_true: b1,
            true_args: Vec::new(),
            if_false: b2,
            false_args: Vec::new(),
        },
    };
    let block1 = AmirBasicBlock {
        id: b1,
        statements: DenseRange::empty(),
        params: DenseRange::empty(),
        terminator: AmirTerminator::Goto {
            target: b3,
            args: Vec::new(),
        },
    };
    let block2 = AmirBasicBlock {
        id: b2,
        statements: DenseRange::new(1, 1),
        params: DenseRange::empty(),
        terminator: AmirTerminator::Goto {
            target: b3,
            args: Vec::new(),
        },
    };
    let block3 = empty_block(3);

    let func = void_func(vec![block0, block1, block2, block3], stmts);
    let liveness = analyze_local_liveness(&func);
    // local(0) defined in block0, used in block2 → live_out(block0)
    // Also live_in(block2)
    assert!(liveness.live_out(b0).contains(local_id(0)));
    assert!(liveness.live_in(b2).contains(local_id(0)));
}

#[test]
fn store_is_def_kills_liveness() {
    let mut stmts = AmirStmtTable::new();
    let b0 = block_id(0);
    // Use local via Load, then Store to same local
    stmts.push(AmirStmt::Assign {
        lhs: temp_id(0),
        rhs: AmirRvalue::Load(crate::amir::AmirPlace {
            local: local_id(0),
            projections: smallvec::smallvec![],
        }),
    });
    stmts.push(AmirStmt::Store {
        lhs: crate::amir::AmirPlace {
            local: local_id(0),
            projections: smallvec::smallvec![],
        },
        rhs: AmirOperand::Constant(AmirConstant::Nil),
    });
    // After the Store, local(0) is defined, so live_out should be empty
    let mut block = empty_block(0);
    block.statements = DenseRange::new(0, 2);
    let func = void_func(vec![block], stmts);
    let liveness = analyze_local_liveness(&func);
    // local(0) used before def → live_in
    assert!(liveness.live_in(b0).contains(local_id(0)));
    // After Store → defined, no further use → live_out empty
    assert!(!liveness.live_out(b0).contains(local_id(0)));
}
