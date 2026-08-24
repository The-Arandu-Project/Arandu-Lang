#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use arandu_lexer::Span;
use arandu_semantics::DenseRange;
use arandu_semantics::amir::{
    AmirBasicBlock, AmirConstant, AmirFunc, AmirLocal, AmirOperand, AmirPlace, AmirProjection,
    AmirRvalue, AmirStmt, AmirStmtTable, AmirTemp, AmirTerminator, BlockId, BlockParam, Dominators,
    GenArenaDomain, LocalId, TempId, reachable_blocks_dense,
};
use arandu_semantics::literal_pool::AmirLiteralPool;
use arandu_semantics::passes::liveness::analyze_local_liveness;
use arandu_semantics::passes::optimize::optimize_amir_func;
use arandu_semantics::passes::type_checker::types::ArType;
use arandu_semantics::{
    DiagCode, SymbolId, SymbolKind, lower_to_amir, lower_to_hir, resolve_for_test, type_check,
    validate_amir_program,
};

#[test]
fn test_amir_golden_files() {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let root_dir = std::path::Path::new(&manifest_dir)
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let fixtures_dir = root_dir.join("tests").join("codegen");

    if !fixtures_dir.exists() {
        // No fixtures directory = nothing to test
        return;
    }

    let mut entries = Vec::new();
    for entry in std::fs::read_dir(&fixtures_dir).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "aru") {
            entries.push(path);
        }
    }

    entries.sort();

    for path in entries {
        let name = path.file_stem().unwrap().to_str().unwrap();
        let src = std::fs::read_to_string(&path).unwrap();

        let program = arandu_parser::parse(&src).unwrap_or_else(|err| {
            panic!("failed to parse {name}: {err:?}");
        });
        let resolution = resolve_for_test(0, &program);
        let mut tc = type_check(resolution, &program);
        let errors: Vec<_> = tc
            .diagnostics
            .iter()
            .filter(|d| d.severity == arandu_semantics::Severity::Error)
            .collect();
        assert!(
            errors.is_empty(),
            "type check failed for {name}: {errors:?}"
        );
        let hir = lower_to_hir(&mut tc, &program).expect("HIR lowering failed");
        hir.validate_invariants(&hir.pool, &tc.symbols)
            .unwrap_or_else(|err| panic!("HIR invariant validation failed for {name}: {err:?}"));
        let amir = lower_to_amir(&tc, &hir).expect("AMIR lowering failed");
        let amir_issues = validate_amir_program(&amir, &tc.symbols, &tc.type_info.type_interner);
        assert!(
            amir_issues.is_empty(),
            "AMIR validation failed for {name}: {amir_issues:?}"
        );
        let pretty = amir.pretty_print(&tc.symbols, &tc.type_info.type_interner);

        arandu_test_support::assert_golden_text("codegen", name, "amir", &pretty);
    }
}

#[test]
fn field_projection_uses_field_symbol_id() {
    let src = r#"
struct Point {
    x: int
    y: int
}

func main() {
    let p: Point = Point { x: 1, y: 2 }
    p.x = 3
}
"#;
    let program = arandu_parser::parse(src).expect("parse failed");
    let resolution = resolve_for_test(0, &program);
    let mut tc = type_check(resolution, &program);
    let x_symbol = tc
        .symbols
        .iter()
        .find(|symbol| symbol.kind == SymbolKind::Field && symbol.name == "x")
        .map(|symbol| symbol.id)
        .expect("missing field symbol");
    let hir = lower_to_hir(&mut tc, &program).expect("HIR lowering failed");
    let amir = lower_to_amir(&tc, &hir).expect("AMIR lowering failed");

    let func = &amir.funcs[0];
    let has_symbol_projection = func.blocks.iter().any(|block| {
        func.block_stmts(block.id).any(|stmt| match stmt {
            AmirStmt::Store { lhs, .. } => lhs
                .projections
                .iter()
                .any(|projection| matches!(projection, AmirProjection::Field(symbol) if *symbol == x_symbol)),
            _ => false,
        })
    });
    assert!(
        has_symbol_projection,
        "expected p.x store to use field SymbolId"
    );
}

