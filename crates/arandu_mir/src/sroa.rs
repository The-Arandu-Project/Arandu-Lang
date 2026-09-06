//! Scalar Replacement of Aggregates (SROA) for AMIR.
//!
//! Propagates values from StructLiteral/Tuple directly into FieldAccess reads
//! across basic blocks where the aggregate value is known and un-mutated.

use crate::amir::{AmirFunc, AmirOperand, AmirRvalue, AmirStmt, BlockId, InstrId, TempId};
use rustc_hash::FxHashMap;

/// Applies SROA to `func`.
///
/// Returns `true` if any field access was resolved to a scalar operand.
pub fn sroa(func: &mut AmirFunc) -> bool {
    let n_blocks = func.blocks.len();
    if n_blocks == 0 || func.stmts.is_empty() {
        return false;
    }

    let mut changed = false;

    // Track aggregates assigned to temporaries: TempId -> field values
    let mut struct_temps: FxHashMap<TempId, Vec<AmirOperand>> = FxHashMap::default();
    let mut rewrites: Vec<(InstrId, AmirStmt)> = Vec::new();

    for bi in 0..n_blocks {
        let bid = BlockId::from_usize(bi);
        let stmt_ids: Vec<InstrId> = func.block_stmt_ids(bid).collect();
        for stmt_id in stmt_ids {
            let stmt = func.stmt(stmt_id).clone();
            if let AmirStmt::Assign { lhs, rhs } = stmt {
                match rhs {
                    AmirRvalue::StructLiteral { fields, .. } => {
                        let field_ops: Vec<AmirOperand> =
                            fields.into_iter().map(|(_, op)| op).collect();
                        struct_temps.insert(lhs, field_ops);
                    }
                    AmirRvalue::Tuple { items } => {
                        struct_temps.insert(lhs, items);
                    }
                    AmirRvalue::FieldAccess { base, field } => {
                        let base_temp = match base {
                            AmirOperand::Copy(t) | AmirOperand::Move(t) => Some(t),
                            _ => None,
                        };
                        if let Some(bt) = base_temp
                            && let Some(field_ops) = struct_temps.get(&bt)
                            && let Some(resolved_op) = field_ops.get(field)
                        {
                            rewrites.push((
                                stmt_id,
                                AmirStmt::Assign {
                                    lhs,
                                    rhs: AmirRvalue::Use(*resolved_op),
                                },
                            ));
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    if !rewrites.is_empty() {
        for (stmt_id, new_stmt) in rewrites {
            if let Some(target) = func.stmts.get_mut(stmt_id) {
                *target = new_stmt;
            }
        }
        changed = true;
    }

    changed
}
