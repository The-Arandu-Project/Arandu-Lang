//! M2 — Borrow conflict diagnostics (O002 / O003 / O006).
//!
//! Pure checks over F2.1/F2.2 facts: no second analysis engine.

#![allow(clippy::too_many_arguments)]
//!
//! | Code | Rule |
//! |------|------|
//! | **O002** | Move/consume of a local while a loan of that local is active |
//! | **O003** | Conflicting loans (any `&mut` overlap) or mutation under an active loan |
//! | **O006** | `Destroy` of a place while a loan is still active |
//!
//! Shared+shared overlap is allowed. Messages use source vocabulary (name + spans),
//! not internal lattice jargon.

use crate::BitSet;
use crate::amir::{
    AmirFunc, AmirOperand, AmirPlace, AmirRvalue, AmirStmt, AmirTerminator, BlockId, LocalId,
    TempId, for_each_rvalue_operand, for_each_rvalue_place,
};
// for_each_rvalue_place used in reverse_transfer_stmt
use crate::borrow_facts::{FuncBorrowFacts, Loan, LoanKind, ProgramPoint, analyze_borrow_facts};
use crate::diagnostics::{DiagCode, Diagnostic};
use crate::liveness::TempLiveness;
use crate::{Span, SymbolTable};

/// Run M2 borrow checks; return diagnostics (no block tags).
pub fn check_borrows(func: &AmirFunc, symbols: &SymbolTable) -> Vec<Diagnostic> {
    check_borrows_by_block(func, symbols)
        .into_iter()
        .map(|(_, d)| d)
        .collect()
}

/// Same as [`check_borrows`], tagged with the AMIR block of the violation.
#[must_use]
pub fn check_borrows_by_block(
    func: &AmirFunc,
    symbols: &SymbolTable,
) -> Vec<(BlockId, Diagnostic)> {
    if func.blocks.is_empty() || func.locals.is_empty() {
        return Vec::new();
    }

    let facts = analyze_borrow_facts(func);
    let temp_live_before = compute_temp_live_before(func, &facts.temp_live);
    let temp_origins = temp_origins_from_loads(func);

    let mut diags = Vec::new();

    for block in &func.blocks {
        let bi = block.id.as_usize();
        let stmts: Vec<&AmirStmt> = func.block_stmts(block.id).collect();
        let live_before = temp_live_before.get(bi).map(Vec::as_slice).unwrap_or(&[]);

        for (si, stmt) in stmts.iter().enumerate() {
            let live = live_before
                .get(si)
                .cloned()
                .unwrap_or_else(|| facts.temp_live.live_in(block.id).clone());
            check_stmt(
                stmt,
                block.id,
                si,
                &live,
                &facts,
                &temp_origins,
                func,
                symbols,
                &mut diags,
            );
        }

        // Terminator may move values into successors.
        let n = stmts.len();
        let live = if n == 0 {
            facts.temp_live.live_in(block.id).clone()
        } else {
            live_before
                .get(n)
                .cloned()
                .unwrap_or_else(|| facts.temp_live.live_out(block.id).clone())
        };
        check_terminator_moves(
            &block.terminator,
            block.id,
            n,
            &live,
            &facts,
            &temp_origins,
            func,
            symbols,
            &mut diags,
        );
    }

    diags
}