#[test]
fn destructor_drop_is_elaborated_once_at_normal_return() {
    let src = r#"
struct Resource { handle: ptr[u8] }

@Destructor
func Resource.close(own self): void {}

func main() {
    let resource = Resource { handle: nil }
}
"#;
    let program = arandu_parser::parse(src).expect("parse");
    let resolution = resolve_for_test(0, &program);
    let mut tc = type_check(resolution, &program);
    assert!(tc.diagnostics.is_empty(), "{:?}", tc.diagnostics);
    let hir = lower_to_hir(&mut tc, &program).expect("HIR");
    let amir = lower_to_amir(&tc, &hir).expect("AMIR");

    let destructor = tc.type_info.destructors.values().copied().next().unwrap();
    for func in &amir.funcs {
        let destroys = func
            .blocks
            .iter()
            .flat_map(|block| func.block_stmts(block.id))
            .filter(|stmt| matches!(stmt, AmirStmt::Destroy(_)))
            .count();
        if func.symbol == destructor {
            assert_eq!(destroys, 0, "destructor must not recursively drop own self");
        } else if tc.symbols.get(func.symbol).name == "main" {
            assert_eq!(destroys, 1, "live resource must be destroyed exactly once");
        }
    }
}

#[test]
fn non_copy_local_use_after_move_fails_during_amir_analysis() {
    let src = r#"
struct Boxed {
    value: str
}

func main() {
    let a: Boxed = Boxed { value: "one" }
    let b: Boxed = a
    let c: Boxed = a
}
"#;
    let program = arandu_parser::parse(src).expect("parse failed");
    let resolution = resolve_for_test(0, &program);
    let mut tc = type_check(resolution, &program);
    let hir = lower_to_hir(&mut tc, &program).expect("HIR lowering failed");
    let diagnostics = lower_to_amir(&tc, &hir).expect_err("expected use after move diagnostic");

    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == DiagCode::O001UseAfterMove),
        "expected O001 use-after-move diagnostic, got {diagnostics:?}"
    );
}

#[test]
fn copy_local_can_be_reused_during_amir_lowering() {
    let src = r#"
func main() {
    let a: int = 1
    let b: int = a
    let c: int = a
}
"#;
    let program = arandu_parser::parse(src).expect("parse failed");
    let resolution = resolve_for_test(0, &program);
    let mut tc = type_check(resolution, &program);
    let hir = lower_to_hir(&mut tc, &program).expect("HIR lowering failed");
    let amir = lower_to_amir(&tc, &hir).expect("AMIR lowering failed");
    let pretty = amir.pretty_print(&tc.symbols, &tc.type_info.type_interner);
    assert!(
        !pretty.contains("move _"),
        "copy types must not emit move operands"
    );
}

#[test]
fn branch_move_mismatch_reports_o007() {
    let src = r#"
struct Boxed {
    value: str
}

func main(cond: bool) {
    let a: Boxed = Boxed { value: "one" }
    if cond {
        let b: Boxed = a
    }
    let c: Boxed = a
}
"#;
    let program = arandu_parser::parse(src).expect("parse failed");
    let resolution = resolve_for_test(0, &program);
    let mut tc = type_check(resolution, &program);
    let hir = lower_to_hir(&mut tc, &program).expect("HIR lowering failed");
    let diagnostics = lower_to_amir(&tc, &hir).expect_err("expected branch move diagnostic");

    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == DiagCode::O007InconsistentMoveBetweenBranches),
        "expected O007 inconsistent move diagnostic, got {diagnostics:?}"
    );
}

