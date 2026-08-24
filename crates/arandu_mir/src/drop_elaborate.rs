//! Elaborate semantic cleanup obligations into explicit AMIR `Destroy`.
//!
//! Arandu currently rejects inconsistent moves between CFG branches, so a
//! valid return boundary never needs a runtime drop flag: each initialized
//! local is either available on every path or moved on every path. This pass
//! reuses the move and definite-init facts that enforce that rule.

use crate::amir::{AmirFunc, AmirPlace, AmirStmt, AmirStmtTable, AmirTerminator, LocalId};
use crate::move_checker::{DropState, move_states_at_block_exit};
use arandu_middle::layout::DenseRange;
use arandu_middle::types::ArType;
use arandu_typeck::TypeInfo;
use smallvec::SmallVec;

/// Insert exactly-once root-local destruction before normal function returns.
pub fn elaborate_drops(func: &mut AmirFunc, type_info: &TypeInfo) {
    if type_info
        .destructor_instances
        .values()
        .any(|symbol| *symbol == func.symbol)
    {
        // `own self` is consumed by the destructor body itself. Re-entering the
        // same destructor from its epilogue would recurse indefinitely.
        return;
    }

    let initialized = crate::definite_init::initialized_at_block_exit(func);
    let moved = move_states_at_block_exit(func);
    let old = std::mem::replace(&mut func.stmts, AmirStmtTable::new());
    let mut rebuilt = AmirStmtTable::new();
    let mut ranges = Vec::with_capacity(func.blocks.len());

    for block in &func.blocks {
        let start = rebuilt.len();
        for id in block.statements.iter_ids::<crate::amir::InstrId>() {
            rebuilt.push(old.payloads[id].clone());
        }
        if matches!(block.terminator, AmirTerminator::Return) {
            for local in func.locals.iter().rev() {
                let destructible =
                    matches!(type_info.resolve_type_id(local.ty), ArType::Named(_, _))
                        && type_info.destructor_instances.contains_key(&local.ty);
                if destructible
                    && initialized[block.id.as_usize()].contains(local.id)
                    && moved[block.id.as_usize()][local.id.as_usize()] == DropState::Available
                {
                    rebuilt.push(AmirStmt::Destroy(AmirPlace {
                        local: LocalId::from_usize(local.id.as_usize()),
                        projections: SmallVec::new(),
                    }));
                }
            }
        }
        ranges.push(DenseRange::new(start, rebuilt.len() - start));
    }

    func.stmts = rebuilt;
    for (block, range) in func.blocks.iter_mut().zip(ranges) {
        block.statements = range;
    }
}