fn check_stmt(
    stmt: &AmirStmt,
    block: BlockId,
    stmt_index: usize,
    live: &BitSet<TempId>,
    facts: &FuncBorrowFacts,
    temp_origins: &[Option<LocalId>],
    func: &AmirFunc,
    symbols: &SymbolTable,
    diags: &mut Vec<(BlockId, Diagnostic)>,
) {
    let point = ProgramPoint { block, stmt_index };

    match stmt {
        AmirStmt::Assign { rhs, .. } => match rhs {
            AmirRvalue::Borrow(place) => {
                check_new_loan(
                    place.local,
                    LoanKind::Shared,
                    point,
                    live,
                    facts,
                    func,
                    symbols,
                    diags,
                );
            }
            AmirRvalue::BorrowMut(place) => {
                check_new_loan(
                    place.local,
                    LoanKind::Exclusive,
                    point,
                    live,
                    facts,
                    func,
                    symbols,
                    diags,
                );
            }
            AmirRvalue::RelativeBorrow { local, mutable } => {
                check_new_loan(
                    *local,
                    if *mutable {
                        LoanKind::Exclusive
                    } else {
                        LoanKind::Shared
                    },
                    point,
                    live,
                    facts,
                    func,
                    symbols,
                    diags,
                );
            }
            other => {
                check_rvalue_moves(
                    other,
                    point,
                    live,
                    facts,
                    temp_origins,
                    func,
                    symbols,
                    diags,
                );
            }
        },
        AmirStmt::Store { lhs, rhs } => {
            // Mutation of the place (whole struct or projected field/element) while borrowed.
            if place_borrowed(lhs.local, live, facts, block) {
                diags.push((
                    block,
                    conflict_diag(
                        lhs.local,
                        func,
                        symbols,
                        facts,
                        live,
                        "mutable borrow conflict",
                        "cannot assign while value is borrowed",
                        block,
                    ),
                ));
            }
            check_operand_move(rhs, point, live, facts, temp_origins, func, symbols, diags);
        }
        AmirStmt::Call { callee, args, .. } => {
            check_operand_move(
                callee,
                point,
                live,
                facts,
                temp_origins,
                func,
                symbols,
                diags,
            );
            for arg in args {
                check_operand_move(arg, point, live, facts, temp_origins, func, symbols, diags);
            }
        }
        AmirStmt::Free(op) => {
            check_operand_move(op, point, live, facts, temp_origins, func, symbols, diags);
        }
        AmirStmt::Destroy(place) => {
            if place_borrowed(place.local, live, facts, block) {
                diags.push((
                    block,
                    destroy_diag(place.local, func, symbols, facts, live, block),
                ));
            }
        }
        AmirStmt::StorageLive(_) | AmirStmt::StorageDead(_) | AmirStmt::Nop => {}
    }
}

fn check_new_loan(
    place: LocalId,
    kind: LoanKind,
    point: ProgramPoint,
    live: &BitSet<TempId>,
    facts: &FuncBorrowFacts,
    func: &AmirFunc,
    symbols: &SymbolTable,
    diags: &mut Vec<(BlockId, Diagnostic)>,
) {
    let active_shared = active_kind(place, LoanKind::Shared, live, facts, point.block);
    let active_excl = active_kind(place, LoanKind::Exclusive, live, facts, point.block);

    let conflict = match kind {
        // `&` conflicts only with active `&mut`
        LoanKind::Shared => active_excl,
        // `&mut` conflicts with any active loan
        LoanKind::Exclusive => active_shared || active_excl,
    };

    if conflict {
        diags.push((
            point.block,
            conflict_diag(
                place,
                func,
                symbols,
                facts,
                live,
                "mutable borrow conflict",
                match kind {
                    LoanKind::Shared => "cannot borrow as shared while exclusively borrowed",
                    LoanKind::Exclusive => "cannot borrow as mutable while already borrowed",
                },
                point.block,
            ),
        ));
    }
}

fn check_rvalue_moves(
    rhs: &AmirRvalue,
    point: ProgramPoint,
    live: &BitSet<TempId>,
    facts: &FuncBorrowFacts,
    temp_origins: &[Option<LocalId>],
    func: &AmirFunc,
    symbols: &SymbolTable,
    diags: &mut Vec<(BlockId, Diagnostic)>,
) {
    for_each_rvalue_operand(rhs, |op| {
        check_operand_move(op, point, live, facts, temp_origins, func, symbols, diags);
    });
}