#[test]
fn loop_ssa_simplification_converges() {
    let src = r#"
func main(cond: bool) {
    let mut acc: int = 0
    let mut i: int = 0
    while i < 10 {
        acc = acc + i
        i = i + 1
    }
}
"#;
    let program = arandu_parser::parse(src).expect("parse failed");
    let resolution = resolve_for_test(0, &program);
    let mut tc = type_check(resolution, &program);
    let hir = lower_to_hir(&mut tc, &program).expect("HIR lowering failed");
    let amir = lower_to_amir(&tc, &hir).expect("OSSA lowering failed");

    assert!(!amir.funcs.is_empty());
    let func = &amir.funcs[0];

    // 1. None of the simple Load/Store statements should survive the pruning pass.
    use arandu_middle::amir::{AmirRvalue, AmirStmt};
    for block in &func.blocks {
        for stmt in func.block_stmts(block.id) {
            match stmt {
                AmirStmt::Store { lhs, .. } => {
                    assert!(
                        !lhs.projections.is_empty(),
                        "Store virtual não podado de variável simples: {:?}",
                        stmt
                    );
                }
                AmirStmt::Assign {
                    rhs: AmirRvalue::Load(place),
                    ..
                } => {
                    assert!(
                        !place.projections.is_empty(),
                        "Load virtual não podado de variável simples: {:?}",
                        stmt
                    );
                }
                _ => {}
            }
        }
    }

    // 2. The loop header block must have exactly 2 block-params (acc, i) after trivial phi elimination.
    let loop_header = func
        .blocks
        .iter()
        .find(|b| func.predecessors(b.id).len() > 1)
        .expect("loop header not found");

    assert_eq!(
        loop_header.params.len(),
        2,
        "esperado exatamente 2 block-params (acc, i) no header do loop, achou {}",
        loop_header.params.len()
    );
}

fn empty_block(id: usize, _predecessors: &[usize], successors: &[usize]) -> AmirBasicBlock {
    let term = match successors {
        [] => AmirTerminator::Unreachable,
        &[s] => AmirTerminator::Goto {
            target: BlockId::from_usize(s),
            args: Vec::new(),
        },
        &[t, f] => AmirTerminator::Branch {
            condition: AmirOperand::Constant(AmirConstant::Bool(true)),
            if_true: BlockId::from_usize(t),
            true_args: Vec::new(),
            if_false: BlockId::from_usize(f),
            false_args: Vec::new(),
        },
        _ => panic!("too many successors in test"),
    };
    AmirBasicBlock {
        id: BlockId::from_usize(id),
        statements: DenseRange::empty(),
        params: Vec::new(),
        terminator: term,
    }
}

fn temp(id: usize) -> TempId {
    TempId::from_usize(id)
}

fn local(id: usize) -> LocalId {
    LocalId::from_usize(id)
}

fn place(id: usize) -> AmirPlace {
    AmirPlace {
        local: local(id),
        projections: Default::default(),
    }
}

fn symbol(id: u32) -> SymbolId {
    SymbolId::new(0, id)
}

fn dummy_span() -> Span {
    Span::new(0, 0, 0)
}

fn intern_ty(ty: ArType) -> arandu_middle::types::TypeId {
    arandu_middle::types::TypeInterner::new().intern(ty)
}

fn test_local(id: usize, symbol_id: u32) -> AmirLocal {
    AmirLocal {
        id: local(id),
        symbol: Some(symbol(symbol_id)),
        ty: intern_ty(ArType::Void),
        is_memory: false,
        span: dummy_span(),
        use_span: None,
    }
}

fn test_temp(id: usize) -> AmirTemp {
    AmirTemp {
        id: temp(id),
        ty: intern_ty(ArType::Void),
        is_copy: true,
        is_nullable: false,
        span: dummy_span(),
    }
}

fn test_func(
    locals: Vec<AmirLocal>,
    temps: Vec<AmirTemp>,
    blocks: Vec<AmirBasicBlock>,
    stmts: AmirStmtTable,
) -> AmirFunc {
    let cfg = arandu_semantics::cfg::compute_cfg_edges(&blocks);
    AmirFunc {
        symbol: symbol(0),
        return_type: intern_ty(ArType::Void),
        receiver: None,
        params: Vec::new(),
        locals,
        temps,
        blocks,
        stmts,
        cfg,
    }
}

