//! Global Value Numbering (GVN) pass for AMIR.
//!
//! Replaces redundant computations across the dominator tree with previously
//! computed values. Supports canonicalized binary operations, pure unary
//! operations, and immutable struct field accesses.

use crate::amir::{
    AmirConstant, AmirFunc, AmirOperand, AmirRvalue, AmirStmt, BlockId, Dominators, InstrId, TempId,
};
use crate::ops::{BinaryOp, UnaryOp};
use rustc_hash::FxHashMap;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum ValueExpr {
    Constant(AmirConstant),
    Unary(UnaryOp, TempId),
    Binary(BinaryOp, TempId, TempId),
    FieldAccess(TempId, usize),
    Use(TempId),
}

/// Applies Global Value Numbering to `func`.
///
/// Returns `true` if any instruction was rewritten.
pub fn gvn(func: &mut AmirFunc) -> bool {
    let n_blocks = func.blocks.len();
    if n_blocks == 0 || func.stmts.is_empty() {
        return false;
    }

    let doms = Dominators::new(func);
    let mut changed = false;

    // Maps TempId -> defining block and canonical temporary leader
    let mut temp_leader: Vec<TempId> = (0..func.temps.len()).map(TempId::from_usize).collect();
    let mut temp_def_block: Vec<Option<BlockId>> = vec![None; func.temps.len()];

    // Record block parameters definition blocks
    for (bi, block) in func.blocks.iter().enumerate() {
        let bid = BlockId::from_usize(bi);
        for param in &block.params {
            temp_def_block[param.id.as_usize()] = Some(bid);
        }
    }

    // Record statement definition blocks
    for bi in 0..n_blocks {
        let bid = BlockId::from_usize(bi);
        let stmt_ids: Vec<InstrId> = func.block_stmt_ids(bid).collect();
        for stmt_id in stmt_ids {
            if let AmirStmt::Assign { lhs, .. } = func.stmt(stmt_id)
                && lhs.as_usize() < temp_def_block.len()
            {
                temp_def_block[lhs.as_usize()] = Some(bid);
            }
        }
    }

    // Canonical expression table: ValueExpr -> (Leader TempId, Defining Block)
    let mut expr_table: FxHashMap<ValueExpr, Vec<(TempId, BlockId)>> = FxHashMap::default();
    let mut replacements: Vec<(InstrId, AmirStmt, TempId, TempId)> = Vec::new();

    for bi in 0..n_blocks {
        let bid = BlockId::from_usize(bi);
        let stmt_ids: Vec<InstrId> = func.block_stmt_ids(bid).collect();
        for stmt_id in stmt_ids {
            let stmt = func.stmt(stmt_id).clone();
            if let AmirStmt::Assign { lhs, rhs } = stmt {
                let expr = match to_value_expr(&rhs, &temp_leader) {
                    Some(e) => e,
                    None => continue,
                };

                // Check if this expression was already computed in a dominating block
                let mut found_leader = None;
                if let Some(candidates) = expr_table.get(&expr) {
                    for &(leader, def_block) in candidates {
                        if doms.dominates(def_block, bid) {
                            found_leader = Some(leader);
                            break;
                        }
                    }
                }

                if let Some(leader) = found_leader {
                    if leader != lhs {
                        replacements.push((
                            stmt_id,
                            AmirStmt::Assign {
                                lhs,
                                rhs: AmirRvalue::Use(AmirOperand::Copy(leader)),
                            },
                            lhs,
                            leader,
                        ));
                    }
                } else {
                    // Register this expression as the canonical leader in this block
                    expr_table.entry(expr).or_default().push((lhs, bid));
                }
            }
        }
    }

    if !replacements.is_empty() {
        for (stmt_id, new_stmt, lhs, leader) in replacements {
            if let Some(target) = func.stmts.get_mut(stmt_id) {
                *target = new_stmt;
            }
            temp_leader[lhs.as_usize()] = leader;
        }
        changed = true;
    }

    changed
}

fn canonicalize_temp(op: &AmirOperand, temp_leader: &[TempId]) -> Option<TempId> {
    match op {
        AmirOperand::Copy(t) | AmirOperand::Move(t) => {
            let idx = t.as_usize();
            if idx < temp_leader.len() {
                Some(temp_leader[idx])
            } else {
                Some(*t)
            }
        }
        _ => None,
    }
}

fn to_value_expr(rvalue: &AmirRvalue, temp_leader: &[TempId]) -> Option<ValueExpr> {
    match rvalue {
        AmirRvalue::Use(AmirOperand::Constant(c)) => Some(ValueExpr::Constant(*c)),
        AmirRvalue::Use(op) => {
            let t = canonicalize_temp(op, temp_leader)?;
            Some(ValueExpr::Use(t))
        }
        AmirRvalue::Unary { op, operand } => {
            // Only pure unary operators (deref and await are impure)
            if matches!(op, UnaryOp::Neg | UnaryOp::Not) {
                let t = canonicalize_temp(operand, temp_leader)?;
                Some(ValueExpr::Unary(*op, t))
            } else {
                None
            }
        }
        AmirRvalue::Binary { op, left, right } => {
            // NullCoalesce, Range etc are treated specially; arithmetic/logic are pure
            if matches!(
                op,
                BinaryOp::Add
                    | BinaryOp::Sub
                    | BinaryOp::Mul
                    | BinaryOp::Div
                    | BinaryOp::Mod
                    | BinaryOp::Equal
                    | BinaryOp::NotEqual
                    | BinaryOp::Lt
                    | BinaryOp::LtEqual
                    | BinaryOp::Gt
                    | BinaryOp::GtEqual
                    | BinaryOp::BitAnd
                    | BinaryOp::BitOr
                    | BinaryOp::BitXor
                    | BinaryOp::ShiftLeft
                    | BinaryOp::ShiftRight
            ) {
                let mut t_left = canonicalize_temp(left, temp_leader)?;
                let mut t_right = canonicalize_temp(right, temp_leader)?;

                // Canonicalize commutative operators so (a + b) == (b + a)
                if matches!(
                    op,
                    BinaryOp::Add
                        | BinaryOp::Mul
                        | BinaryOp::Equal
                        | BinaryOp::NotEqual
                        | BinaryOp::BitAnd
                        | BinaryOp::BitOr
                        | BinaryOp::BitXor
                ) && t_left.as_usize() > t_right.as_usize()
                {
                    std::mem::swap(&mut t_left, &mut t_right);
                }

                Some(ValueExpr::Binary(*op, t_left, t_right))
            } else {
                None
            }
        }
        AmirRvalue::FieldAccess { base, field } => {
            let t = canonicalize_temp(base, temp_leader)?;
            Some(ValueExpr::FieldAccess(t, *field))
        }
        _ => None,
    }
}