fn check_terminator_moves(
    term: &AmirTerminator,
    block: BlockId,
    stmt_index: usize,
    live: &BitSet<TempId>,
    facts: &FuncBorrowFacts,
    temp_origins: &[Option<LocalId>],
    func: &AmirFunc,
    symbols: &SymbolTable,
    diags: &mut Vec<(BlockId, Diagnostic)>,
) {
    let point = ProgramPoint { block, stmt_index };
    match term {
        AmirTerminator::Branch {
            condition,
            true_args,
            false_args,
            ..
        } => {
            check_operand_move(
                condition,
                point,
                live,
                facts,
                temp_origins,
                func,
                symbols,
                diags,
            );
            for a in true_args {
                check_operand_move(a, point, live, facts, temp_origins, func, symbols, diags);
            }
            for a in false_args {
                check_operand_move(a, point, live, facts, temp_origins, func, symbols, diags);
            }
        }
        AmirTerminator::SwitchInt {
            discriminant,
            targets,
            otherwise,
            ..
        } => {
            check_operand_move(
                discriminant,
                point,
                live,
                facts,
                temp_origins,
                func,
                symbols,
                diags,
            );
            for (_, _, args) in targets {
                for a in args {
                    check_operand_move(a, point, live, facts, temp_origins, func, symbols, diags);
                }
            }
            for a in &otherwise.1 {
                check_operand_move(a, point, live, facts, temp_origins, func, symbols, diags);
            }
        }
        AmirTerminator::Goto { args, .. } => {
            for a in args {
                check_operand_move(a, point, live, facts, temp_origins, func, symbols, diags);
            }
        }
        AmirTerminator::Suspend { future, args, .. } => {
            check_operand_move(
                future,
                point,
                live,
                facts,
                temp_origins,
                func,
                symbols,
                diags,
            );
            for a in args {
                check_operand_move(a, point, live, facts, temp_origins, func, symbols, diags);
            }
        }
        AmirTerminator::Return | AmirTerminator::Unreachable => {}
    }
}

fn check_operand_move(
    op: &AmirOperand,
    point: ProgramPoint,
    live: &BitSet<TempId>,
    facts: &FuncBorrowFacts,
    temp_origins: &[Option<LocalId>],
    func: &AmirFunc,
    symbols: &SymbolTable,
    diags: &mut Vec<(BlockId, Diagnostic)>,
) {
    let AmirOperand::Move(temp) = op else {
        return;
    };
    if func.temps.get(temp.as_usize()).is_some_and(|t| t.is_copy) {
        return;
    }
    let Some(local) = temp_origins.get(temp.as_usize()).copied().flatten() else {
        return;
    };
    if place_borrowed(local, live, facts, point.block) {
        diags.push((
            point.block,
            move_while_borrowed_diag(local, func, symbols, facts, live, point.block),
        ));
    }
}

fn place_borrowed(
    local: LocalId,
    live: &BitSet<TempId>,
    facts: &FuncBorrowFacts,
    current_block: BlockId,
) -> bool {
    active_kind(local, LoanKind::Shared, live, facts, current_block)
        || active_kind(local, LoanKind::Exclusive, live, facts, current_block)
}

fn active_kind(
    local: LocalId,
    kind: LoanKind,
    live: &BitSet<TempId>,
    facts: &FuncBorrowFacts,
    current_block: BlockId,
) -> bool {
    for loan in &facts.loans {
        if loan.place_local != local || loan.kind != kind {
            continue;
        }
        if loan_holders_live(loan, live, facts, current_block) {
            return true;
        }
    }
    false
}

fn loan_holders_live(
    loan: &Loan,
    live: &BitSet<TempId>,
    facts: &FuncBorrowFacts,
    current_block: BlockId,
) -> bool {
    for t in loan.holder_temps.iter() {
        if live.contains(t) {
            return true;
        }
    }
    for l in loan.holder_locals.iter() {
        if facts.local_live.live_in(current_block).contains(l)
            || facts.local_live.live_out(current_block).contains(l)
        {
            return true;
        }
    }
    false
}