#[test]
fn dense_reachability_tracks_cfg_without_hash_sets() {
    let mut func = test_func(
        Vec::new(),
        Vec::new(),
        vec![
            empty_block(0, &[], &[1]),
            empty_block(1, &[0], &[2]),
            empty_block(2, &[1], &[]),
            empty_block(3, &[], &[]),
        ],
        AmirStmtTable::new(),
    );
    func.blocks[0].terminator = AmirTerminator::Goto {
        target: BlockId::from_usize(1),
        args: Vec::new(),
    };
    func.blocks[1].terminator = AmirTerminator::Goto {
        target: BlockId::from_usize(2),
        args: Vec::new(),
    };
    func.blocks[2].terminator = AmirTerminator::Return;

    let reachable = reachable_blocks_dense(&func);

    assert!(reachable.contains(BlockId::from_usize(0)));
    assert!(reachable.contains(BlockId::from_usize(1)));
    assert!(reachable.contains(BlockId::from_usize(2)));
    assert!(!reachable.contains(BlockId::from_usize(3)));
}

#[test]
fn dominance_frontiers_are_represented_as_dense_bit_matrix() {
    let func = test_func(
        Vec::new(),
        Vec::new(),
        vec![
            empty_block(0, &[], &[1, 2]),
            empty_block(1, &[0], &[3]),
            empty_block(2, &[0], &[3]),
            empty_block(3, &[1, 2], &[]),
        ],
        AmirStmtTable::new(),
    );
    let doms = Dominators::new(&func);
    let frontiers = doms.frontiers(&func);

    assert!(frontiers.contains(BlockId::from_usize(1), BlockId::from_usize(3)));
    assert!(frontiers.contains(BlockId::from_usize(2), BlockId::from_usize(3)));
    assert!(!frontiers.contains(BlockId::from_usize(0), BlockId::from_usize(3)));
}

#[test]
fn local_liveness_uses_dense_bitsets() {
    let mut stmts = AmirStmtTable::new();
    let first = stmts.push(AmirStmt::Assign {
        lhs: temp(0),
        rhs: AmirRvalue::Load(place(0)),
    });
    let second = stmts.push(AmirStmt::Store {
        lhs: place(1),
        rhs: AmirOperand::Copy(temp(0)),
    });
    let func = test_func(
        vec![test_local(0, 1), test_local(1, 2)],
        vec![test_temp(0)],
        vec![AmirBasicBlock {
            id: BlockId::from_usize(0),
            statements: DenseRange::new(first.as_usize(), second.as_usize() - first.as_usize() + 1),
            params: Vec::new(),
            terminator: AmirTerminator::Return,
        }],
        stmts,
    );

    let liveness = analyze_local_liveness(&func);

    assert!(liveness.live_in(BlockId::from_usize(0)).contains(local(0)));
    assert!(!liveness.live_in(BlockId::from_usize(0)).contains(local(1)));
}

#[test]
fn dce_tracks_used_temps_with_dense_bitsets() {
    let mut stmts = AmirStmtTable::new();
    let first = stmts.push(AmirStmt::Assign {
        lhs: temp(0),
        rhs: AmirRvalue::Use(AmirOperand::Constant(AmirConstant::Bool(true))),
    });
    stmts.push(AmirStmt::Assign {
        lhs: temp(1),
        rhs: AmirRvalue::Use(AmirOperand::Constant(AmirConstant::Bool(false))),
    });
    let func_block = AmirBasicBlock {
        id: BlockId::from_usize(0),
        statements: DenseRange::new(first.as_usize(), 2),
        params: Vec::new(),
        terminator: AmirTerminator::Return,
    };
    let mut func = test_func(
        Vec::new(),
        vec![test_temp(0), test_temp(1)],
        vec![func_block],
        stmts,
    );
    let mut literal_pool = AmirLiteralPool::default();

    optimize_amir_func(&mut func, &mut literal_pool).unwrap();

    let remaining: Vec<_> = func.block_stmt_ids(BlockId::from_usize(0)).collect();
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0], first);
}

