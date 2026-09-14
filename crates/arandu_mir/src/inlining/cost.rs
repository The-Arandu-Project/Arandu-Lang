//! Eligibility and cost evaluation for AMIR leaf function inlining.

use crate::amir::{AmirFunc, AmirStmt, AmirTerminator, BlockId};

/// Default maximum statement/operation budget for inlining a leaf function.
pub const INLINE_LEAF_BUDGET: usize = 32;

/// Maximum number of basic blocks allowed in an inlinable leaf function.
/// Short-circuit predicates and small branch-only classification helpers can
/// lower to several blocks even though their instruction cost remains small.
/// The independent instruction budget still prevents code-size growth from
/// complex leaves.
pub const MAX_LEAF_BLOCKS: usize = 12;

/// Evaluates whether `func` is a leaf function eligible for inlining into callers,
/// and returns its estimated cost if eligible.
#[must_use]
pub fn evaluate_leaf_inlining(func: &AmirFunc, budget: usize) -> Option<usize> {
    if func.blocks.is_empty() || func.blocks.len() > MAX_LEAF_BLOCKS {
        return None;
    }

    // Check for loops via DFS cycle detection (three-color marking)
    if has_cycles(func) {
        return None;
    }

    let mut total_cost = 0usize;

    for block in &func.blocks {
        // Suspend terminators (coroutine frontiers) are not inlinable in Phase 1
        if matches!(block.terminator, AmirTerminator::Suspend { .. }) {
            return None;
        }

        // Cost of terminator
        match &block.terminator {
            AmirTerminator::Return => {}
            AmirTerminator::Goto { .. } => total_cost += 1,
            AmirTerminator::Branch { .. } => total_cost += 2,
            AmirTerminator::SwitchInt { targets, .. } => total_cost += 2 + targets.len(),
            AmirTerminator::Unreachable => {}
            AmirTerminator::Suspend { .. } => return None,
        }

        // Inspect statements
        for instr_id in block.statements.iter_ids::<crate::amir::InstrId>() {
            let stmt = func.stmts.get(instr_id)?;
            match stmt {
                // Must be a LEAF: no nested function calls
                AmirStmt::Call { .. } => return None,
                AmirStmt::Assign { .. } => total_cost += 1,
                AmirStmt::Store { .. } => total_cost += 1,
                AmirStmt::Free(_) | AmirStmt::Destroy(_) => total_cost += 1,
                AmirStmt::StorageLive(_) | AmirStmt::StorageDead(_) | AmirStmt::Nop => {}
            }
        }

        if total_cost > budget {
            return None;
        }
    }

    Some(total_cost)
}

/// Detects if `func` contains any cycles (loops) in its control flow graph.
fn has_cycles(func: &AmirFunc) -> bool {
    let n = func.blocks.len();
    if n == 0 {
        return false;
    }

    // 0 = unvisited, 1 = on current DFS stack (gray), 2 = finished (black)
    let mut state = vec![0u8; n];

    fn dfs(u: usize, func: &AmirFunc, state: &mut [u8]) -> bool {
        state[u] = 1;
        let bid = BlockId::from_usize(u);
        for &succ in func.successors(bid) {
            let v = succ.as_usize();
            if v >= state.len() {
                continue;
            }
            if state[v] == 1 {
                return true; // Back-edge detected
            }
            if state[v] == 0 && dfs(v, func, state) {
                return true;
            }
        }
        state[u] = 2;
        false
    }

    for i in 0..n {
        if state[i] == 0 && dfs(i, func, &mut state) {
            return true;
        }
    }

    false
}
