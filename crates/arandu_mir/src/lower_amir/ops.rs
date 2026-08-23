use super::LowerCtx;
use crate::SymbolTable;
use crate::amir::{AmirOperand, AmirRvalue, TempId};
use crate::diagnostics::Diagnostic;
use crate::hir::HirExprId;
use crate::ops::{BinaryOp, UnaryOp};
use crate::passes::type_checker::types::ArType;

impl LowerCtx<'_> {
    pub(crate) fn lower_binary(
        &mut self,
        op: BinaryOp,
        left: HirExprId,
        right: HirExprId,
        expr_ty: crate::types::TypeId,
        target: Option<TempId>,
        symbols: &SymbolTable,
    ) -> Result<AmirOperand, Diagnostic> {
        let l_op = self.lower_expr(left, None, symbols)?;
        let r_op = self.lower_expr(right, None, symbols)?;
        let dest = target.unwrap_or_else(|| self.new_temp_id(expr_ty));
        self.emit_assign_temp(
            dest,
            AmirRvalue::Binary {
                op,
                left: l_op,
                right: r_op,
            },
        );
        Ok(AmirOperand::Copy(dest))
    }

    pub(crate) fn lower_unary(
        &mut self,
        op: UnaryOp,
        sub_expr: HirExprId,
        expr_ty: crate::types::TypeId,
        target: Option<TempId>,
        symbols: &SymbolTable,
    ) -> Result<AmirOperand, Diagnostic> {
        // F2.0: `&`/`&mut` lower to place borrows; `*` on a ref loads through the pointer.
        match op {
            UnaryOp::Ref | UnaryOp::RefMut => {
                let place = self.lower_expr_to_place(sub_expr, symbols)?;
                // F2.0: address-taken *stack* scalars need a stack home (`is_memory`).
                // BC.4a: a place that goes through `Deref` already has a materialised
                // pointer in the local's SSA value — do NOT force a stack slot for it
                // (stack_addr of the pointer cell ≠ the pointer itself).
                let through_ptr = place
                    .projections
                    .iter()
                    .any(|p| matches!(p, crate::amir::AmirProjection::Deref));
                if place.projections.is_empty() && !through_ptr {
                    let idx = place.local.as_usize();
                    if idx < self.locals.len() {
                        self.locals[idx].is_memory = true;
                    }
                }
                let dest = target.unwrap_or_else(|| self.new_temp_id(expr_ty));
                let rv = if matches!(op, UnaryOp::RefMut) {
                    AmirRvalue::BorrowMut(place)
                } else {
                    AmirRvalue::Borrow(place)
                };
                self.emit_assign_temp(dest, rv);
                Ok(AmirOperand::Copy(dest))
            }
            UnaryOp::Deref => {
                // `*p` where p is `&T` / `&mut T` / local holding a ref: load pointee.
                // If sub is a place of the referent (`*&x` after fold would be x), use Load.
                // Otherwise treat the operand as a pointer value and load through it via
                // FieldAccess-free Load of a temporary place when possible.
                if let Ok(place) = self.lower_expr_to_place(sub_expr, symbols) {
                    // Local of type Ref/RefMut still needs one indirection — Load the place
                    // yields the reference bits; for stack locals of Ref, that *is* the
                    // pointer. Backend maps Load of Ref-typed local as "use pointer value"
                    // and for Borrow result we already have a pointer temp.
                    //
                    // Gold path: `*p` with p: &T → emit Load after reinterpreting.
                    // Use Unary Deref for pointer-valued operands so backends can load.
                    let sub_op = self.read_variable_source(place.local)?;
                    let dest = target.unwrap_or_else(|| self.new_temp_id(expr_ty));
                    if place.projections.is_empty() {
                        self.emit_assign_temp(
                            dest,
                            AmirRvalue::Unary {
                                op: UnaryOp::Deref,
                                operand: sub_op,
                            },
                        );
                    } else {
                        self.emit_assign_temp(dest, AmirRvalue::Load(place));
                    }
                    Ok(AmirOperand::Copy(dest))
                } else {
                    let sub_op = self.lower_expr(sub_expr, None, symbols)?;
                    let dest = target.unwrap_or_else(|| self.new_temp_id(expr_ty));
                    self.emit_assign_temp(
                        dest,
                        AmirRvalue::Unary {
                            op: UnaryOp::Deref,
                            operand: sub_op,
                        },
                    );
                    Ok(AmirOperand::Copy(dest))
                }
            }
            // A3.1: inside `async func` or `async { … }`, `await` is a CFG suspension
            // frontier. Sync drive-to-completion (await in non-coroutine context) stays
            // a plain unary await without Suspend.
            UnaryOp::Await if self.func_is_async || self.coroutine_depth > 0 => {
                let future_op = self.lower_expr(sub_expr, None, symbols)?;
                let resume = self.new_block();
                self.emit_suspend(future_op, resume)?;
                // Continue after the frontier; future still dominates resume (temps).
                self.builder.current_block = Some(resume);
                let dest = target.unwrap_or_else(|| self.new_temp_id(expr_ty));
                self.emit_assign_temp(
                    dest,
                    AmirRvalue::Unary {
                        op: UnaryOp::Await,
                        operand: future_op,
                    },
                );
                Ok(AmirOperand::Copy(dest))
            }
            _ => {
                let sub_op = self.lower_expr(sub_expr, None, symbols)?;
                let dest = target.unwrap_or_else(|| self.new_temp_id(expr_ty));
                self.emit_assign_temp(
                    dest,
                    AmirRvalue::Unary {
                        op,
                        operand: sub_op,
                    },
                );
                Ok(AmirOperand::Copy(dest))
            }
        }
    }

    /// Lower a call argument with consume mode + W3 auto-ref materialization.
    ///
    /// Auto-ref only when the **argument value** is not already a ref and the
    /// formal is `&T`/`&mut T`. Re-borrowing an existing ref local would create
    /// a loan of the *pointer cell* (not the pointee) and breaks O003 on
    /// overlapping pointee loans (see cli_smoke O003 fixture).
    pub(crate) fn lower_call_arg(
        &mut self,
        arg: HirExprId,
        formal_index: usize,
        callee: crate::SymbolId,
        formal_ty: Option<&ArType>,
        symbols: &SymbolTable,
    ) -> Result<AmirOperand, Diagnostic> {
        let mode = self.arg_modes.kind(callee, formal_index);
        let arg_expr = self.hir.pool.expr(arg);
        let arg_ty = self.resolve_ty(arg_expr.ty);
        let formal_is_ref =
            formal_ty.is_some_and(|t| matches!(t, ArType::Ref(_) | ArType::RefMut(_)));
        let arg_is_ref = matches!(arg_ty, ArType::Ref(_) | ArType::RefMut(_));
        let exclusive = matches!(formal_ty, Some(ArType::RefMut(_))) || mode.is_exclusive();

        // W3.3 auto-ref: formal is ref, value is not — materialize Borrow of place.
        if formal_is_ref
            && !arg_is_ref
            && let Ok(place) = self.lower_expr_to_place(arg, symbols)
        {
            if place.projections.is_empty() {
                let idx = place.local.as_usize();
                if idx < self.locals.len() {
                    self.locals[idx].is_memory = true;
                }
            }
            let formal_tid = match formal_ty {
                Some(t) => self.intern_ty_ref(t),
                None => arg_expr.ty,
            };
            let dest = self.new_temp_id(formal_tid);
            let rv = if exclusive {
                AmirRvalue::BorrowMut(place)
            } else {
                AmirRvalue::Borrow(place)
            };
            self.emit_assign_temp(dest, rv);
            return Ok(AmirOperand::Copy(dest));
        }

        let op = self.lower_expr(arg, None, symbols)?;
        if mode.is_borrow() || arg_is_ref {
            // shared/mut self or already a reference: do not move.
            Ok(op)
        } else {
            self.consume_operand(op)
        }
    }
}
