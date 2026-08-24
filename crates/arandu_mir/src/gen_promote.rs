//! F2.3.runtime — promote escaping int-local borrows to GenInsert/GenGet.
//!
//! For [`EscapeKind::HeapStore`] (and not `@NoFallback`):
//! - `t = Borrow(p)` / `BorrowMut(p)` → `t_payload = Load(p); t = GenInsert(t_payload)`
//! - `t2 = *t` when `t` is a gen-ref temp → `t2 = GenGet(t)`
//!
//! Payload MVP: integer-sized locals only (host arena is i64).

use crate::amir::{AmirFunc, AmirOperand, AmirRvalue, AmirStmt, AmirTemp, GenArenaDomain, TempId};
use crate::escape_analysis::{EscapeCheckOptions, EscapeKind, find_escapes};
use crate::ops::UnaryOp;
use crate::types::{ArType, Primitive};
use arandu_lexer::Span;
use rustc_hash::FxHashSet;

/// Rewrite `func` in place when escape analysis finds heap-store candidates.
pub fn apply_gen_promotion(
    func: &mut AmirFunc,
    interner: &crate::types::TypeInterner,
    opts: EscapeCheckOptions,
) {
    if opts.effective_no_fallback() || func.blocks.is_empty() {
        return;
    }

    let events = find_escapes(func, interner);
    let promote_locals: FxHashSet<_> = events
        .into_iter()
        .filter(|e| e.kind == EscapeKind::HeapStore)
        .filter(|e| is_int_local(func, interner, e.place_local))
        .map(|e| e.place_local)
        .collect();

    if promote_locals.is_empty() {
        return;
    }

    let gen_ty = interner.intern(ArType::GenRef);
    let mut gen_temps: FxHashSet<TempId> = FxHashSet::default();
    // Collect rewrites as (block_idx, stmt_index_in_block) operations.
    // We rebuild each block's statement range after expansion.

    for bi in 0..func.blocks.len() {
        let bid = func.blocks[bi].id;
        let old_ids: Vec<_> = func.block_stmt_ids(bid).collect();
        if old_ids.is_empty() {
            continue;
        }

        let mut new_stmts = Vec::new();
        for sid in old_ids {
            let Some(stmt) = func.stmts.get(sid).cloned() else {
                continue;
            };
            match stmt {
                AmirStmt::Assign {
                    lhs,
                    rhs: AmirRvalue::Borrow(place) | AmirRvalue::BorrowMut(place),
                } if place.projections.is_empty() && promote_locals.contains(&place.local) => {
                    let place = place.clone();
                    let local = place.local;
                    let payload_ty = func.locals[local.as_usize()].ty;
                    let span = place_span(func, local);
                    let payload_temp = alloc_temp(func, payload_ty, span);
                    new_stmts.push(AmirStmt::Assign {
                        lhs: payload_temp,
                        rhs: AmirRvalue::Load(place),
                    });
                    // Reuse original lhs as gen-ref holder.
                    if let Some(t) = func.temps.get_mut(lhs.as_usize()) {
                        t.ty = gen_ty;
                        t.is_copy = true;
                    }
                    new_stmts.push(AmirStmt::Assign {
                        lhs,
                        rhs: AmirRvalue::GenInsert {
                            value: AmirOperand::Copy(payload_temp),
                            payload_ty,
                            arena: GenArenaDomain::CompilerManaged,
                            origin: span,
                        },
                    });
                    gen_temps.insert(lhs);
                }
                AmirStmt::Assign {
                    lhs,
                    rhs:
                        AmirRvalue::Unary {
                            op: UnaryOp::Deref,
                            operand: AmirOperand::Copy(src) | AmirOperand::Move(src),
                        },
                } if gen_temps.contains(&src) => {
                    new_stmts.push(AmirStmt::Assign {
                        lhs,
                        rhs: AmirRvalue::GenGet {
                            gen_ref: AmirOperand::Copy(src),
                            payload_ty: func.temps[lhs.as_usize()].ty,
                            arena: GenArenaDomain::CompilerManaged,
                            origin: func.temps[lhs.as_usize()].span,
                        },
                    });
                }
                other => new_stmts.push(other),
            }
        }

        // Replace block statement range with newly pushed stmts.
        let start = func.stmts.len();
        for s in new_stmts {
            func.stmts.push(s);
        }
        let len = func.stmts.len() - start;
        func.blocks[bi].statements = crate::layout::DenseRange::new(start, len);
    }

    // Propagate GenRef type through Use aliases of gen temps.
    let mut retarget: Vec<TempId> = Vec::new();
    for bi in 0..func.blocks.len() {
        let bid = func.blocks[bi].id;
        for sid in func.block_stmt_ids(bid) {
            let Some(AmirStmt::Assign { lhs, rhs }) = func.stmts.get(sid) else {
                continue;
            };
            if let AmirRvalue::Use(AmirOperand::Copy(src) | AmirOperand::Move(src)) = rhs
                && gen_temps.contains(src)
            {
                gen_temps.insert(*lhs);
                retarget.push(*lhs);
            }
        }
    }
    for lhs in retarget {
        if let Some(t) = func.temps.get_mut(lhs.as_usize()) {
            t.ty = gen_ty;
            t.is_copy = true;
        }
    }
}

fn is_int_local(
    func: &AmirFunc,
    interner: &crate::types::TypeInterner,
    local: crate::amir::LocalId,
) -> bool {
    func.locals.get(local.as_usize()).is_some_and(|l| {
        let ty = interner.resolve(l.ty);
        matches!(ty, ArType::IntLiteral)
            || matches!(
                ty,
                ArType::Primitive(p)
                    if p.is_integer() || matches!(p, Primitive::Int | Primitive::Uint)
            )
    })
}

fn place_span(func: &AmirFunc, local: crate::amir::LocalId) -> Span {
    func.locals
        .get(local.as_usize())
        .map(|l| l.span)
        .unwrap_or_else(|| Span::new(0, 0, 0))
}

fn alloc_temp(func: &mut AmirFunc, ty: crate::types::TypeId, span: Span) -> TempId {
    let id = TempId::from_usize(func.temps.len());
    let is_copy = true;
    func.temps.push(AmirTemp {
        id,
        ty,
        is_copy,
        is_nullable: false,
        span,
    });
    id
}
