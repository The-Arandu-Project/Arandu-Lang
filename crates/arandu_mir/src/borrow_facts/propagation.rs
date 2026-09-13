//! CFG propagation of holder states and reference live range tracking.

use super::paths::{
    LocalHolderState, merge_local_paths, merge_temp_paths, place_path, prefix_paths,
};
use super::types::{BorrowState, Loan};
use crate::BitSet;
use crate::amir::{AmirFunc, AmirOperand, AmirStmt, AmirTerminator, BlockId, LocalId, TempId};
use std::collections::{BTreeSet, VecDeque};

/// Forward may-analysis for stack carriers. Unlike the global holder index,
/// this state models overwrites: assigning one projection kills only that
/// subtree, while joins union the paths arriving from each predecessor.
pub(crate) fn analyze_local_holder_states(
    func: &AmirFunc,
    loans: &[Loan],
) -> Vec<Vec<Vec<BitSet<LocalId>>>> {
    let empty_state = || vec![vec![BTreeSet::new(); func.locals.len()]; loans.len()];
    let mut block_in = vec![empty_state(); func.blocks.len()];
    let mut block_out = vec![empty_state(); func.blocks.len()];
    let mut queue = (0..func.blocks.len())
        .map(BlockId::from_usize)
        .collect::<VecDeque<_>>();
    let mut queued = vec![true; func.blocks.len()];

    while let Some(block_id) = queue.pop_front() {
        let bi = block_id.as_usize();
        queued[bi] = false;
        let mut state = block_in[bi].clone();
        for stmt in func.block_stmts(block_id) {
            transfer_local_holders(stmt, loans, &mut state);
        }
        if state == block_out[bi] {
            continue;
        }
        block_out[bi] = state.clone();
        let block = &func.blocks[bi];
        for (target, args) in terminator_edges(&block.terminator) {
            let mut edge = state.clone();
            if let Some(successor) = func.blocks.get(target.as_usize()) {
                let succ_params = func.block_params(successor.params);
                for (parameter, argument) in succ_params.iter().zip(args) {
                    let source = operand_temp(argument);
                    for (loan_index, loan) in loans.iter().enumerate() {
                        let paths = source
                            .and_then(|temp| loan.holder_temp_paths.get(temp.as_usize()))
                            .cloned()
                            .unwrap_or_default();
                        edge[loan_index][parameter.local.as_usize()] = paths;
                    }
                }
            }
            if merge_local_state(&mut block_in[target.as_usize()], &edge)
                && !queued[target.as_usize()]
            {
                queue.push_back(target);
                queued[target.as_usize()] = true;
            }
        }
    }

    let mut points = Vec::with_capacity(func.blocks.len());
    for block in &func.blocks {
        let mut state = block_in[block.id.as_usize()].clone();
        let mut block_points = Vec::new();
        block_points.push(holder_bits(&state, func.locals.len()));
        for stmt in func.block_stmts(block.id) {
            transfer_local_holders(stmt, loans, &mut state);
            block_points.push(holder_bits(&state, func.locals.len()));
        }
        points.push(block_points);
    }
    points
}

fn transfer_local_holders(stmt: &AmirStmt, loans: &[Loan], state: &mut LocalHolderState) {
    match stmt {
        AmirStmt::Store { lhs, rhs } => {
            let destination = place_path(lhs);
            let source = operand_temp(rhs);
            for (loan_index, loan) in loans.iter().enumerate() {
                let local = &mut state[loan_index][lhs.local.as_usize()];
                local.retain(|path| !path.0.starts_with(&destination.0));
                if let Some(source) = source {
                    let paths = loan
                        .holder_temp_paths
                        .get(source.as_usize())
                        .cloned()
                        .unwrap_or_default();
                    local.extend(prefix_paths(&paths, &destination));
                }
            }
        }
        AmirStmt::StorageDead(local) => {
            for loan_state in state.iter_mut() {
                loan_state[local.as_usize()].clear();
            }
        }
        AmirStmt::Assign { .. }
        | AmirStmt::Call { .. }
        | AmirStmt::Free(_)
        | AmirStmt::StorageLive(_)
        | AmirStmt::Destroy(_)
        | AmirStmt::Nop => {}
    }
}

fn merge_local_state(target: &mut LocalHolderState, source: &LocalHolderState) -> bool {
    let mut changed = false;
    for (target_loan, source_loan) in target.iter_mut().zip(source) {
        for (target_local, source_local) in target_loan.iter_mut().zip(source_loan) {
            let old_len = target_local.len();
            target_local.extend(source_local.iter().cloned());
            changed |= target_local.len() != old_len;
        }
    }
    changed
}

fn holder_bits(state: &LocalHolderState, num_locals: usize) -> Vec<BitSet<LocalId>> {
    state
        .iter()
        .map(|loan| {
            let mut bits = BitSet::with_capacity(num_locals);
            for (index, paths) in loan.iter().enumerate() {
                if !paths.is_empty() {
                    bits.insert(LocalId::from_usize(index));
                }
            }
            bits
        })
        .collect()
}

fn terminator_edges(terminator: &AmirTerminator) -> Vec<(BlockId, &[AmirOperand])> {
    match terminator {
        AmirTerminator::Goto { target, args } => vec![(*target, args)],
        AmirTerminator::Suspend { resume, args, .. } => vec![(*resume, args)],
        AmirTerminator::Branch {
            if_true,
            true_args,
            if_false,
            false_args,
            ..
        } => vec![(*if_true, true_args), (*if_false, false_args)],
        AmirTerminator::SwitchInt {
            targets, otherwise, ..
        } => {
            let mut edges = targets
                .iter()
                .map(|(_, target, args)| (*target, args.as_slice()))
                .collect::<Vec<_>>();
            edges.push((otherwise.0, otherwise.1.as_slice()));
            edges
        }
        AmirTerminator::Return | AmirTerminator::Unreachable => Vec::new(),
    }
}

pub(crate) fn propagate_terminator_args(
    func: &AmirFunc,
    target: BlockId,
    args: &[AmirOperand],
    loans: &mut [Loan],
    changed: &mut bool,
) {
    let Some(tb) = func.blocks.get(target.as_usize()) else {
        return;
    };
    for (i, arg) in args.iter().enumerate() {
        let Some(src) = operand_temp(arg) else {
            continue;
        };
        let Some(param) = func.block_params(tb.params).get(i) else {
            continue;
        };
        for loan in loans.iter_mut() {
            let paths = loan
                .holder_temp_paths
                .get(src.as_usize())
                .cloned()
                .unwrap_or_default();
            *changed |= merge_temp_paths(loan, param.id, paths.clone());
            // Block params often alias a local.
            *changed |= merge_local_paths(loan, param.local, paths);
        }
    }
}

pub(crate) fn operand_temp(op: &AmirOperand) -> Option<TempId> {
    match op {
        AmirOperand::Copy(t) | AmirOperand::Move(t) => Some(*t),
        _ => None,
    }
}

pub(crate) fn state_from_live_holders(
    loans: &[Loan],
    num_locals: usize,
    temp_live: &BitSet<TempId>,
    local_live: &BitSet<LocalId>,
) -> BorrowState {
    let mut st = BorrowState::new(num_locals);
    for loan in loans {
        let temp_active = loan.holder_temps.iter().any(|t| temp_live.contains(t));
        let local_active = loan.holder_locals.iter().any(|l| local_live.contains(l));
        if temp_active || local_active {
            st.activate(loan);
        }
    }
    st
}
