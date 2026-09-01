use arandu_semantics::amir::{AmirPlace, AmirProjection};
use arandu_semantics::passes::type_checker::types::ArType;
use cranelift_codegen::ir::{InstBuilder, Value};

use super::FunctionTranslator;

impl<M: cranelift_module::Module> FunctionTranslator<'_, '_, M> {
    /// Address of `place` as `(base_ptr, constant_offset)`.
    ///
    /// - **F2.0** stack homes: address of the stack slot.
    /// - **BC.4a** `Deref` projections: base is the pointer **value** already in the local
    ///   (heap/`ptr` materialised — identity GEP root).
    /// - **Named/heap objects**: SSA local holds the object pointer.
    pub(super) fn translate_place_address_for_load(&mut self, place: &AmirPlace) -> (Value, i32) {
        let through_ptr = place
            .projections
            .iter()
            .any(|p| matches!(p, AmirProjection::Deref));

        let mut ptr_val = if !through_ptr {
            if let Some(&slot) = self.local_stack_slots.get(&place.local) {
                self.builder.ins().stack_addr(self.ptr_type, slot, 0)
            } else if let Some(&var) = self.local_map.get(&place.local) {
                self.builder.use_var(var)
            } else if let Some(&(var_ptr, _)) = self.str_local_map.get(&place.local) {
                self.builder.use_var(var_ptr)
            } else {
                self.record_ice(
                    "use of undeclared AMIR local in codegen",
                    self.local_span(place.local),
                );
                return (self.poison_i32(), 0);
            }
        } else {
            // BC.4a: local holds the base pointer; do not take address of the stack slot.
            if let Some(&var) = self.local_map.get(&place.local) {
                self.builder.use_var(var)
            } else {
                self.record_ice(
                    "BC.4a: heap/ptr borrow of undeclared local",
                    self.local_span(place.local),
                );
                return (self.poison_i32(), 0);
            }
        };

        let mut current_ty = self.local_ar_ty(place.local);
        let projs = &place.projections;

        // Walk all but last projection (loads for nested pointers).
        for i in 0..projs.len().saturating_sub(1) {
            match &projs[i] {
                AmirProjection::Deref => {
                    current_ty = unwrap_ptr_like(&current_ty, self);
                }
                AmirProjection::Field(symbol_id) => {
                    let offset = self.translate_projection_offset(&mut current_ty, *symbol_id);
                    ptr_val = self.builder.ins().load(
                        self.ptr_type,
                        cranelift_codegen::ir::MemFlagsData::new(),
                        ptr_val,
                        offset,
                    );
                }
                AmirProjection::Index(op) => {
                    let idx_val = self.translate_operand(op, Some(self.ptr_type));
                    if matches!(current_ty, ArType::Slice(_)) {
                        ptr_val = self.builder.ins().load(
                            self.ptr_type,
                            cranelift_codegen::ir::MemFlagsData::new(),
                            ptr_val,
                            0,
                        );
                    }
                    let inner_ty_id = match &current_ty {
                        ArType::Ptr(inner) | ArType::Slice(inner) | ArType::Array(_, inner) => {
                            *inner
                        }
                        ArType::Ref(inner) | ArType::RefMut(inner) => *inner,
                        _ => {
                            self.record_ice(
                                "indexing non-indexable type in codegen",
                                self.local_span(place.local),
                            );
                            return (self.poison_i32(), 0);
                        }
                    };
                    let inner_ty = self.type_info.resolve_type_id(inner_ty_id).clone();
                    current_ty = inner_ty;

                    let layout = self.checked_layout(&current_ty);
                    let elem_size = self.builder.ins().iconst(self.ptr_type, layout.size as i64);
                    let offset_val = self.builder.ins().imul(idx_val, elem_size);
                    ptr_val = self.builder.ins().iadd(ptr_val, offset_val);
                }
            }
        }

        let Some(last_proj) = projs.last() else {
            return (ptr_val, 0);
        };
        match last_proj {
            AmirProjection::Deref => {
                current_ty = unwrap_ptr_like(&current_ty, self);
                let _ = current_ty;
                (ptr_val, 0)
            }
            AmirProjection::Field(symbol_id) => {
                // After Deref, current_ty may still be Ptr/Ref — unwrap for field layout.
                if matches!(
                    current_ty,
                    ArType::Ptr(_) | ArType::Ref(_) | ArType::RefMut(_) | ArType::Nullable(_)
                ) {
                    current_ty = unwrap_ptr_like(&current_ty, self);
                }
                let offset = self.translate_projection_offset(&mut current_ty, *symbol_id);
                (ptr_val, offset)
            }
            AmirProjection::Index(op) => {
                if matches!(
                    current_ty,
                    ArType::Ptr(_) | ArType::Ref(_) | ArType::RefMut(_) | ArType::Nullable(_)
                ) {
                    current_ty = unwrap_ptr_like(&current_ty, self);
                }
                let idx_val = self.translate_operand(op, Some(self.ptr_type));
                if matches!(current_ty, ArType::Slice(_)) {
                    ptr_val = self.builder.ins().load(
                        self.ptr_type,
                        cranelift_codegen::ir::MemFlagsData::new(),
                        ptr_val,
                        0,
                    );
                }
                let inner_ty_id = match &current_ty {
                    ArType::Ptr(inner) | ArType::Slice(inner) | ArType::Array(_, inner) => *inner,
                    ArType::Ref(inner) | ArType::RefMut(inner) => *inner,
                    _ => {
                        self.record_ice(
                            "indexing non-indexable type in codegen",
                            self.local_span(place.local),
                        );
                        return (self.poison_i32(), 0);
                    }
                };
                let inner_ty = self.type_info.resolve_type_id(inner_ty_id).clone();
                current_ty = inner_ty;

                let layout = self.checked_layout(&current_ty);
                let elem_size = self.builder.ins().iconst(self.ptr_type, layout.size as i64);
                let offset_val = self.builder.ins().imul(idx_val, elem_size);
                let target_ptr = self.builder.ins().iadd(ptr_val, offset_val);
                (target_ptr, 0)
            }
        }
    }