#[test]
fn validate_amir_rejects_poison_temp_with_icegen002() {
    use arandu_semantics::DiagCode;

    let func_block = AmirBasicBlock {
        id: BlockId::from_usize(0),
        statements: DenseRange::new(0, 0),
        params: Vec::new(),
        terminator: AmirTerminator::Return,
    };
    // TYP-1: poison Error type must ICE when validated with the interner.
    let interner = arandu_middle::types::TypeInterner::new();
    let mut poison_temp = test_temp(0);
    poison_temp.ty = interner.error_type_id();
    let func = test_func(
        Vec::new(),
        vec![poison_temp],
        vec![func_block],
        AmirStmtTable::new(),
    );
    let mut symbols = arandu_semantics::SymbolTable::new(0);
    symbols
        .define(
            symbols.global_scope(),
            "test_fn",
            SymbolKind::Func,
            dummy_span(),
        )
        .unwrap();
    let issues = arandu_middle::amir_validate::validate_amir_func(&func, &symbols, &interner);
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].code, DiagCode::ICEGEN002);
}

fn validation_symbols() -> arandu_semantics::SymbolTable {
    let mut symbols = arandu_semantics::SymbolTable::new(0);
    symbols
        .define(
            symbols.global_scope(),
            "test_fn",
            SymbolKind::Func,
            dummy_span(),
        )
        .unwrap();
    symbols
}

#[test]
fn validate_amir_rejects_edge_argument_count_mismatch() {
    let interner = arandu_middle::types::TypeInterner::new();
    let ty = interner.intern(ArType::Primitive(arandu_middle::types::Primitive::Int));
    let blocks = vec![
        AmirBasicBlock {
            id: BlockId::from_usize(0),
            statements: DenseRange::empty(),
            params: Vec::new(),
            terminator: AmirTerminator::Goto {
                target: BlockId::from_usize(1),
                args: Vec::new(),
            },
        },
        AmirBasicBlock {
            id: BlockId::from_usize(1),
            statements: DenseRange::empty(),
            params: vec![BlockParam {
                id: temp(0),
                local: local(0),
                ty,
                from: None,
                moved: false,
            }],
            terminator: AmirTerminator::Return,
        },
    ];
    let func = test_func(
        vec![test_local(0, 1)],
        vec![AmirTemp { ty, ..test_temp(0) }],
        blocks,
        AmirStmtTable::new(),
    );

    let issues =
        arandu_middle::amir_validate::validate_amir_func(&func, &validation_symbols(), &interner);
    assert!(
        issues.iter().any(|issue| {
            issue.code == DiagCode::ICEGEN002 && issue.message.contains("SSA-EDGE")
        })
    );
}

#[test]
fn validate_amir_rejects_edge_argument_type_mismatch() {
    let interner = arandu_middle::types::TypeInterner::new();
    let int_ty = interner.intern(ArType::Primitive(arandu_middle::types::Primitive::Int));
    let bool_ty = interner.intern(ArType::Primitive(arandu_middle::types::Primitive::Bool));
    let blocks = vec![
        AmirBasicBlock {
            id: BlockId::from_usize(0),
            statements: DenseRange::empty(),
            params: Vec::new(),
            terminator: AmirTerminator::Goto {
                target: BlockId::from_usize(1),
                args: vec![AmirOperand::Copy(temp(0))],
            },
        },
        AmirBasicBlock {
            id: BlockId::from_usize(1),
            statements: DenseRange::empty(),
            params: vec![BlockParam {
                id: temp(1),
                local: local(0),
                ty: bool_ty,
                from: None,
                moved: false,
            }],
            terminator: AmirTerminator::Return,
        },
    ];
    let func = test_func(
        vec![test_local(0, 1)],
        vec![
            AmirTemp {
                ty: int_ty,
                ..test_temp(0)
            },
            AmirTemp {
                ty: bool_ty,
                ..test_temp(1)
            },
        ],
        blocks,
        AmirStmtTable::new(),
    );

    let issues =
        arandu_middle::amir_validate::validate_amir_func(&func, &validation_symbols(), &interner);
    assert!(
        issues.iter().any(|issue| {
            issue.code == DiagCode::ICEGEN002 && issue.message.contains("SSA-TYPE")
        }),
        "edge operands must have the exact type declared by the destination parameter: {issues:?}"
    );
}