/// Per-block: `live_before[si]` = temps live immediately before statement `si`.
/// Extra slot `live_before[n]` = live before terminator (= live-out after reverse walk).
fn compute_temp_live_before(func: &AmirFunc, temp_live: &TempLiveness) -> Vec<Vec<BitSet<TempId>>> {
    let num_temps = func.temps.len();
    let mut result = Vec::with_capacity(func.blocks.len());

    for block in &func.blocks {
        let stmts: Vec<&AmirStmt> = func.block_stmts(block.id).collect();
        let n = stmts.len();
        let mut live_before = vec![BitSet::with_capacity(num_temps); n + 1];

        let mut live = temp_live.live_out(block.id).clone();
        // `Return` consumes the conventional return register (`TempId(0)`).
        // The terminator has no explicit operand in AMIR, so seed it here or a
        // returned borrow appears dead before epilogue `Destroy` statements.
        if matches!(block.terminator, AmirTerminator::Return) && !func.temps.is_empty() {
            live.insert(TempId::from_usize(0));
        }
        // Before terminator:
        live_before[n] = live.clone();
        // Reverse statements.
        for si in (0..n).rev() {
            // live = live after stmt si, before killing def / adding uses of si
            // After reverse transfer, live = live before stmt si.
            reverse_transfer_stmt(stmts[si], &mut live, num_temps);
            live_before[si] = live.clone();
        }
        // Optionally union with live_in for safety (phi / incomplete use info).
        live_before[0].union_with(temp_live.live_in(block.id));

        result.push(live_before);
    }
    result
}

fn reverse_transfer_stmt(stmt: &AmirStmt, live: &mut BitSet<TempId>, _num_temps: usize) {
    // Reverse: first undo def, then add uses (backward dataflow step).
    match stmt {
        AmirStmt::Assign { lhs, rhs } => {
            live.remove(*lhs);
            for_each_rvalue_operand(rhs, |op| mark_use(op, live));
            for_each_rvalue_place(rhs, |place| mark_place_index_uses(place, live));
        }
        AmirStmt::Store { lhs, rhs } => {
            mark_use(rhs, live);
            mark_place_index_uses(lhs, live);
        }
        AmirStmt::Call {
            lhs, callee, args, ..
        } => {
            if let Some(t) = lhs {
                live.remove(*t);
            }
            mark_use(callee, live);
            for a in args {
                mark_use(a, live);
            }
        }
        AmirStmt::Free(op) => mark_use(op, live),
        AmirStmt::Destroy(place) => mark_place_index_uses(place, live),
        AmirStmt::StorageLive(_) | AmirStmt::StorageDead(_) | AmirStmt::Nop => {}
    }
}

fn mark_use(op: &AmirOperand, live: &mut BitSet<TempId>) {
    if let AmirOperand::Copy(t) | AmirOperand::Move(t) = op {
        live.insert(*t);
    }
}

fn mark_place_index_uses(place: &AmirPlace, live: &mut BitSet<TempId>) {
    for proj in &place.projections {
        if let crate::amir::AmirProjection::Index(op) = proj {
            mark_use(op, live);
        }
    }
}

/// Map each temp to a stack local origin when it is a Load of that local (move path).
fn temp_origins_from_loads(func: &AmirFunc) -> Vec<Option<LocalId>> {
    let mut origins = vec![None; func.temps.len()];
    let mut changed = true;
    let mut iterations = 0;
    while changed && iterations < 100 {
        changed = false;
        iterations += 1;
        for block in &func.blocks {
            for stmt in func.block_stmts(block.id) {
                if let AmirStmt::Assign {
                    lhs,
                    rhs: AmirRvalue::Load(place),
                } = stmt
                    && place.projections.is_empty()
                {
                    let old = origins[lhs.as_usize()];
                    let new = Some(place.local);
                    if old != new {
                        origins[lhs.as_usize()] = new;
                        changed = true;
                    }
                }
                if let AmirStmt::Assign {
                    lhs,
                    rhs: AmirRvalue::Use(AmirOperand::Copy(t) | AmirOperand::Move(t)),
                } = stmt
                {
                    let old = origins[lhs.as_usize()];
                    let new = origins[t.as_usize()];
                    if old != new {
                        origins[lhs.as_usize()] = new;
                        changed = true;
                    }
                }
            }
        }
    }
    origins
}