    pub(super) fn translate_store_place(&mut self, lhs: &AmirPlace, val: Value) {
        if self.error.is_some() {
            return;
        }
        if lhs.projections.is_empty() {
            if let Some(&var) = self.local_map.get(&lhs.local) {
                self.builder.def_var(var, val);
            }
            if let Some(&slot) = self.local_stack_slots.get(&lhs.local) {
                let addr = self.builder.ins().stack_addr(self.ptr_type, slot, 0);
                self.builder
                    .ins()
                    .store(cranelift_codegen::ir::MemFlagsData::new(), val, addr, 0);
            } else if !self.local_map.contains_key(&lhs.local)
                && !self.str_local_map.contains_key(&lhs.local)
            {
                self.record_ice(
                    "use of undeclared AMIR local in codegen",
                    self.local_span(lhs.local),
                );
            }
        } else {
            let (base_ptr, offset) = self.translate_place_address_for_load(lhs);
            self.builder.ins().store(
                cranelift_codegen::ir::MemFlagsData::new(),
                val,
                base_ptr,
                offset,
            );
        }
    }

    pub(super) fn translate_projection_offset(
        &mut self,
        current_ty: &mut ArType,
        symbol_id: arandu_semantics::SymbolId,
    ) -> i32 {
        // Unwrap one pointer layer for field access on `*p.field` bases.
        let struct_ty = match &*current_ty {
            ArType::Ptr(inner)
            | ArType::Ref(inner)
            | ArType::RefMut(inner)
            | ArType::Nullable(inner) => self.type_info.resolve_type_id(*inner),
            other => other.clone(),
        };

        let field_name = self.symbol_table.get(symbol_id).name.clone();
        let field_idx = self
            .type_info
            .struct_field_indices
            .get(&match &struct_ty {
                ArType::Named(id, _) => *id,
                _ => {
                    *current_ty = ArType::Error;
                    return 0;
                }
            })
            .and_then(|m| m.get(field_name.as_str()).copied())
            .unwrap_or(0);

        let layout = self.checked_layout(&struct_ty);
        let offset = layout.field_offsets.get(field_idx).copied().unwrap_or(0) as i32;

        // Update current_ty to the field type for nested projections.
        if let ArType::Named(sid, _) = &struct_ty {
            if let Some(fields) = self.type_info.struct_fields.get(sid) {
                if let Some(&tid) = fields.get(field_name.as_str()) {
                    *current_ty = self.type_info.resolve_type_id(tid);
                    return offset;
                }
            }
        }
        *current_ty = ArType::Error;
        offset
    }
}

fn unwrap_ptr_like<M: cranelift_module::Module>(
    ty: &ArType,
    this: &FunctionTranslator<'_, '_, M>,
) -> ArType {
    match ty {
        ArType::Ptr(inner)
        | ArType::Ref(inner)
        | ArType::RefMut(inner)
        | ArType::Nullable(inner) => this.type_info.resolve_type_id(*inner),
        other => other.clone(),
    }
}
