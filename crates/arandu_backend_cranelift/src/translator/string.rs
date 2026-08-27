use arandu_semantics::amir::{AmirOperand, AmirRvalue};
use arandu_semantics::passes::type_checker::types::{ArType, Primitive};
use cranelift_codegen::ir::{InstBuilder, Value};

use super::FunctionTranslator;

impl<M: cranelift_module::Module> FunctionTranslator<'_, '_, M> {
    pub(super) fn translate_str_rvalue(&mut self, rvalue: &AmirRvalue) -> (Value, Value) {
        if self.error.is_some() {
            return (self.poison_i32(), self.poison_i32());
        }

        match rvalue {
            AmirRvalue::Use(op) => self.translate_str_operand(op),
            AmirRvalue::Load(place) => {
                if place.projections.is_empty() {
                    if let Some(&(var_ptr, var_len)) = self.str_local_map.get(&place.local) {
                        (self.builder.use_var(var_ptr), self.builder.use_var(var_len))
                    } else {
                        self.record_ice(
                            "use of undeclared AMIR local str in codegen",
                            self.local_span(place.local),
                        );
                        (self.poison_i32(), self.poison_i32())
                    }
                } else {
                    let (ptr_val, offset) = self.translate_place_address_for_load(place);
                    let loaded_ptr = self.builder.ins().load(
                        self.ptr_type,
                        cranelift_codegen::ir::MemFlagsData::new(),
                        ptr_val,
                        offset,
                    );
                    let loaded_len = self.builder.ins().load(
                        self.ptr_type,
                        cranelift_codegen::ir::MemFlagsData::new(),
                        ptr_val,
                        offset + self.ptr_type.bytes() as i32,
                    );
                    (loaded_ptr, loaded_len)
                }
            }
            AmirRvalue::FieldAccess { base, field } => {
                let ptr_val = self.translate_operand(base, Some(self.ptr_type));
                let base_ty = match base {
                    AmirOperand::Copy(temp_id) | AmirOperand::Move(temp_id) => {
                        self.temp_ar_ty(*temp_id)
                    }
                    _ => arandu_semantics::types::ArType::Error,
                };
                let struct_ty = match base_ty {
                    arandu_semantics::types::ArType::Ptr(inner)
                    | arandu_semantics::types::ArType::Ref(inner)
                    | arandu_semantics::types::ArType::RefMut(inner)
                    | arandu_semantics::types::ArType::Nullable(inner) => {
                        self.type_info.resolve_type_id(inner)
                    }
                    other => other,
                };
                let pointer_width = self.ptr_type.bytes() as u64;
                let layout = self.checked_layout(&struct_ty);
                let offset = layout.field_offsets.get(*field).copied().unwrap_or(0) as i32;

                let loaded_ptr = self.builder.ins().load(
                    self.ptr_type,
                    cranelift_codegen::ir::MemFlagsData::new(),
                    ptr_val,
                    offset,
                );
                let loaded_len = self.builder.ins().load(
                    self.ptr_type,
                    cranelift_codegen::ir::MemFlagsData::new(),
                    ptr_val,
                    offset + pointer_width as i32,
                );
                (loaded_ptr, loaded_len)
            }
            AmirRvalue::EnumPayload {
                value,
                variant,
                index,
            } => {
                let ptr_val = self.translate_operand(value, Some(self.ptr_type));
                let pointer_width = self.ptr_type.bytes() as u64;

                let base_ty = match value {
                    AmirOperand::Copy(temp_id) | AmirOperand::Move(temp_id) => {
                        self.temp_ar_ty(*temp_id)
                    }
                    _ => arandu_semantics::types::ArType::Error,
                };
                let enum_ty = match base_ty {
                    arandu_semantics::types::ArType::Ptr(inner) => {
                        self.type_info.resolve_type_id(inner)
                    }
                    other => other,
                };
                let enum_id = match enum_ty {
                    ArType::Named(enum_id, _) => enum_id,
                    _ => arandu_semantics::SymbolId::DUMMY,
                };

                let mut payload_offset = 0;
                if let Some(variants) =
                    arandu_semantics::layout::StructLayoutProvider::get_enum_variants(
                        self.type_info,
                        enum_id,
                    )
                {
                    let tag = self
                        .type_info
                        .enum_variant_tags
                        .get(variant)
                        .copied()
                        .unwrap_or(0);
                    if let Some(variant_shape) = variants.get(tag) {
                        if let Some(payload_ty_id) = variant_shape.payload_ty {
                            let payload_ty = self.type_info.resolve_type_id(payload_ty_id);
                            let payload_layout = self.checked_layout(&payload_ty);
                            if *index < payload_layout.field_offsets.len() {
                                payload_offset = payload_layout.field_offsets[*index] as i32;
                            }
                        }
                    }
                }

                let total_offset = pointer_width as i32 + payload_offset;
                let loaded_ptr = self.builder.ins().load(
                    self.ptr_type,
                    cranelift_codegen::ir::MemFlagsData::new(),
                    ptr_val,
                    total_offset,
                );
                let loaded_len = self.builder.ins().load(
                    self.ptr_type,
                    cranelift_codegen::ir::MemFlagsData::new(),
                    ptr_val,
                    total_offset + self.ptr_type.bytes() as i32,
                );
                (loaded_ptr, loaded_len)
            }
            AmirRvalue::StringInterp { parts } => self.translate_string_interp(parts),
            AmirRvalue::ToStr { value, src_ty } => self.translate_to_str(value, *src_ty),
            _ => {
                self.record_ice(
                    "unsupported rvalue kind returning str in codegen",
                    self.func_span(),
                );
                (self.poison_i32(), self.poison_i32())
            }
        }
    }