fn local_name(local: LocalId, func: &AmirFunc, symbols: &SymbolTable) -> String {
    func.locals
        .get(local.as_usize())
        .and_then(|l| l.symbol)
        .map_or_else(
            || format!("_{}", local.as_usize()),
            |sym| symbols.get(sym).name.to_string(),
        )
}

fn local_span(local: LocalId, func: &AmirFunc, symbols: &SymbolTable) -> Span {
    let Some(l) = func.locals.get(local.as_usize()) else {
        return Span::new(0, 0, 0);
    };
    if let Some(u) = l.use_span
        && u.start != u.end
    {
        return u;
    }
    if l.span.start != l.span.end {
        return l.span;
    }
    if let Some(sym) = l.symbol {
        let s = symbols.get(sym).span;
        if s.start != s.end {
            return s;
        }
    }
    Span::new(0, 0, 0)
}

fn first_loan_span(
    local: LocalId,
    facts: &FuncBorrowFacts,
    live: &BitSet<TempId>,
    func: &AmirFunc,
    symbols: &SymbolTable,
    current_block: BlockId,
) -> Span {
    for loan in &facts.loans {
        if loan.place_local != local {
            continue;
        }
        if !loan_holders_live(loan, live, facts, current_block) {
            continue;
        }
        // Prefer a holder temp span.
        for t in loan.holder_temps.iter() {
            if live.contains(t)
                && let Some(temp) = func.temps.get(t.as_usize())
                && temp.span.start != temp.span.end
            {
                return temp.span;
            }
        }
    }
    local_span(local, func, symbols)
}

fn move_while_borrowed_diag(
    local: LocalId,
    func: &AmirFunc,
    symbols: &SymbolTable,
    facts: &FuncBorrowFacts,
    live: &BitSet<TempId>,
    current_block: BlockId,
) -> Diagnostic {
    let name = local_name(local, func, symbols);
    let span = local_span(local, func, symbols);
    let origin = first_loan_span(local, facts, live, func, symbols, current_block);
    Diagnostic::error(
        DiagCode::O002MoveWhileBorrowed,
        format!("cannot move '{name}' while borrowed"),
        span,
    )
    .with_label(origin, "borrow is still active from here")
    .with_note("end all uses of the reference before moving the owner")
}

fn destroy_diag(
    local: LocalId,
    func: &AmirFunc,
    symbols: &SymbolTable,
    facts: &FuncBorrowFacts,
    live: &BitSet<TempId>,
    current_block: BlockId,
) -> Diagnostic {
    let name = local_name(local, func, symbols);
    let span = local_span(local, func, symbols);
    let origin = first_loan_span(local, facts, live, func, symbols, current_block);
    Diagnostic::error(
        DiagCode::O006DestroyWhileBorrowed,
        format!("cannot destroy '{name}' while borrowed"),
        span,
    )
    .with_label(origin, "borrow is still active from here")
    .with_note("ending the owner while a reference is live would create a dangling pointer")
}

fn conflict_diag(
    local: LocalId,
    func: &AmirFunc,
    symbols: &SymbolTable,
    facts: &FuncBorrowFacts,
    live: &BitSet<TempId>,
    _prefix: &str,
    note: &str,
    current_block: BlockId,
) -> Diagnostic {
    let name = local_name(local, func, symbols);
    let span = local_span(local, func, symbols);
    let origin = first_loan_span(local, facts, live, func, symbols, current_block);
    Diagnostic::error(
        DiagCode::O003MutableBorrowConflict,
        format!("mutable borrow conflict on '{name}'"),
        span,
    )
    .with_label(origin, "previous borrow is still active")
    .with_note(note)
}

#[cfg(test)]
#[path = "borrow_check_tests.rs"]
mod tests;
