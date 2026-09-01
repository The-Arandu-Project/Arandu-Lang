//! Intraprocedural liveness analysis (locals + SSA temps).
//!
//! - [`analyze_local_liveness`]: stack locals (register allocation / OSSA).
//! - [`analyze_temp_liveness`]: SSA temps — **F2.2** reuses this so a loan's
//!   window equals the live range of the reference value that holds it.

use std::collections::VecDeque;

use crate::amir::reachability::terminator_targets;
use crate::amir::{
    AmirFunc, AmirOperand, AmirPlace, AmirProjection, AmirRvalue, AmirStmt, AmirTerminator,
    BlockId, LocalId, TempId, for_each_rvalue_operand, for_each_rvalue_place,
    for_each_terminator_operand,
};
use crate::{BitMatrix, BitSet};

/// Liveness query results for all local variables within a single function.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalLiveness {
    live_in: Vec<BitSet<LocalId>>,
    live_out: Vec<BitSet<LocalId>>,
}

impl LocalLiveness {
    /// Returns the set of local variables that are live at the entry of the given block.
    #[must_use]
    pub fn live_in(&self, block: BlockId) -> &BitSet<LocalId> {
        &self.live_in[block.as_usize()]
    }

    /// Returns the set of local variables that are live at the exit of the given block.
    #[must_use]
    pub fn live_out(&self, block: BlockId) -> &BitSet<LocalId> {
        &self.live_out[block.as_usize()]
    }
}

/// Liveness of SSA temps (per-block live-in / live-out).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TempLiveness {
    live_in: Vec<BitSet<TempId>>,
    live_out: Vec<BitSet<TempId>>,
}

impl TempLiveness {
    #[must_use]
    pub fn live_in(&self, block: BlockId) -> &BitSet<TempId> {
        &self.live_in[block.as_usize()]
    }

    #[must_use]
    pub fn live_out(&self, block: BlockId) -> &BitSet<TempId> {
        &self.live_out[block.as_usize()]
    }
}

/// Runs intraprocedural liveness analysis for all local variables in the function.
///
/// Uses a backward dataflow analysis over the CFG.
#[must_use]
pub fn analyze_local_liveness(func: &AmirFunc) -> LocalLiveness {
    let num_blocks = func.blocks.len();
    let num_locals = func.locals.len();
    let mut block_uses = BitMatrix::<BlockId, LocalId>::new(num_blocks, num_locals);
    let mut block_defs = BitMatrix::<BlockId, LocalId>::new(num_blocks, num_locals);

    for block in &func.blocks {
        let mut defined = BitSet::<LocalId>::with_capacity(num_locals);
        for stmt in func.block_stmts(block.id) {
            collect_stmt_uses(stmt, &defined, &mut block_uses, block.id);
            collect_stmt_defs(stmt, &mut defined, &mut block_defs, block.id);
        }
        collect_terminator_uses(&block.terminator, &defined, &mut block_uses, block.id);
    }

    let mut live_in = vec![BitSet::<LocalId>::with_capacity(num_locals); num_blocks];
    let mut live_out = vec![BitSet::<LocalId>::with_capacity(num_locals); num_blocks];

    let rpo = crate::amir::reverse_post_order(func);

    let mut in_worklist = vec![false; num_blocks];
    let mut worklist = VecDeque::new();
    for &block_id in rpo.iter().rev() {
        worklist.push_back(block_id);
        in_worklist[block_id.as_usize()] = true;
    }

    let mut new_out = BitSet::<LocalId>::with_capacity(num_locals);
    let mut new_in = BitSet::<LocalId>::with_capacity(num_locals);

    let max_iterations =
        num_blocks * num_locals + crate::analysis_limits::DATAFLOW_FIXPOINT_HEADROOM;
    let mut iterations = 0;

    while let Some(block_id) = worklist.pop_front() {
        iterations += 1;
        let index = block_id.as_usize();
        in_worklist[index] = false;

        assert!(
            iterations <= max_iterations,
            "liveness analysis failed to converge within theoretical limit: {iterations} > {max_iterations}"
        );

        let block = &func.blocks[index];

        new_out.clear();
        for successor in terminator_targets(&block.terminator) {
            new_out.union_with(&live_in[successor.as_usize()]);
        }

        new_in.clone_from(&new_out);
        new_in.difference_with(&block_defs.row_set(block_id));
        new_in.union_with(&block_uses.row_set(block_id));

        if new_in != live_in[index] || new_out != live_out[index] {
            live_in[index].clone_from(&new_in);
            live_out[index].clone_from(&new_out);

            for &pred in func.predecessors(block_id) {
                let pred_index = pred.as_usize();
                if !in_worklist[pred_index] {
                    worklist.push_back(pred);
                    in_worklist[pred_index] = true;
                }
            }
        }
    }

    LocalLiveness { live_in, live_out }
}