    /// Format a primitive via host ToStr helpers → `(ptr, len)`.
    fn translate_to_str(
        &mut self,
        value: &AmirOperand,
        src_ty: arandu_semantics::types::TypeId,
    ) -> (Value, Value) {
        let ar_ty = self.type_info.type_interner.resolve(src_ty);
        if matches!(ar_ty, ArType::Primitive(Primitive::Str)) {
            return self.translate_str_operand(value);
        }

        let i64_ty = cranelift_codegen::ir::types::I64;
        let helper_name = match &ar_ty {
            ArType::Primitive(Primitive::Bool) => "ar_jit_bool_to_str",
            ArType::Primitive(Primitive::Char) => "ar_jit_char_to_str",
            ArType::FloatLiteral => "ar_jit_f64_to_str",
            ArType::Primitive(p) if p.is_float() => "ar_jit_f64_to_str",
            ArType::IntLiteral => "ar_jit_i64_to_str",
            ArType::Primitive(p) if p.is_integer() && p.is_signed() => "ar_jit_i64_to_str",
            ArType::Primitive(p) if p.is_integer() => "ar_jit_u64_to_str",
            ArType::Err => "ar_jit_err_to_str",
            _ => {
                self.record_ice(
                    format!("ToStr v0.1 unsupported type in Cranelift: {ar_ty:?}"),
                    self.func_span(),
                );
                return (self.poison_i32(), self.poison_i32());
            }
        };

        // Stack slot for out_len: i64
        let len_slot = self
            .builder
            .create_sized_stack_slot(cranelift_codegen::ir::StackSlotData {
                kind: cranelift_codegen::ir::StackSlotKind::ExplicitSlot,
                size: 8,
                align_shift: 3,
                key: None,
            });
        let len_ptr = self.builder.ins().stack_addr(self.ptr_type, len_slot, 0);

        let arg = match helper_name {
            "ar_jit_bool_to_str" => {
                let v = self.translate_operand(value, Some(cranelift_codegen::ir::types::I8));
                // Ensure i8 width
                if self.builder.func.dfg.value_type(v) != cranelift_codegen::ir::types::I8 {
                    self.builder
                        .ins()
                        .ireduce(cranelift_codegen::ir::types::I8, v)
                } else {
                    v
                }
            }
            "ar_jit_char_to_str" => {
                let v = self.translate_operand(value, Some(cranelift_codegen::ir::types::I32));
                let vt = self.builder.func.dfg.value_type(v);
                if vt == cranelift_codegen::ir::types::I32 {
                    v
                } else if vt.bits() < 32 {
                    self.builder
                        .ins()
                        .uextend(cranelift_codegen::ir::types::I32, v)
                } else {
                    self.builder
                        .ins()
                        .ireduce(cranelift_codegen::ir::types::I32, v)
                }
            }
            "ar_jit_f64_to_str" => {
                let v = self.translate_operand(value, Some(cranelift_codegen::ir::types::F64));
                let vt = self.builder.func.dfg.value_type(v);
                if vt == cranelift_codegen::ir::types::F64 {
                    v
                } else if vt == cranelift_codegen::ir::types::F32 {
                    self.builder
                        .ins()
                        .fpromote(cranelift_codegen::ir::types::F64, v)
                } else {
                    v
                }
            }
            "ar_jit_i64_to_str" | "ar_jit_u64_to_str" => {
                let v = self.translate_operand(value, Some(i64_ty));
                let vt = self.builder.func.dfg.value_type(v);
                if vt == i64_ty {
                    v
                } else if vt.bits() < 64 {
                    if helper_name == "ar_jit_u64_to_str" {
                        self.builder.ins().uextend(i64_ty, v)
                    } else {
                        self.builder.ins().sextend(i64_ty, v)
                    }
                } else {
                    self.builder.ins().ireduce(i64_ty, v)
                }
            }
            "ar_jit_err_to_str" => self.translate_operand(value, Some(self.ptr_type)),
            _ => unreachable!(),
        };

        let Some(func_id) = self.func_ids.get(helper_name).copied() else {
            self.record_ice(
                format!("{helper_name} was not declared in the JIT module"),
                self.func_span(),
            );
            return (self.poison_i32(), self.poison_i32());
        };
        let local_ref = self.module.declare_func_in_func(func_id, self.builder.func);
        let call = self.builder.ins().call(local_ref, &[arg, len_ptr]);
        let ptr = self.builder.inst_results(call)[0];
        let len = self
            .builder
            .ins()
            .stack_load(self.ptr_type, i64_ty, len_slot, 0);
        let len = if self.ptr_type.bits() < 64 {
            self.builder.ins().ireduce(self.ptr_type, len)
        } else {
            len
        };
        (ptr, len)
    }