#[test]
fn validate_amir_rejects_block_parameter_temp_type_mismatch() {
    let interner = arandu_middle::types::TypeInterner::new();
    let int_ty = interner.intern(ArType::Primitive(arandu_middle::types::Primitive::Int));
    let bool_ty = interner.intern(ArType::Primitive(arandu_middle::types::Primitive::Bool));
    let blocks = vec![AmirBasicBlock {
        id: BlockId::from_usize(0),
        statements: DenseRange::empty(),
        params: vec![BlockParam {
            id: temp(0),
            local: local(0),
            ty: bool_ty,
            from: None,
            moved: false,
        }],
        terminator: AmirTerminator::Return,
    }];
    let func = test_func(
        vec![test_local(0, 1)],
        vec![AmirTemp {
            ty: int_ty,
            ..test_temp(0)
        }],
        blocks,
        AmirStmtTable::new(),
    );

    let issues =
        arandu_middle::amir_validate::validate_amir_func(&func, &validation_symbols(), &interner);
    assert!(
        issues.iter().any(|issue| {
            issue.code == DiagCode::ICEGEN002 && issue.message.contains("SSA-PARAM")
        }),
        "block parameters and their defining temps must agree on type: {issues:?}"
    );
}

#[test]
fn validate_amir_rejects_inconsistent_gen_payload_and_handle_types() {
    use arandu_middle::types::{Primitive, TypeInterner};

    let interner = TypeInterner::new();
    let int_ty = interner.intern(ArType::Primitive(Primitive::Int));
    let bool_ty = interner.intern(ArType::Primitive(Primitive::Bool));
    let gen_ty = interner.intern(ArType::GenRef);
    let mut stmts = AmirStmtTable::new();
    stmts.push(AmirStmt::Assign {
        lhs: temp(0),
        rhs: AmirRvalue::GenInsert {
            value: AmirOperand::Copy(temp(1)),
            payload_ty: int_ty,
            arena: GenArenaDomain::CompilerManaged,
            origin: dummy_span(),
        },
    });
    stmts.push(AmirStmt::Assign {
        lhs: temp(2),
        rhs: AmirRvalue::GenGet {
            gen_ref: AmirOperand::Copy(temp(1)),
            payload_ty: int_ty,
            arena: GenArenaDomain::CompilerManaged,
            origin: dummy_span(),
        },
    });
    let blocks = vec![AmirBasicBlock {
        id: BlockId::from_usize(0),
        statements: DenseRange::new(0, 2),
        params: Vec::new(),
        terminator: AmirTerminator::Return,
    }];
    let func = AmirFunc {
        symbol: symbol(0),
        return_type: int_ty,
        receiver: None,
        params: Vec::new(),
        locals: Vec::new(),
        temps: vec![
            AmirTemp {
                id: temp(0),
                ty: bool_ty,
                is_copy: true,
                is_nullable: false,
                span: dummy_span(),
            },
            AmirTemp {
                id: temp(1),
                ty: bool_ty,
                is_copy: true,
                is_nullable: false,
                span: dummy_span(),
            },
            AmirTemp {
                id: temp(2),
                ty: gen_ty,
                is_copy: true,
                is_nullable: false,
                span: dummy_span(),
            },
        ],
        cfg: arandu_semantics::cfg::compute_cfg_edges(&blocks),
        blocks,
        stmts,
    };
    let program = arandu_semantics::amir::AmirProgram {
        funcs: vec![func],
        literal_pool: AmirLiteralPool::default(),
        extern_funcs: Default::default(),
    };

    let issues = validate_amir_program(&program, &validation_symbols(), &interner);
    let gen_type_issues = issues
        .iter()
        .filter(|issue| issue.code == DiagCode::ICEGEN002 && issue.message.contains("GEN-TYPE"))
        .count();
    assert_eq!(
        gen_type_issues, 4,
        "all malformed Gen type edges: {issues:?}"
    );
}