/// Backward dataflow: which SSA temps are live-in / live-out per block (F2.2).
#[must_use]
pub fn analyze_temp_liveness(func: &AmirFunc) -> TempLiveness {
    let num_blocks = func.blocks.len();
    let num_temps = func.temps.len();
    let mut block_uses = BitMatrix::<BlockId, TempId>::new(num_blocks, num_temps);
    let mut block_defs = BitMatrix::<BlockId, TempId>::new(num_blocks, num_temps);

    for block in &func.blocks {
        let mut defined = BitSet::<TempId>::with_capacity(num_temps);
        // Block params are defs at entry (before body uses).
        for param in &block.params {
            defined.insert(param.id);
            block_defs.insert(block.id, param.id);
        }
        for stmt in func.block_stmts(block.id) {
            collect_stmt_temp_uses(stmt, &defined, &mut block_uses, block.id);
            collect_stmt_temp_defs(stmt, &mut defined, &mut block_defs, block.id);
        }
        collect_terminator_temp_uses(&block.terminator, &defined, &mut block_uses, block.id);
    }

    let mut live_in = vec![BitSet::<TempId>::with_capacity(num_temps); num_blocks];
    let mut live_out = vec![BitSet::<TempId>::with_capacity(num_temps); num_blocks];

    let rpo = crate::amir::reverse_post_order(func);

    let mut in_worklist = vec![false; num_blocks];
    let mut worklist = VecDeque::new();
    for &block_id in rpo.iter().rev() {
        worklist.push_back(block_id);
        in_worklist[block_id.as_usize()] = true;
    }

    let mut new_out = BitSet::<TempId>::with_capacity(num_temps);
    let mut new_in = BitSet::<TempId>::with_capacity(num_temps);

    let max_iterations =
        num_blocks * num_temps + crate::analysis_limits::DATAFLOW_FIXPOINT_HEADROOM;
    let mut iterations = 0;

    while let Some(block_id) = worklist.pop_front() {
        iterations += 1;
        let index = block_id.as_usize();
        in_worklist[index] = false;

        assert!(
            iterations <= max_iterations,
            "liveness analysis failed to converge within theoretical limit: {iterations} > {max_iterations}"
        );

        let block = &func.blocks[index];

        new_out.clear();
        for successor in terminator_targets(&block.terminator) {
            new_out.union_with(&live_in[successor.as_usize()]);
        }

        new_in.clone_from(&new_out);
        new_in.difference_with(&block_defs.row_set(block_id));
        new_in.union_with(&block_uses.row_set(block_id));

        if new_in != live_in[index] || new_out != live_out[index] {
            live_in[index].clone_from(&new_in);
            live_out[index].clone_from(&new_out);

            for &pred in func.predecessors(block_id) {
                let pred_index = pred.as_usize();
                if !in_worklist[pred_index] {
                    worklist.push_back(pred);
                    in_worklist[pred_index] = true;
                }
            }
        }
    }

    TempLiveness { live_in, live_out }
}

fn collect_stmt_temp_uses(
    stmt: &AmirStmt,
    defined: &BitSet<TempId>,
    uses: &mut BitMatrix<BlockId, TempId>,
    block: BlockId,
) {
    match stmt {
        AmirStmt::Assign { rhs, .. } => {
            for_each_rvalue_operand(rhs, |op| mark_temp_use(op, defined, uses, block));
            for_each_rvalue_place(rhs, |place| {
                for proj in &place.projections {
                    if let AmirProjection::Index(op) = proj {
                        mark_temp_use(op, defined, uses, block);
                    }
                }
            });
        }
        AmirStmt::Store { lhs, rhs } => {
            mark_temp_use(rhs, defined, uses, block);
            for proj in &lhs.projections {
                if let AmirProjection::Index(op) = proj {
                    mark_temp_use(op, defined, uses, block);
                }
            }
        }
        AmirStmt::Call { callee, args, .. } => {
            mark_temp_use(callee, defined, uses, block);
            for arg in args {
                mark_temp_use(arg, defined, uses, block);
            }
        }
        AmirStmt::Free(op) => mark_temp_use(op, defined, uses, block),
        AmirStmt::Destroy(place) => {
            for proj in &place.projections {
                if let AmirProjection::Index(op) = proj {
                    mark_temp_use(op, defined, uses, block);
                }
            }
        }
        AmirStmt::StorageLive(_) | AmirStmt::StorageDead(_) | AmirStmt::Nop => {}
    }
}

fn collect_stmt_temp_defs(
    stmt: &AmirStmt,
    defined: &mut BitSet<TempId>,
    defs: &mut BitMatrix<BlockId, TempId>,
    block: BlockId,
) {
    match stmt {
        AmirStmt::Assign { lhs, .. } => {
            defined.insert(*lhs);
            defs.insert(block, *lhs);
        }
        AmirStmt::Call { lhs: Some(t), .. } => {
            defined.insert(*t);
            defs.insert(block, *t);
        }
        _ => {}
    }
}

