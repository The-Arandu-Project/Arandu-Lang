//! Intraprocedural AMIR move checking (M1).
//!
//! This pass tracks whole-local ownership state across the AMIR CFG. It is
//! intentionally conservative for v0.1: projections are treated as reads of the
//! base local and moves are recovered from `Load(place)` followed by consuming
//! `Move(temp)` operands.

#![allow(clippy::collapsible_if)]

use crate::amir::{
    AmirFunc, AmirOperand, AmirPlace, AmirRvalue, AmirStmt, AmirTerminator, BlockId, LocalId,
    TempId, for_each_rvalue_operand, for_each_rvalue_place,
};
use crate::diagnostics::{DiagCode, Diagnostic};
use crate::{BitSet, SymbolTable};
use arandu_lexer::Span;
use std::collections::VecDeque;

/// Sink for block-tagged move diagnostics during CFG walk.
type MoveDiagSink<'a> = Option<(&'a SymbolTable, BlockId, &'a mut Vec<(BlockId, Diagnostic)>)>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LocalMoveState {
    Available,
    Moved,
    MaybeMoved,
}

/// Ownership state used by drop elaboration at a block boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropState {
    Available,
    Moved,
    MaybeMoved,
}

/// Whole-local ownership state after each block, computed by the same
/// transfer function as move diagnostics so cleanup cannot diverge from the
/// move checker.
#[must_use]
pub fn move_states_at_block_exit(func: &AmirFunc) -> Vec<Vec<DropState>> {
    let bump = bumpalo::Bump::new();
    let Some(block_in) = compute_move_in(func, &bump) else {
        return vec![vec![DropState::Available; func.locals.len()]; func.blocks.len()];
    };
    let origins = temp_origins(func, &bump);
    block_in
        .iter()
        .enumerate()
        .map(|(index, incoming)| {
            let mut state = incoming.clone();
            apply_block(BlockId::from_usize(index), func, &origins, &mut state, None);
            func.locals
                .iter()
                .map(|local| match state.get(local.id) {
                    LocalMoveState::Available => DropState::Available,
                    LocalMoveState::Moved => DropState::Moved,
                    LocalMoveState::MaybeMoved => DropState::MaybeMoved,
                })
                .collect()
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct MoveState {
    moved: BitSet<LocalId>,
    maybe_moved: BitSet<LocalId>,
}

impl MoveState {
    fn new(num_locals: usize) -> Self {
        Self {
            moved: BitSet::with_capacity(num_locals),
            maybe_moved: BitSet::with_capacity(num_locals),
        }
    }

    #[tracing::instrument(level = "trace", target = "arandu_mir::move_checker", skip_all)]
    fn join_predecessors<'a>(preds: impl Iterator<Item = &'a Self>, num_locals: usize) -> Self {
        let mut preds = preds.peekable();
        let Some(first) = preds.next() else {
            return Self::new(num_locals);
        };
        let mut acc = first.clone();

        for pred in preds {
            // symmetric difference of moved sets → maybe_moved
            // one BitSet clone instead of two
            let mut sym = acc.moved.clone();
            sym.union_with(&pred.moved);
            acc.moved.intersect_with(&pred.moved);
            sym.difference_with(&acc.moved);

            acc.maybe_moved.union_with(&pred.maybe_moved);
            acc.maybe_moved.union_with(&sym);
        }
        acc
    }

    fn get(&self, local: LocalId) -> LocalMoveState {
        if self.moved.contains(local) {
            LocalMoveState::Moved
        } else if self.maybe_moved.contains(local) {
            LocalMoveState::MaybeMoved
        } else {
            LocalMoveState::Available
        }
    }

    fn set(&mut self, local: LocalId, state: LocalMoveState) {
        match state {
            LocalMoveState::Available => {
                self.moved.remove(local);
                self.maybe_moved.remove(local);
            }
            LocalMoveState::Moved => {
                self.moved.insert(local);
                self.maybe_moved.remove(local);
            }
            LocalMoveState::MaybeMoved => {
                self.moved.remove(local);
                self.maybe_moved.insert(local);
            }
        }
    }
    fn is_monotonic_from(&self, old: &Self) -> bool {
        if !self.moved.is_superset_of(&old.moved) {
            return false;
        }
        for id in old.maybe_moved.iter() {
            if !self.maybe_moved.contains(id) && !self.moved.contains(id) {
                return false;
            }
        }
        true
    }
}

/// Count of locals that are `Moved` or `MaybeMoved` at each block entry.
#[must_use]
pub fn moved_in_counts(func: &AmirFunc) -> Vec<u32> {
    let bump = bumpalo::Bump::new();
    let Some(block_in) = compute_move_in(func, &bump) else {
        return vec![0; func.blocks.len()];
    };
    block_in
        .iter()
        .map(|s| (s.moved.len() + s.maybe_moved.len()) as u32)
        .collect()
}

pub fn check_moves(func: &AmirFunc, symbols: &SymbolTable) -> Vec<Diagnostic> {
    check_moves_by_block(func, symbols)
        .into_iter()
        .map(|(_, d)| d)
        .collect()
}

/// Same as [`check_moves`], tagging each diagnostic with the AMIR block of the use.
#[must_use]
pub fn check_moves_by_block(
    func: &AmirFunc,
    symbols: &SymbolTable,
) -> Vec<(crate::amir::BlockId, Diagnostic)> {
    let bump = bumpalo::Bump::new();
    let Some(block_in) = compute_move_in(func, &bump) else {
        return Vec::new();
    };

    let temp_origins = temp_origins(func, &bump);
    let mut diagnostics = Vec::new();
    let mut block_in = block_in;
    for block in &func.blocks {
        // Take ownership of each IN set — only used once during the check walk.
        let mut state = std::mem::take(&mut block_in[block.id.as_usize()]);
        apply_block(
            block.id,
            func,
            &temp_origins,
            &mut state,
            Some((symbols, block.id, &mut diagnostics)),
        );
    }

    diagnostics
}

fn compute_move_in<'bump>(
    func: &AmirFunc,
    bump: &'bump bumpalo::Bump,
) -> Option<bumpalo::collections::Vec<'bump, MoveState>> {
    let num_locals = func.locals.len();
    let num_blocks = func.blocks.len();

    if num_locals == 0 || num_blocks == 0 {
        return None;
    }

    let temp_origins = temp_origins(func, bump);
    let mut block_in = bumpalo::collections::Vec::with_capacity_in(num_blocks, bump);
    let mut block_out = bumpalo::collections::Vec::with_capacity_in(num_blocks, bump);
    for _ in 0..num_blocks {
        block_in.push(MoveState::new(num_locals));
        block_out.push(MoveState::new(num_locals));
    }
    let mut worklist = VecDeque::new();

    for block in &func.blocks {
        worklist.push_back(block.id);
    }

    let mut iterations = 0;
    let sanity_limit = num_blocks * num_locals * 2 + 1000;

    while let Some(bid) = worklist.pop_front() {
        iterations += 1;
        assert!(
            iterations <= sanity_limit,
            "move checker failed to converge within theoretical limit: {iterations} > {sanity_limit} ({num_blocks} blocks) — possível bug de monotonicidade no dataflow"
        );

        let bi = bid.as_usize();
        let block = &func.blocks[bi];
        let new_in = MoveState::join_predecessors(
            func.predecessors(bid)
                .iter()
                .map(|pred| &block_out[pred.as_usize()]),
            num_locals,
        );
        let mut new_out = new_in.clone();
        apply_block(block.id, func, &temp_origins, &mut new_out, None);

        debug_assert!(
            new_out.is_monotonic_from(&block_out[bi]),
            "Move checker dataflow is not monotonic at block {bi}"
        );

        if new_in != block_in[bi] || new_out != block_out[bi] {
            block_in[bi] = new_in;
            block_out[bi] = new_out;
            for succ in successors(&block.terminator) {
                worklist.push_back(succ);
            }
        }
    }

    Some(block_in)
}