    /// Compare two `str` fat pointers for equality (`==` / `!=`).
    /// Equal when lengths match and `memcmp` reports zero (empty strings included).
    pub(super) fn translate_str_eq(
        &mut self,
        left: &AmirOperand,
        right: &AmirOperand,
        op: arandu_semantics::ops::BinaryOp,
    ) -> Value {
        let (l_ptr, l_len) = self.translate_str_operand(left);
        let (r_ptr, r_len) = self.translate_str_operand(right);
        let i8_ty = cranelift_codegen::ir::types::I8;
        let i32_ty = cranelift_codegen::ir::types::I32;
        let len_eq =
            self.builder
                .ins()
                .icmp(cranelift_codegen::ir::condcodes::IntCC::Equal, l_len, r_len);

        // If lengths differ → not equal. If both zero-length → equal.
        // Else memcmp(l_ptr, r_ptr, len) == 0.
        let zero_len = self.builder.ins().iconst(self.ptr_type, 0);
        let len_nonzero = self.builder.ins().icmp(
            cranelift_codegen::ir::condcodes::IntCC::NotEqual,
            l_len,
            zero_len,
        );

        let Some(memcmp_id) = self.memcmp_func_id() else {
            return self.poison_i32();
        };
        let memcmp_ref = self
            .module
            .declare_func_in_func(memcmp_id, self.builder.func);
        // memcmp size is size_t (= pointer width).
        let size = l_len;

        let call = self.builder.ins().call(memcmp_ref, &[l_ptr, r_ptr, size]);
        let cmp = self.builder.inst_results(call)[0];
        let zero_i32 = self.builder.ins().iconst(i32_ty, 0);
        let bytes_eq = self.builder.ins().icmp(
            cranelift_codegen::ir::condcodes::IntCC::Equal,
            cmp,
            zero_i32,
        );

        // content_eq = !len_nonzero || bytes_eq  (icmp results are already i8 bools)
        let not_nonzero = self.builder.ins().bxor_imm_u(len_nonzero, 1);
        let content_eq = self.builder.ins().bor(not_nonzero, bytes_eq);
        let eq = self.builder.ins().band(len_eq, content_eq);
        let _ = i8_ty;

        match op {
            arandu_semantics::ops::BinaryOp::Equal => eq,
            arandu_semantics::ops::BinaryOp::NotEqual => self.builder.ins().bxor_imm_u(eq, 1),
            _ => self.poison_i32(),
        }
    }