fn collect_terminator_temp_uses(
    term: &AmirTerminator,
    defined: &BitSet<TempId>,
    uses: &mut BitMatrix<BlockId, TempId>,
    block: BlockId,
) {
    // Shared visitor: conditions + all jump args (same contract as DCE).
    for_each_terminator_operand(term, |op| mark_temp_use(op, defined, uses, block));
}

fn mark_temp_use(
    op: &AmirOperand,
    defined: &BitSet<TempId>,
    uses: &mut BitMatrix<BlockId, TempId>,
    block: BlockId,
) {
    if let AmirOperand::Copy(t) | AmirOperand::Move(t) = op
        && !defined.contains(*t)
    {
        uses.insert(block, *t);
    }
}

fn collect_stmt_uses(
    stmt: &AmirStmt,
    defined: &BitSet<LocalId>,
    uses: &mut BitMatrix<BlockId, LocalId>,
    block: BlockId,
) {
    match stmt {
        AmirStmt::Assign { rhs, .. } => collect_rvalue_uses(rhs, defined, uses, block),
        AmirStmt::Store { lhs, rhs } => {
            if !lhs.projections.is_empty() {
                collect_place_use(lhs, defined, uses, block);
            } else {
                collect_projection_uses(lhs, defined, uses, block);
            }
            collect_operand_uses(rhs, defined, uses, block);
        }
        AmirStmt::Call { callee, args, .. } => {
            collect_operand_uses(callee, defined, uses, block);
            for arg in args {
                collect_operand_uses(arg, defined, uses, block);
            }
        }
        AmirStmt::Free(op) => collect_operand_uses(op, defined, uses, block),
        AmirStmt::Destroy(place) => collect_place_use(place, defined, uses, block),
        AmirStmt::StorageLive(_) | AmirStmt::StorageDead(_) | AmirStmt::Nop => {}
    }
}

fn collect_stmt_defs(
    stmt: &AmirStmt,
    defined: &mut BitSet<LocalId>,
    defs: &mut BitMatrix<BlockId, LocalId>,
    block: BlockId,
) {
    if let AmirStmt::Store { lhs, .. } = stmt
        && lhs.projections.is_empty()
    {
        defined.insert(lhs.local);
        defs.insert(block, lhs.local);
    }
}

fn collect_terminator_uses(
    term: &AmirTerminator,
    defined: &BitSet<LocalId>,
    uses: &mut BitMatrix<BlockId, LocalId>,
    block: BlockId,
) {
    match term {
        AmirTerminator::Branch { condition, .. } => {
            collect_operand_uses(condition, defined, uses, block);
        }
        AmirTerminator::SwitchInt { discriminant, .. } => {
            collect_operand_uses(discriminant, defined, uses, block);
        }
        AmirTerminator::Suspend { future, args, .. } => {
            collect_operand_uses(future, defined, uses, block);
            for a in args {
                collect_operand_uses(a, defined, uses, block);
            }
        }
        AmirTerminator::Return | AmirTerminator::Goto { .. } | AmirTerminator::Unreachable => {}
    }
}

fn collect_rvalue_uses(
    rvalue: &AmirRvalue,
    defined: &BitSet<LocalId>,
    uses: &mut BitMatrix<BlockId, LocalId>,
    block: BlockId,
) {
    // Shared visitor: places (Load/Borrow) and any nested operands (RC-ANALYSIS-LOAD).
    for_each_rvalue_place(rvalue, |place| {
        collect_place_use(place, defined, uses, block);
    });
    for_each_rvalue_operand(rvalue, |op| {
        collect_operand_uses(op, defined, uses, block);
    });
}

fn collect_place_use(
    place: &AmirPlace,
    defined: &BitSet<LocalId>,
    uses: &mut BitMatrix<BlockId, LocalId>,
    block: BlockId,
) {
    if !defined.contains(place.local) {
        uses.insert(block, place.local);
    }
    collect_projection_uses(place, defined, uses, block);
}

fn collect_projection_uses(
    place: &AmirPlace,
    defined: &BitSet<LocalId>,
    uses: &mut BitMatrix<BlockId, LocalId>,
    block: BlockId,
) {
    for projection in &place.projections {
        if let AmirProjection::Index(op) = projection {
            collect_operand_uses(op, defined, uses, block);
        }
    }
}

fn collect_operand_uses(
    _op: &AmirOperand,
    _defined: &BitSet<LocalId>,
    _uses: &mut BitMatrix<BlockId, LocalId>,
    _block: BlockId,
) {
}

#[cfg(test)]
#[path = "liveness_tests.rs"]
mod tests;