fn temp_origins<'bump>(
    func: &AmirFunc,
    bump: &'bump bumpalo::Bump,
) -> bumpalo::collections::Vec<'bump, Option<LocalId>> {
    let mut origins =
        bumpalo::collections::Vec::from_iter_in(std::iter::repeat_n(None, func.temps.len()), bump);
    for (i, &param_temp) in func.params.iter().enumerate() {
        origins[param_temp.as_usize()] = Some(LocalId::from_usize(i));
    }
    for block in &func.blocks {
        for param in &block.params {
            origins[param.id.as_usize()] = Some(param.local);
        }
    }
    let mut changed = true;
    while changed {
        changed = false;
        for block in &func.blocks {
            for stmt in func.block_stmts(block.id) {
                if let AmirStmt::Assign { lhs, rhs } = stmt {
                    let mut found_origin = None;
                    match rhs {
                        AmirRvalue::Load(place) if place.projections.is_empty() => {
                            found_origin = Some(place.local);
                        }
                        AmirRvalue::Use(AmirOperand::Copy(t) | AmirOperand::Move(t)) => {
                            found_origin = origins[t.as_usize()];
                        }
                        _ => {}
                    }
                    if let Some(loc) = found_origin {
                        if origins[lhs.as_usize()].is_none() {
                            origins[lhs.as_usize()] = Some(loc);
                            changed = true;
                        }
                    }
                }
            }
        }
    }
    origins
}

