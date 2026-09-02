use super::LowerCtx;
use crate::SymbolId;
use crate::SymbolTable;
use crate::amir::{AmirOperand, AmirPlace, AmirProjection, AmirRvalue, TempId};
use crate::diagnostics::Diagnostic;
use crate::hir::{HirExprId, HirExprKind};
use crate::ops::UnaryOp;
use crate::passes::type_checker::types::ArType;

impl LowerCtx<'_> {
    /// Keep dummy `Store`s for `local` so projected places see a defined SSA value.
    ///
    /// `prune_dummy_loads_stores` drops plain stores to non-`is_memory` locals (Ref/Ptr
    /// are scalar). A place with `Deref`/`Field` is addressed via that local's value in
    /// codegen — without a surviving store, `use_var` is undef (SIGSEGV). Setting
    /// `is_memory` is the same flag F2.0 already uses for address-taken homes; for
    /// pointer-sized types it does **not** force a stack slot (`needs_scalar_stack_home`
    /// is only true for primitive scalars).
    pub(crate) fn mark_local_materialized(&mut self, local: crate::amir::LocalId) {
        let idx = local.as_usize();
        if idx < self.locals.len() {
            self.locals[idx].is_memory = true;
        }
    }

    /// Lower an expression to an [`AmirPlace`] for Borrow/Load (F2.0 + BC.4a).
    ///
    /// Gold path: locals, `*p` through ptr/ref, and field chains.
    pub(crate) fn lower_expr_to_place(
        &mut self,
        expr_id: HirExprId,
        symbols: &SymbolTable,
    ) -> Result<AmirPlace, Diagnostic> {
        let expr = self.hir.pool.expr(expr_id);
        match &expr.kind {
            HirExprKind::Path { symbol } => {
                if let Some(&local_id) = self.symbol_map.get(symbol) {
                    return Ok(AmirPlace {
                        local: local_id,
                        projections: smallvec::smallvec![],
                    });
                }
                Err(self.move_diag(format!(
                    "cannot take address of non-local `{}`",
                    symbols.get(*symbol).name
                )))
            }
            // BC.4a: `*p` where `p` is a local holding a pointer — place through that pointer.
            HirExprKind::Unary {
                op: UnaryOp::Deref,
                expr: inner,
            } => {
                let mut place = self.lower_expr_to_place(*inner, symbols)?;
                let base_ty = self.resolve_ty(self.hir.pool.expr(*inner).ty);
                if !matches!(
                    base_ty,
                    ArType::Ptr(_) | ArType::Ref(_) | ArType::RefMut(_) | ArType::Nullable(_)
                ) {
                    return Err(self.move_diag(
                        "BC.4a: can only form a place through `ptr[T]`, `&T`, or `&mut T`",
                    ));
                }
                place.projections.push(AmirProjection::Deref);
                // Projected places address through the local's *value*. Dummy Stores to
                // non-memory Ref/Ptr locals are pruned — keep this base materialised so
                // Cranelift `use_var` sees a defined pointer (BC.4a).
                self.mark_local_materialized(place.local);
                Ok(place)
            }
            HirExprKind::Field { base, field } | HirExprKind::SafeField { base, field } => {
                let mut place = self.lower_expr_to_place(*base, symbols)?;
                let base_ty = self.resolve_ty(self.hir.pool.expr(*base).ty);
                let field_sym = self
                    .resolve_field_symbol(&base_ty, field.as_str())
                    .ok_or_else(|| {
                        self.move_diag(format!(
                            "cannot borrow field `{}`: symbol not resolved",
                            field
                        ))
                    })?;
                place.projections.push(AmirProjection::Field(field_sym));
                self.mark_local_materialized(place.local);
                Ok(place)
            }
            HirExprKind::Index { base, index } => {
                let mut place = self.lower_expr_to_place(*base, symbols)?;
                let base_ty = self.resolve_ty(self.hir.pool.expr(*base).ty);
                let is_vec = match &base_ty {
                    ArType::Named(sym_id, args) => {
                        (symbols.get(*sym_id).name == "Vec"
                            || symbols.get(*sym_id).name.ends_with(".Vec"))
                            && args.len() == 1
                    }
                    _ => false,
                };
                if !matches!(base_ty, ArType::Array(_, _) | ArType::Slice(_)) && !is_vec {
                    return Err(self.move_diag(
                        "can only borrow an indexed place from an array, slice, or vector",
                    ));
                }
                let index = self.lower_expr(*index, None, symbols)?;
                place.projections.push(AmirProjection::Index(index));
                self.mark_local_materialized(place.local);
                Ok(place)
            }
            _ => Err(self.move_diag(
                "can only borrow locals, indexed array/slice elements, `*p` through a pointer, and field paths",
            )),
        }
    }

    /// Struct symbol for a base type (unwraps Ptr/Ref/RefMut/Nullable).
    fn struct_id_of_base(&self, base_ty: &ArType) -> Option<SymbolId> {
        let interner = &self.tc.type_info.type_interner;
        match base_ty {
            ArType::Named(id, _) => Some(*id),
            ArType::Nullable(inner)
            | ArType::Ptr(inner)
            | ArType::Ref(inner)
            | ArType::RefMut(inner) => {
                let inner_ty = interner.resolve(*inner);
                match inner_ty {
                    ArType::Named(id, _) => Some(id),
                    ArType::Ptr(ptr_inner) | ArType::Ref(ptr_inner) | ArType::RefMut(ptr_inner) => {
                        match interner.resolve(ptr_inner) {
                            ArType::Named(id, _) => Some(id),
                            _ => None,
                        }
                    }
                    _ => None,
                }
            }
            _ => None,
        }
    }

    pub(crate) fn resolve_field_symbol(&self, base_ty: &ArType, field: &str) -> Option<SymbolId> {
        let sid = self.struct_id_of_base(base_ty)?;
        self.tc
            .type_info
            .struct_field_symbols
            .get(&sid)
            .and_then(|m| m.get(field).copied())
    }

    pub(crate) fn resolve_field_index(&self, base_ty: &ArType, field: &str) -> usize {
        if let Ok(idx) = field.parse::<usize>() {
            return idx;
        }
        if field.starts_with('_')
            && let Ok(idx) = field[1..].parse::<usize>()
        {
            return idx;
        }
        self.struct_id_of_base(base_ty)
            .and_then(|sid| self.tc.type_info.struct_field_indices.get(&sid))
            .and_then(|m| m.get(field).copied())
            .unwrap_or(0)
    }

    pub(crate) fn lower_field(
        &mut self,
        base: HirExprId,
        field: &str,
        expr_ty: crate::types::TypeId,
        target: Option<TempId>,
        symbols: &SymbolTable,
    ) -> Result<AmirOperand, Diagnostic> {
        let base_op = self.lower_expr(base, None, symbols)?;
        let dest = target.unwrap_or_else(|| self.new_temp_id(expr_ty));
        let base_expr = self.hir.pool.expr(base);
        let base_ty = self.resolve_ty(base_expr.ty);
        let field_idx = self.resolve_field_index(&base_ty, field);
        self.emit_assign_temp(
            dest,
            AmirRvalue::FieldAccess {
                base: base_op,
                field: field_idx,
            },
        );
        Ok(AmirOperand::Copy(dest))
    }

    pub(crate) fn lower_index(
        &mut self,
        base: HirExprId,
        index: HirExprId,
        expr_ty: crate::types::TypeId,
        target: Option<TempId>,
        symbols: &SymbolTable,
    ) -> Result<AmirOperand, Diagnostic> {
        let base_op = self.lower_expr(base, None, symbols)?;
        let idx_op = self.lower_expr(index, None, symbols)?;
        let dest = target.unwrap_or_else(|| self.new_temp_id(expr_ty));
        self.emit_assign_temp(
            dest,
            AmirRvalue::IndexAccess {
                base: base_op,
                index: idx_op,
            },
        );
        Ok(AmirOperand::Copy(dest))
    }
}