    /// Concatenate `str` fat-pointer parts via `malloc` + `memcpy`.
    /// Returns `(ptr, len)` for the newly allocated buffer (not freed; debug/JIT lifetime).
    fn translate_string_interp(&mut self, parts: &[AmirOperand]) -> (Value, Value) {
        if parts.is_empty() {
            let empty_ptr = self.builder.ins().iconst(self.ptr_type, 0);
            let empty_len = self.builder.ins().iconst(self.ptr_type, 0);
            return (empty_ptr, empty_len);
        }

        // Materialize each part as (ptr, len).
        let mut part_vals: Vec<(Value, Value)> = Vec::with_capacity(parts.len());
        for part in parts {
            part_vals.push(self.translate_str_operand(part));
        }

        // total = sum of lengths (ptr_type)
        let mut total = self.builder.ins().iconst(self.ptr_type, 0);
        for &(_, len) in &part_vals {
            total = self.builder.ins().iadd(total, len);
        }

        // malloc(total + 1) for trailing NUL safety
        let one = self.builder.ins().iconst(self.ptr_type, 1);
        let alloc_size = self.builder.ins().iadd(total, one);

        let Some(malloc_id) = self.malloc_func_id() else {
            return (self.poison_i32(), self.poison_i32());
        };
        let malloc_ref = self
            .module
            .declare_func_in_func(malloc_id, self.builder.func);
        let call = self.builder.ins().call(malloc_ref, &[alloc_size]);
        let buf = self.builder.inst_results(call)[0];

        let Some(memcpy_id) = self.memcpy_func_id() else {
            return (self.poison_i32(), self.poison_i32());
        };
        let memcpy_ref = self
            .module
            .declare_func_in_func(memcpy_id, self.builder.func);

        // Copy each part into the buffer.
        let mut offset_ptr = self.builder.ins().iconst(self.ptr_type, 0);
        for &(src_ptr, src_len) in &part_vals {
            let dest = self.builder.ins().iadd(buf, offset_ptr);
            self.builder
                .ins()
                .call(memcpy_ref, &[dest, src_ptr, src_len]);
            offset_ptr = self.builder.ins().iadd(offset_ptr, src_len);
        }

        // Write trailing NUL at buf + total
        let zero_byte = self
            .builder
            .ins()
            .iconst(cranelift_codegen::ir::types::I8, 0);
        let end_ptr = self.builder.ins().iadd(buf, total);
        self.builder.ins().store(
            cranelift_codegen::ir::MemFlagsData::new(),
            zero_byte,
            end_ptr,
            0,
        );

        (buf, total)
    }
}