fn apply_block(
    block: crate::amir::BlockId,
    func: &AmirFunc,
    temp_origins: &[Option<LocalId>],
    state: &mut MoveState,
    mut diagnostics: MoveDiagSink<'_>,
) {
    for stmt in func.block_stmts(block) {
        match stmt {
            AmirStmt::Assign { rhs, .. } => {
                check_rvalue_reads(rhs, func, state, &mut diagnostics);
                consume_rvalue(rhs, func, temp_origins, state, &mut diagnostics);
            }
            AmirStmt::Store { lhs, rhs } => {
                if !lhs.projections.is_empty() {
                    check_place_read(lhs, func, state, &mut diagnostics);
                }
                consume_operand(rhs, func, temp_origins, state, &mut diagnostics, false);
                if lhs.projections.is_empty() {
                    state.set(lhs.local, LocalMoveState::Available);
                }
            }
            AmirStmt::Call { callee, args, .. } => {
                consume_operand(callee, func, temp_origins, state, &mut diagnostics, false);
                for arg in args {
                    consume_operand(arg, func, temp_origins, state, &mut diagnostics, false);
                }
            }
            AmirStmt::Free(op) => {
                consume_operand(op, func, temp_origins, state, &mut diagnostics, true);
            }
            AmirStmt::Destroy(place) => {
                check_consume_place(place, func, state, &mut diagnostics, true);
                state.set(place.local, LocalMoveState::Moved);
            }
            AmirStmt::StorageLive(_) | AmirStmt::StorageDead(_) | AmirStmt::Nop => {}
        }
    }

    match &func.block(block).terminator {
        AmirTerminator::Branch {
            condition,
            true_args,
            false_args,
            ..
        } => {
            check_operand_read(condition, func, temp_origins, state, &mut diagnostics);
            let mut true_state = state.clone();
            let mut false_state = state.clone();

            for arg in true_args {
                consume_operand(
                    arg,
                    func,
                    temp_origins,
                    &mut true_state,
                    &mut diagnostics,
                    false,
                );
            }
            for arg in false_args {
                consume_operand(
                    arg,
                    func,
                    temp_origins,
                    &mut false_state,
                    &mut diagnostics,
                    false,
                );
            }
            *state = MoveState::join_predecessors(
                [&true_state, &false_state].into_iter(),
                func.locals.len(),
            );
        }
        AmirTerminator::SwitchInt {
            discriminant,
            targets,
            otherwise,
            ..
        } => {
            check_operand_read(discriminant, func, temp_origins, state, &mut diagnostics);
            let mut arm_states = Vec::with_capacity(targets.len() + 1);

            for (_, _, args) in targets {
                let mut arm_state = state.clone();
                for arg in args {
                    consume_operand(
                        arg,
                        func,
                        temp_origins,
                        &mut arm_state,
                        &mut diagnostics,
                        false,
                    );
                }
                arm_states.push(arm_state);
            }
            let mut otherwise_state = state.clone();
            for arg in &otherwise.1 {
                consume_operand(
                    arg,
                    func,
                    temp_origins,
                    &mut otherwise_state,
                    &mut diagnostics,
                    false,
                );
            }
            arm_states.push(otherwise_state);

            *state = MoveState::join_predecessors(arm_states.iter(), func.locals.len());
        }
        AmirTerminator::Goto { args, .. } => {
            for arg in args {
                consume_operand(arg, func, temp_origins, state, &mut diagnostics, false);
            }
        }
        AmirTerminator::Suspend { future, args, .. } => {
            check_operand_read(future, func, temp_origins, state, &mut diagnostics);
            for arg in args {
                consume_operand(arg, func, temp_origins, state, &mut diagnostics, false);
            }
        }
        AmirTerminator::Return | AmirTerminator::Unreachable => {}
    }
}