#[test]
fn validate_amir_rejects_overlapping_and_out_of_bounds_statement_ranges() {
    let interner = arandu_middle::types::TypeInterner::new();
    let mut stmts = AmirStmtTable::new();
    stmts.push(AmirStmt::Nop);
    let blocks = vec![
        AmirBasicBlock {
            id: BlockId::from_usize(0),
            statements: DenseRange::new(0, 1),
            params: Vec::new(),
            terminator: AmirTerminator::Return,
        },
        AmirBasicBlock {
            id: BlockId::from_usize(1),
            statements: DenseRange::new(0, 1),
            params: Vec::new(),
            terminator: AmirTerminator::Unreachable,
        },
        AmirBasicBlock {
            id: BlockId::from_usize(2),
            statements: DenseRange::new(1, 1),
            params: Vec::new(),
            terminator: AmirTerminator::Unreachable,
        },
    ];
    let func = test_func(Vec::new(), Vec::new(), blocks, stmts);

    let issues =
        arandu_middle::amir_validate::validate_amir_func(&func, &validation_symbols(), &interner);
    let range_issues = issues
        .iter()
        .filter(|issue| issue.message.contains("IR-RANGE"))
        .count();
    assert_eq!(range_issues, 2);
}

#[test]
fn temp_ids_are_dense_and_positional() {
    // Confirma a invariante que a otimização depende: para toda AmirFunc,
    // func.temps[i].id == TempId::from_usize(i) para todo i,
    // e func.locals[i].id == LocalId::from_usize(i) para todo i.
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let root_dir = std::path::Path::new(&manifest_dir)
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let fixtures_dir = root_dir.join("tests").join("codegen");

    if !fixtures_dir.exists() {
        return;
    }

    for entry in std::fs::read_dir(&fixtures_dir).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "aru") {
            let src = std::fs::read_to_string(&path).unwrap();
            let program = arandu_parser::parse(&src).expect("Failed to parse");
            let resolution = resolve_for_test(0, &program);
            let mut tc = type_check(resolution, &program);
            let hir = lower_to_hir(&mut tc, &program).expect("HIR lowering failed");
            let amir = lower_to_amir(&tc, &hir).expect("AMIR lowering failed");

            for func in &amir.funcs {
                for (i, temp) in func.temps.iter().enumerate() {
                    assert_eq!(
                        temp.id.as_usize(),
                        i,
                        "Temp at index {i} in function {} has mismatched TempId {:?}",
                        tc.symbols.get(func.symbol).name,
                        temp.id
                    );
                }
                for (i, local) in func.locals.iter().enumerate() {
                    assert_eq!(
                        local.id.as_usize(),
                        i,
                        "Local at index {i} in function {} has mismatched LocalId {:?}",
                        tc.symbols.get(func.symbol).name,
                        local.id
                    );
                }
            }
        }
    }
}

#[test]
fn path_use_records_nonempty_use_span_on_local() {
    // Full pipeline: reading `x` in `return x` must set AmirLocal.use_span.
    let src = r#"
func main(): int {
    let x = 42
    return x
}
"#;
    let program = arandu_parser::parse(src).expect("parse");
    let resolution = resolve_for_test(0, &program);
    let mut tc = type_check(resolution, &program);
    assert!(
        tc.diagnostics
            .iter()
            .all(|d| d.severity != arandu_semantics::Severity::Error),
        "typeck: {:?}",
        tc.diagnostics
    );
    let hir = lower_to_hir(&mut tc, &program).expect("hir");
    let amir = lower_to_amir(&tc, &hir).expect("amir");
    let func = &amir.funcs[0];
    let x = func
        .locals
        .iter()
        .find(|l| {
            l.symbol
                .map(|s| tc.symbols.get(s).name.as_str() == "x")
                .unwrap_or(false)
        })
        .expect("local x");
    let use_sp = x
        .use_span
        .expect("use_span must be set after path load of x");
    assert!(
        use_sp.start != use_sp.end,
        "use_span must be non-empty, got {use_sp:?}"
    );
    // Path `x` in return appears after the `let x = 42` declaration in the source.
    assert!(
        use_sp.start >= x.span.start,
        "use_span {use_sp:?} should not start before decl {:?}",
        x.span
    );
}
