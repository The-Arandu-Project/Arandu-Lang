use super::LowerCtx;
use crate::SymbolTable;
use crate::amir::{AmirConstant, AmirOperand, AmirRvalue, TempId};
use crate::diagnostics::Diagnostic;
use crate::hir::{HirExprId, HirExprKind};
use crate::ops::{BinaryOp, UnaryOp};
use crate::passes::type_checker::types::{ArType, Primitive};

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
        if matches!(op, BinaryOp::And | BinaryOp::Or) {
            let dest = target.unwrap_or_else(|| self.new_temp_id(expr_ty));
            if self.builder.current_block.is_none() {
                return Ok(AmirOperand::Copy(dest));
            }

            let bb_short = self.new_block();
            let bb_right = self.new_block();
            let bb_join = self.new_block();
            if matches!(op, BinaryOp::And) {
                self.set_bool_branch(l_op, bb_right, bb_short);
            } else {
                self.set_bool_branch(l_op, bb_short, bb_right);
            }
            self.seal_block(bb_short);
            self.seal_block(bb_right);

            self.builder.current_block = Some(bb_short);
            let result_local = self.new_compiler_local(ArType::Primitive(Primitive::Bool));
            self.write_variable(
                bb_short,
                result_local,
                AmirOperand::Constant(AmirConstant::Bool(matches!(op, BinaryOp::Or))),
            );
            self.emit_goto(bb_join);

            self.builder.current_block = Some(bb_right);
            let right_op = self.lower_expr(right, None, symbols)?;
            if self.builder.current_block.is_some() {
                let right_block = self.require_block()?;
                self.write_variable(right_block, result_local, right_op);
                self.emit_goto(bb_join);
            }

            self.seal_block(bb_join);
            self.builder.current_block = Some(bb_join);
            let result = self.read_variable(bb_join, result_local);
            self.emit_assign_temp(dest, AmirRvalue::Use(result));
            return Ok(AmirOperand::Copy(dest));
        }
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

        // Preserve the source place for named-field move arguments. Ordinary field
        // value lowering produces `FieldAccess`, which intentionally carries only the
        // ordinal needed by codegen and loses the ownership path. A direct
        // `Load(place)` keeps stable field symbols without rebuilding them later.
        let projected_place = matches!(&arg_expr.kind, HirExprKind::Field { .. });
        let op = if !mode.is_borrow() && !arg_is_ref && projected_place {
            match self.lower_expr_to_place(arg, symbols) {
                Ok(place) => {
                    let root_has_destructor =
                        self.locals
                            .get(place.local.as_usize())
                            .is_some_and(|local| {
                                self.tc
                                    .type_info
                                    .destructor_instances
                                    .contains_key(&local.ty)
                            });
                    if !place.projections.is_empty() && root_has_destructor {
                        return Err(Diagnostic::error(
                            crate::DiagCode::U001FeatureNotSupported,
                            "cannot move a field out of a value with an explicit destructor",
                            arg_expr.span,
                        )
                        .with_note(
                            "the destructor requires the complete value; move the whole value or borrow the field",
                        ));
                    }
                    self.load_place(&place, arg_expr.ty)?
                }
                // A projection on a temporary is a value expression rather than a
                // place rooted in a local. Keep ordinary lowering for that case.
                Err(_) => self.lower_expr(arg, None, symbols)?,
            }
        } else {
            self.lower_expr(arg, None, symbols)?
        };
        if mode.is_borrow() || arg_is_ref {
            // shared/mut self or already a reference: do not move.
            Ok(op)
        } else {
            self.consume_operand(op)
        }
    }
}