fn check_rvalue_reads(
    rvalue: &AmirRvalue,
    func: &AmirFunc,
    state: &MoveState,
    diagnostics: &mut MoveDiagSink<'_>,
) {
    for_each_rvalue_place(rvalue, |place| {
        check_place_read(place, func, state, diagnostics);
    });
}

fn consume_rvalue(
    rvalue: &AmirRvalue,
    func: &AmirFunc,
    temp_origins: &[Option<LocalId>],
    state: &mut MoveState,
    diagnostics: &mut MoveDiagSink<'_>,
) {
    // Shared visitor covers all operand-bearing rvalues (RC-ANALYSIS-LOAD).
    // Load/Borrow/BorrowMut only contribute Index projection operands, not the base place.
    for_each_rvalue_operand(rvalue, |op| {
        consume_operand(op, func, temp_origins, state, diagnostics, false);
    });
}

fn check_operand_read(
    op: &AmirOperand,
    func: &AmirFunc,
    temp_origins: &[Option<LocalId>],
    state: &MoveState,
    diagnostics: &mut MoveDiagSink<'_>,
) {
    let (AmirOperand::Copy(temp) | AmirOperand::Move(temp)) = op else {
        return;
    };
    if let Some(local) = origin_for(*temp, temp_origins) {
        check_local_read(local, func, state, diagnostics);
    }
}

fn consume_operand(
    op: &AmirOperand,
    func: &AmirFunc,
    temp_origins: &[Option<LocalId>],
    state: &mut MoveState,
    diagnostics: &mut MoveDiagSink<'_>,
    double_free: bool,
) {
    let AmirOperand::Move(temp) = op else {
        check_operand_read(op, func, temp_origins, state, diagnostics);
        return;
    };

    if func.temps[temp.as_usize()].is_copy {
        return;
    }
    let Some(local) = origin_for(*temp, temp_origins) else {
        return;
    };
    check_consume_local(local, func, state, diagnostics, double_free);
    state.set(local, LocalMoveState::Moved);
}

fn check_place_read(
    place: &AmirPlace,
    func: &AmirFunc,
    state: &MoveState,
    diagnostics: &mut MoveDiagSink<'_>,
) {
    check_local_read(place.local, func, state, diagnostics);
}

fn check_consume_place(
    place: &AmirPlace,
    func: &AmirFunc,
    state: &MoveState,
    diagnostics: &mut MoveDiagSink<'_>,
    double_free: bool,
) {
    check_consume_local(place.local, func, state, diagnostics, double_free);
}

fn check_local_read(
    local: LocalId,
    func: &AmirFunc,
    state: &MoveState,
    diagnostics: &mut MoveDiagSink<'_>,
) {
    let Some((symbols, block, diagnostics)) = diagnostics.as_mut() else {
        return;
    };
    let block = *block;
    match state.get(local) {
        LocalMoveState::Available => {}
        LocalMoveState::Moved => diagnostics.push((
            block,
            move_diag(
                DiagCode::O001UseAfterMove,
                local,
                func,
                symbols,
                "use of moved value",
                "value was moved before this use",
            ),
        )),
        LocalMoveState::MaybeMoved => diagnostics.push((
            block,
            move_diag(
                DiagCode::O007InconsistentMoveBetweenBranches,
                local,
                func,
                symbols,
                "value may have been moved on some control-flow paths",
                "ensure all branches leave the value in a consistent ownership state",
            ),
        )),
    }
}

fn check_consume_local(
    local: LocalId,
    func: &AmirFunc,
    state: &MoveState,
    diagnostics: &mut MoveDiagSink<'_>,
    double_free: bool,
) {
    let Some((symbols, block, diagnostics)) = diagnostics.as_mut() else {
        return;
    };
    let block = *block;
    match state.get(local) {
        LocalMoveState::Available => {}
        LocalMoveState::Moved if double_free => diagnostics.push((
            block,
            move_diag(
                DiagCode::O005DoubleFree,
                local,
                func,
                symbols,
                "double free/drop of moved value",
                "value was already consumed on this path",
            ),
        )),
        LocalMoveState::Moved => diagnostics.push((
            block,
            move_diag(
                DiagCode::O001UseAfterMove,
                local,
                func,
                symbols,
                "use of moved value",
                "value was already consumed on this path",
            ),
        )),
        LocalMoveState::MaybeMoved => diagnostics.push((
            block,
            move_diag(
                DiagCode::O007InconsistentMoveBetweenBranches,
                local,
                func,
                symbols,
                "value may have been moved on some control-flow paths",
                "ensure all branches leave the value in a consistent ownership state",
            ),
        )),
    }
}

fn origin_for(temp: TempId, temp_origins: &[Option<LocalId>]) -> Option<LocalId> {
    temp_origins.get(temp.as_usize()).copied().flatten()
}

#[cold]
#[inline(never)]
fn move_diag(
    code: DiagCode,
    local: LocalId,
    func: &AmirFunc,
    symbols: &SymbolTable,
    prefix: &str,
    note: &str,
) -> Diagnostic {
    let name = local_name(local, func, symbols);
    let span = local_diag_span(local, func, symbols);
    Diagnostic::error(code, format!("{prefix} `{name}`"), span).with_note(note)
}

/// Prefer use site → declaration → symbol span → zero (S-SPAN-THREAD).
fn local_diag_span(local: LocalId, func: &AmirFunc, symbols: &SymbolTable) -> Span {
    let Some(l) = func.locals.get(local.as_usize()) else {
        return Span::new(0, 0, 0);
    };
    if let Some(u) = l.use_span {
        if u.start != u.end {
            return u;
        }
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

fn local_name(local: LocalId, func: &AmirFunc, symbols: &SymbolTable) -> String {
    func.locals
        .get(local.as_usize())
        .and_then(|local| local.symbol)
        .map_or_else(
            || format!("s{}", local.as_usize()),
            |symbol| symbols.get(symbol).name.to_string(),
        )
}

fn successors(term: &AmirTerminator) -> Vec<crate::amir::BlockId> {
    match term {
        AmirTerminator::Return | AmirTerminator::Unreachable => Vec::new(),
        AmirTerminator::Goto { target, .. } => vec![*target],
        AmirTerminator::Suspend { resume, .. } => vec![*resume],
        AmirTerminator::Branch {
            if_true, if_false, ..
        } => vec![*if_true, *if_false],
        AmirTerminator::SwitchInt {
            targets, otherwise, ..
        } => {
            let mut out: Vec<_> = targets.iter().map(|(_, block, _)| *block).collect();
            out.push(otherwise.0);
            out
        }
    }
}

#[cfg(test)]
#[path = "move_checker_tests.rs"]
mod tests;
