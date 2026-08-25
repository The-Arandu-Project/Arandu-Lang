use arandu_semantics::amir::{AmirConstant, AmirOperand, AmirRvalue};
use arandu_semantics::ops::UnaryOp;
use arandu_semantics::passes::type_checker::types::{ArType, Primitive};
use cranelift_codegen::ir::{InstBuilder, Type, Value};

use super::FunctionTranslator;

impl<M: cranelift_module::Module> FunctionTranslator<'_, '_, M> {
    /// Box a scalar into a heap cell for `T?` (null-or-pointer ABI).
    fn box_nullable_scalar(&mut self, val: Value, inner: &ArType) -> Value {
        let Some(malloc_id) = self.malloc_func_id() else {
            return self.poison_i32();
        };
        let malloc_ref = self
            .module
            .declare_func_in_func(malloc_id, self.builder.func);
        let layout = self.checked_layout(inner);
        let size = self
            .builder
            .ins()
            .iconst(self.ptr_type, layout.size.max(1) as i64);
        let call = self.builder.ins().call(malloc_ref, &[size]);
        let ptr = self.builder.inst_results(call)[0];
        self.builder
            .ins()
            .store(cranelift_codegen::ir::MemFlagsData::new(), val, ptr, 0);
        ptr
    }

    /// Load a boxed scalar from a non-null `T?` handle.
    fn unbox_nullable_scalar(&mut self, handle: Value, inner: &ArType) -> Value {
        let clif = match crate::types::clif_type(inner, self.ptr_type) {
            crate::types::ClifType::Concrete(t) => t,
            crate::types::ClifType::Void => return self.poison_i32(),
        };
        self.builder
            .ins()
            .load(clif, cranelift_codegen::ir::MemFlagsData::new(), handle, 0)
    }

    pub(super) fn translate_rvalue(
        &mut self,
        rvalue: &AmirRvalue,
        expected_ty: Option<Type>,
        expected_ar_type: Option<&ArType>,
    ) -> Value {
        if self.error.is_some() {
            return self.poison_i32();
        }

        // ── Nullable handle ABI ──────────────────────────────────────────
        // `T?` is always a pointer: null = nil; non-null = object ptr or
        // boxed scalar. Box/unbox keeps `int? = 0` distinct from `nil`.
        if let Some(ArType::Nullable(inner_id)) = expected_ar_type {
            let inner = self.type_info.type_interner.resolve(*inner_id);
            if matches!(
                rvalue,
                AmirRvalue::Use(AmirOperand::Constant(AmirConstant::Nil))
            ) {
                return self.builder.ins().iconst(self.ptr_type, 0);
            }
            // Already a nullable handle (copy/move or nested) → pass through.
            if let AmirRvalue::Use(op) = rvalue {
                let op_ty = self.get_operand_ar_type(op);
                if matches!(op_ty, ArType::Nullable(_)) {
                    return self.translate_operand(op, Some(self.ptr_type));
                }
            }
            // Produce the inner value, then box scalars.
            let inner_clif = match crate::types::clif_type(&inner, self.ptr_type) {
                crate::types::ClifType::Concrete(t) => Some(t),
                crate::types::ClifType::Void => None,
            };
            let raw = self.translate_rvalue_inner(rvalue, inner_clif, Some(&inner));
            if inner.needs_nullable_box() {
                return self.box_nullable_scalar(raw, &inner);
            }
            return raw;
        }

        // Unbox when assigning a `T?` handle into a non-nullable `T` (e.g. `??`).
        if let AmirRvalue::Use(op) = rvalue {
            let op_ty = self.get_operand_ar_type(op);
            if let ArType::Nullable(inner_id) = &op_ty {
                let inner = self.type_info.type_interner.resolve(*inner_id);
                if expected_ar_type.is_none_or(|e| !matches!(e, ArType::Nullable(_)))
                    && inner.needs_nullable_box()
                {
                    let handle = self.translate_operand(op, Some(self.ptr_type));
                    return self.unbox_nullable_scalar(handle, &inner);
                }
            }
        }

        self.translate_rvalue_inner(rvalue, expected_ty, expected_ar_type)
    }

    fn translate_rvalue_inner(
        &mut self,
        rvalue: &AmirRvalue,
        expected_ty: Option<Type>,
        expected_ar_type: Option<&ArType>,
    ) -> Value {
        if self.error.is_some() {
            return self.poison_i32();
        }

        match rvalue {
            AmirRvalue::Use(op) => self.translate_operand(op, expected_ty),
            AmirRvalue::Binary { op, left, right } => {
                // `str` equality uses fat pointers + memcmp (not scalar icmp).
                let left_is_str = matches!(
                    self.get_operand_ar_type(left),
                    ArType::Primitive(Primitive::Str)
                );
                let right_is_str = matches!(
                    self.get_operand_ar_type(right),
                    ArType::Primitive(Primitive::Str)
                );
                if left_is_str || right_is_str {
                    match op {
                        arandu_semantics::ops::BinaryOp::Equal
                        | arandu_semantics::ops::BinaryOp::NotEqual => {
                            return self.translate_str_eq(left, right, *op);
                        }
                        _ => {
                            self.record_ice(
                                "unsupported binary op on str in codegen",
                                self.func_span(),
                            );
                            return self.poison_i32();
                        }
                    }
                }
                let opt_ty = match op {
                    arandu_semantics::ops::BinaryOp::Add
                    | arandu_semantics::ops::BinaryOp::Sub
                    | arandu_semantics::ops::BinaryOp::Mul
                    | arandu_semantics::ops::BinaryOp::Div
                    | arandu_semantics::ops::BinaryOp::Mod
                    | arandu_semantics::ops::BinaryOp::BitOr
                    | arandu_semantics::ops::BinaryOp::BitXor
                    | arandu_semantics::ops::BinaryOp::BitAnd
                    | arandu_semantics::ops::BinaryOp::ShiftLeft
                    | arandu_semantics::ops::BinaryOp::ShiftRight => expected_ty,
                    // Comparisons (incl. `x == nil` / `x != nil`): prefer the
                    // non-constant side's ABI type so Nil is a zero of matching width.
                    arandu_semantics::ops::BinaryOp::Equal
                    | arandu_semantics::ops::BinaryOp::NotEqual
                    | arandu_semantics::ops::BinaryOp::Lt
                    | arandu_semantics::ops::BinaryOp::LtEqual
                    | arandu_semantics::ops::BinaryOp::Gt
                    | arandu_semantics::ops::BinaryOp::GtEqual => {
                        let left_ty = match left {
                            AmirOperand::Copy(t) | AmirOperand::Move(t) => {
                                self.get_temp_clif_type(*t)
                            }
                            _ => None,
                        };
                        let right_ty = match right {
                            AmirOperand::Copy(t) | AmirOperand::Move(t) => {
                                self.get_temp_clif_type(*t)
                            }
                            _ => None,
                        };
                        left_ty.or(right_ty).or(expected_ty)
                    }
                    _ => None,
                };
                let lhs = self.translate_operand(left, opt_ty);
                let rhs = self.translate_operand(right, opt_ty);
                self.translate_binary_op(*op, lhs, rhs, Some(left), Some(right))
            }
            AmirRvalue::Unary { op, operand } => {
                // Deref loads through a pointer at offset 0.
                if matches!(op, UnaryOp::Deref) {
                    let ptr = self.translate_operand(operand, Some(self.ptr_type));
                    let load_ty = expected_ty.unwrap_or(self.ptr_type);
                    return self.builder.ins().load(
                        load_ty,
                        cranelift_codegen::ir::MemFlagsData::new(),
                        ptr,
                        0,
                    );
                }
                // A3.6: await = block_on(poll) until Ready. State layout:
                //   +0 disc (u32, 0=Ready), +8 payload.
                if matches!(op, UnaryOp::Await) {
                    return self.translate_await_block_on(operand, expected_ty);
                }
                let val = self.translate_operand(operand, expected_ty);
                self.translate_unary_op(*op, val)
            }
            AmirRvalue::Load(place) => {
                if place.projections.is_empty() {
                    // Address-taken scalar: load from stack home (F2.0).
                    if let Some(&slot) = self.local_stack_slots.get(&place.local) {
                        let addr = self.builder.ins().stack_addr(self.ptr_type, slot, 0);
                        let clif_ty = expected_ty.unwrap_or(self.ptr_type);
                        self.builder.ins().load(
                            clif_ty,
                            cranelift_codegen::ir::MemFlagsData::new(),
                            addr,
                            0,
                        )
                    } else {
                        match self.local_map.get(&place.local) {
                            Some(var) => self.builder.use_var(*var),
                            None => {
                                self.record_ice(
                                    "use of undeclared AMIR local in codegen",
                                    self.local_span(place.local),
                                );
                                self.poison_i32()
                            }
                        }
                    }
                } else {
                    let (base_ptr, offset) = self.translate_place_address_for_load(place);
                    let clif_ty = expected_ty.unwrap_or(self.ptr_type);
                    self.builder.ins().load(
                        clif_ty,
                        cranelift_codegen::ir::MemFlagsData::new(),
                        base_ptr,
                        offset,
                    )
                }
            }

            AmirRvalue::StructLiteral {
                struct_symbol,
                fields,
            } => {
                let Some(malloc_func_id) = self.malloc_func_id() else {
                    return self.poison_i32();
                };
                let local_ref = self
                    .module
                    .declare_func_in_func(malloc_func_id, self.builder.func);

                let pointer_width = self.ptr_type.bytes() as u64;
                let struct_ty = expected_ar_type.cloned().unwrap_or_else(|| {
                    arandu_semantics::types::ArType::Named(*struct_symbol, Vec::new())
                });
                let layout = self.checked_layout(&struct_ty);

                let size_val = self.builder.ins().iconst(self.ptr_type, layout.size as i64);
                let call_inst = self.builder.ins().call(local_ref, &[size_val]);
                let ptr_val = self.builder.inst_results(call_inst)[0];

                for (i, (name, op)) in fields.iter().enumerate() {
                    let field_idx = self
                        .type_info
                        .struct_field_indices
                        .get(struct_symbol)
                        .and_then(|m| m.get(name.as_str()).copied())
                        .unwrap_or(i);
                    let offset = layout.field_offsets.get(field_idx).copied().unwrap_or(0) as i32;
                    let op_ty = self.get_operand_ar_type(op);
                    if matches!(op_ty, ArType::Primitive(Primitive::Str)) {
                        let (elem_ptr, elem_len) = self.translate_str_operand(op);
                        self.builder.ins().store(
                            cranelift_codegen::ir::MemFlagsData::new(),
                            elem_ptr,
                            ptr_val,
                            offset,
                        );
                        self.builder.ins().store(
                            cranelift_codegen::ir::MemFlagsData::new(),
                            elem_len,
                            ptr_val,
                            offset + pointer_width as i32,
                        );
                    } else {
                        let field_defs = self.type_info.struct_fields.get(struct_symbol);
                        let field_ty = field_defs
                            .and_then(|m| m.get(name.as_str()).copied())
                            .map(|tid| self.type_info.type_interner.resolve(tid))
                            .unwrap_or(ArType::Error);
                        let expected_field_ty =
                            match crate::types::clif_type(&field_ty, self.ptr_type) {
                                crate::types::ClifType::Concrete(ty) => Some(ty),
                                crate::types::ClifType::Void => None,
                            };
                        let val = self.translate_operand(op, expected_field_ty);
                        self.builder.ins().store(
                            cranelift_codegen::ir::MemFlagsData::new(),
                            val,
                            ptr_val,
                            offset,
                        );
                    }
                }
                ptr_val
            }
            AmirRvalue::Tuple { items } => {
                let Some(malloc_func_id) = self.malloc_func_id() else {
                    return self.poison_i32();
                };
                let local_ref = self
                    .module
                    .declare_func_in_func(malloc_func_id, self.builder.func);

                let pointer_width = self.ptr_type.bytes() as u64;
                let tuple_ty = expected_ar_type.cloned().unwrap_or(ArType::Error);
                let layout = self.checked_layout(&tuple_ty);

                let size_val = self.builder.ins().iconst(self.ptr_type, layout.size as i64);
                let call_inst = self.builder.ins().call(local_ref, &[size_val]);
                let ptr_val = self.builder.inst_results(call_inst)[0];

                for (i, op) in items.iter().enumerate() {
                    let offset = layout.field_offsets.get(i).copied().unwrap_or(0) as i32;
                    let op_ty = self.get_operand_ar_type(op);
                    if matches!(op_ty, ArType::Primitive(Primitive::Str)) {
                        let (elem_ptr, elem_len) = self.translate_str_operand(op);
                        self.builder.ins().store(
                            cranelift_codegen::ir::MemFlagsData::new(),
                            elem_ptr,
                            ptr_val,
                            offset,
                        );
                        self.builder.ins().store(
                            cranelift_codegen::ir::MemFlagsData::new(),
                            elem_len,
                            ptr_val,
                            offset + pointer_width as i32,
                        );
                    } else {
                        let elem_ty = match &tuple_ty {
                            ArType::Tuple(tids) => tids
                                .get(i)
                                .map(|&tid| self.type_info.type_interner.resolve(tid))
                                .unwrap_or(ArType::Error),
                            _ => ArType::Error,
                        };
                        let expected_elem_ty =
                            match crate::types::clif_type(&elem_ty, self.ptr_type) {
                                crate::types::ClifType::Concrete(ty) => Some(ty),
                                crate::types::ClifType::Void => None,
                            };
                        let val = self.translate_operand(op, expected_elem_ty);
                        self.builder.ins().store(
                            cranelift_codegen::ir::MemFlagsData::new(),
                            val,
                            ptr_val,
                            offset,
                        );
                    }
                }
                ptr_val
            }
            AmirRvalue::Array { items } => {
                let Some(malloc_func_id) = self.malloc_func_id() else {
                    return self.poison_i32();
                };
                let local_ref = self
                    .module
                    .declare_func_in_func(malloc_func_id, self.builder.func);

                let pointer_width = self.ptr_type.bytes() as u64;
                let array_ty = expected_ar_type.cloned().unwrap_or(ArType::Error);
                let layout = self.checked_layout(&array_ty);

                let size_val = self.builder.ins().iconst(self.ptr_type, layout.size as i64);
                let call_inst = self.builder.ins().call(local_ref, &[size_val]);
                let ptr_val = self.builder.inst_results(call_inst)[0];

                let item_ar_ty = match &array_ty {
                    ArType::Array(_, inner) => self.type_info.resolve_type_id(*inner),
                    _ => ArType::Error,
                };
                let item_layout = self.checked_layout(&item_ar_ty);
                let item_size = item_layout.size as i32;

                for (i, op) in items.iter().enumerate() {
                    let offset = i as i32 * item_size;
                    let op_ty = self.get_operand_ar_type(op);
                    if matches!(op_ty, ArType::Primitive(Primitive::Str)) {
                        let (elem_ptr, elem_len) = self.translate_str_operand(op);
                        self.builder.ins().store(
                            cranelift_codegen::ir::MemFlagsData::new(),
                            elem_ptr,
                            ptr_val,
                            offset,
                        );
                        self.builder.ins().store(
                            cranelift_codegen::ir::MemFlagsData::new(),
                            elem_len,
                            ptr_val,
                            offset + pointer_width as i32,
                        );
                    } else {
                        let expected_item_ty =
                            match crate::types::clif_type(&item_ar_ty, self.ptr_type) {
                                crate::types::ClifType::Concrete(ty) => Some(ty),
                                crate::types::ClifType::Void => None,
                            };
                        let val = self.translate_operand(op, expected_item_ty);
                        self.builder.ins().store(
                            cranelift_codegen::ir::MemFlagsData::new(),
                            val,
                            ptr_val,
                            offset,
                        );
                    }
                }
                ptr_val
            }

            AmirRvalue::FieldAccess { base, field } => {
                let ptr_val = self.translate_operand(base, Some(self.ptr_type));
                let base_ty = match base {
                    AmirOperand::Copy(temp_id) | AmirOperand::Move(temp_id) => {
                        self.temp_ar_ty(*temp_id)
                    }
                    _ => arandu_semantics::types::ArType::Error,
                };
                // Unwrap ptr / ref / nullable so layout sees the struct/tuple payload.
                // `shared`/`mut self` formals are `&T`/`&mut T` (pointer-sized SSA).
                let struct_ty = match base_ty {
                    arandu_semantics::types::ArType::Ptr(inner)
                    | arandu_semantics::types::ArType::Ref(inner)
                    | arandu_semantics::types::ArType::RefMut(inner)
                    | arandu_semantics::types::ArType::Nullable(inner) => {
                        self.type_info.resolve_type_id(inner)
                    }
                    other => other,
                };
                let layout = self.checked_layout(&struct_ty);
                let Some(&off) = layout.field_offsets.get(*field) else {
                    // Dead `p?.field` access branch with nil/ZST base, or incomplete layout.
                    return self.poison_i32();
                };
                let offset = off as i32;

                let clif_ty = expected_ty.unwrap_or(self.ptr_type);
                self.builder.ins().load(
                    clif_ty,
                    cranelift_codegen::ir::MemFlagsData::new(),
                    ptr_val,
                    offset,
                )
            }
            AmirRvalue::EnumConstruct {
                variant_tag,
                payload,
            } => {
                let Some(malloc_func_id) = self.malloc_func_id() else {
                    return self.poison_i32();
                };
                let local_ref = self
                    .module
                    .declare_func_in_func(malloc_func_id, self.builder.func);

                let pointer_width = self.ptr_type.bytes() as u64;
                let enum_ty = expected_ar_type.cloned().unwrap_or(ArType::Error);
                let layout = self.checked_layout(&enum_ty);

                let size_val = self.builder.ins().iconst(self.ptr_type, layout.size as i64);
                let call_inst = self.builder.ins().call(local_ref, &[size_val]);
                let ptr_val = self.builder.inst_results(call_inst)[0];

                let tag_val = self
                    .builder
                    .ins()
                    .iconst(self.ptr_type, *variant_tag as i64);
                self.builder.ins().store(
                    cranelift_codegen::ir::MemFlagsData::new(),
                    tag_val,
                    ptr_val,
                    0,
                );

                if let Some(op) = payload {
                    let op_ty = self.get_operand_ar_type(op);
                    let payload_ar_ty = match &enum_ty {
                        ArType::Named(enum_id, _) => {
                            arandu_semantics::layout::StructLayoutProvider::get_enum_variants(
                                self.type_info,
                                *enum_id,
                            )
                            .and_then(|variants| variants.get(*variant_tag).cloned())
                            .and_then(|shape| shape.payload_ty)
                        }
                        ArType::Result(ok, err) => match *variant_tag {
                            0 => Some(*ok),
                            1 => Some(*err),
                            _ => None,
                        },
                        // Option.Some = 1; Poll.Ready = 0.
                        ArType::Option(inner) if *variant_tag == 1 => Some(*inner),
                        ArType::Poll(inner) if *variant_tag == 0 => Some(*inner),
                        _ => None,
                    }
                    .map(|ty_id| self.type_info.resolve_type_id(ty_id));
                    // ZST payloads (void / typeck error) only need the discriminant tag.
                    // `Err` is a message handle (pointer) and is stored like other scalars.
                    if matches!(op_ty, ArType::Void | ArType::Error) {
                        // no payload bytes
                    } else if matches!(op_ty, ArType::Primitive(Primitive::Str)) {
                        let (elem_ptr, elem_len) = self.translate_str_operand(op);
                        self.builder.ins().store(
                            cranelift_codegen::ir::MemFlagsData::new(),
                            elem_ptr,
                            ptr_val,
                            pointer_width as i32,
                        );
                        self.builder.ins().store(
                            cranelift_codegen::ir::MemFlagsData::new(),
                            elem_len,
                            ptr_val,
                            (pointer_width * 2) as i32,
                        );
                    } else {
                        // Literals retain their source-level `IntLiteral`/`FloatLiteral`
                        // type in AMIR. Translate them using the variant's declared
                        // payload type: otherwise an `int` literal defaults to i32 while
                        // `int` is pointer-width, leaving the upper bytes uninitialized.
                        let payload_clif_ty = payload_ar_ty
                            .as_ref()
                            .and_then(|ty| crate::types::clif_type(ty, self.ptr_type).concrete())
                            // Some intrinsic constructors (notably the `Err` handle path)
                            // are represented by their already-concrete operand type.
                            .or_else(|| crate::types::clif_type(&op_ty, self.ptr_type).concrete());
                        if payload_clif_ty.is_none() {
                            self.record_ice(
                                format!(
                                    "missing concrete payload type for enum variant tag {variant_tag}"
                                ),
                                self.func_span(),
                            );
                        }
                        let val = self.translate_operand(op, payload_clif_ty);
                        self.builder.ins().store(
                            cranelift_codegen::ir::MemFlagsData::new(),
                            val,
                            ptr_val,
                            pointer_width as i32,
                        );
                    }
                }

                ptr_val
            }
            AmirRvalue::Discriminant { value } => {
                let ptr_val = self.translate_operand(value, Some(self.ptr_type));
                self.builder.ins().load(
                    self.ptr_type,
                    cranelift_codegen::ir::MemFlagsData::new(),
                    ptr_val,
                    0,
                )
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
                let clif_ty = expected_ty.unwrap_or(self.ptr_type);
                self.builder.ins().load(
                    clif_ty,
                    cranelift_codegen::ir::MemFlagsData::new(),
                    ptr_val,
                    total_offset,
                )
            }
            AmirRvalue::IndexAccess { base, index } => {
                let ptr_val = self.translate_operand(base, Some(self.ptr_type));
                let mut idx_val = self.translate_operand(index, None);
                let idx_ty = self.builder.func.dfg.value_type(idx_val);
                if idx_ty != self.ptr_type {
                    idx_val = self.builder.ins().uextend(self.ptr_type, idx_val);
                }

                let base_ty = self.get_operand_ar_type(base);
                let deref_ty = match &base_ty {
                    ArType::Ptr(inner) => self.type_info.resolve_type_id(*inner),
                    other => other.clone(),
                };
                let elem_ty = match deref_ty {
                    ArType::Array(_, elem) => self.type_info.resolve_type_id(elem),
                    ArType::Slice(elem) => self.type_info.resolve_type_id(elem),
                    _ => ArType::Error,
                };

                let layout = self.checked_layout(&elem_ty);

                let elem_size = self.builder.ins().iconst(self.ptr_type, layout.size as i64);
                let offset_val = self.builder.ins().imul(idx_val, elem_size);
                let target_ptr = self.builder.ins().iadd(ptr_val, offset_val);

                let clif_ty = expected_ty.unwrap_or(self.ptr_type);
                self.builder.ins().load(
                    clif_ty,
                    cranelift_codegen::ir::MemFlagsData::new(),
                    target_ptr,
                    0,
                )
            }
            AmirRvalue::Borrow(place) | AmirRvalue::BorrowMut(place) => {
                let ty = self.local_ar_ty(place.local);
                let local_is_memory = self.current_func.locals[place.local.as_usize()].is_memory;
                let has_stack_home = self.local_stack_slots.contains_key(&place.local);
                // BC.4a: Deref means base local already holds a materialised pointer
                // (heap/`ptr`/`&T`); address = use_var, never stack_addr of the slot.
                let through_ptr = place
                    .projections
                    .iter()
                    .any(|p| matches!(p, arandu_semantics::amir::AmirProjection::Deref));
                let is_memory_backed = through_ptr
                    || has_stack_home
                    || local_is_memory
                    || !place.projections.is_empty()
                    || matches!(
                        ty,
                        ArType::Tuple(_)
                            | ArType::Array(_, _)
                            | ArType::Slice(_)
                            | ArType::Primitive(Primitive::Str)
                            | ArType::Ref(_)
                            | ArType::RefMut(_)
                            | ArType::Ptr(_)
                    )
                    || matches!(
                        ty,
                        ArType::Named(sym_id, _) if matches!(
                            self.symbol_table.get(sym_id).kind,
                            arandu_semantics::SymbolKind::Struct | arandu_semantics::SymbolKind::Enum
                        )
                    );

                if is_memory_backed {
                    let (base_ptr, offset) = self.translate_place_address_for_load(place);
                    if offset == 0 {
                        base_ptr
                    } else {
                        let offset_val = self.builder.ins().iconst(self.ptr_type, offset as i64);
                        self.builder.ins().iadd(base_ptr, offset_val)
                    }
                } else {
                    self.record_error(
                        arandu_semantics::DiagCode::U001FeatureNotSupported,
                        "borrow of non-memory local without address_taken (F2.0 should mark is_memory)",
                        self.local_span(place.local),
                    );
                    self.poison_i32()
                }
            }
            // A3.4: pin-free — value is LocalId index (not a raw address).
            // Loads through it are rewritten to Load(local) before codegen.
            AmirRvalue::RelativeBorrow { local, .. } => self
                .builder
                .ins()
                .iconst(self.ptr_type, local.as_usize() as i64),
            AmirRvalue::Len(op) => self.translate_len(op, expected_ty),
            AmirRvalue::Alloc(op) => self.translate_alloc(op),
            // A3.0/A3.3: ready coroutine state = payload at +0 (stack or heap).
            AmirRvalue::CoroutineReady {
                value,
                payload_ty,
                stack,
            } => self.translate_coroutine_ready(value, *payload_ty, *stack),
            AmirRvalue::GenInsert {
                value, payload_ty, ..
            } => self.translate_gen_insert(value, *payload_ty),
            AmirRvalue::GenGet {
                gen_ref,
                payload_ty,
                ..
            } => self.translate_gen_read("ar_gen_get_raw", gen_ref, *payload_ty),
            AmirRvalue::GenSet {
                gen_ref,
                value,
                payload_ty,
                ..
            } => self.translate_gen_write("ar_gen_set_raw", gen_ref, value, *payload_ty, false),
            AmirRvalue::GenUpsert {
                gen_ref,
                value,
                payload_ty,
                ..
            } => self.translate_gen_write("ar_gen_upsert_raw", gen_ref, value, *payload_ty, true),
            AmirRvalue::GenRemove {
                gen_ref,
                payload_ty,
                ..
            } => self.translate_gen_read("ar_gen_remove_raw", gen_ref, *payload_ty),
            AmirRvalue::ToStr { .. } | AmirRvalue::StringInterp { .. } => {
                // Fat-pointer results must go through translate_str_rvalue.
                self.record_ice(
                    "ToStr/StringInterp must be lowered via str rvalue path",
                    self.func_span(),
                );
                self.poison_i32()
            }
        }
    }

    fn gen_payload_slot(
        &mut self,
        payload_ty: arandu_semantics::types::TypeId,
    ) -> Option<(cranelift_codegen::ir::StackSlot, Type, u64, u64, bool)> {
        let ar_ty = self.type_info.resolve_type_id(payload_ty);
        let has_destructor = self
            .type_info
            .destructor_instances
            .contains_key(&payload_ty);
        if !self.type_info.is_copy(payload_ty) && !has_destructor {
            self.record_ice(
                "non-Copy GenRef payload has no explicit @Destructor contract",
                self.func_span(),
            );
            return None;
        }
        let layout = self.checked_layout(&ar_ty);
        let clif_ty = match crate::types::clif_type(&ar_ty, self.ptr_type) {
            crate::types::ClifType::Concrete(ty) => ty,
            crate::types::ClifType::Void => {
                self.record_ice("void GenRef payload", self.func_span());
                return None;
            }
        };
        let Ok(size) = u32::try_from(layout.size.max(1)) else {
            self.record_ice("GenRef payload exceeds stack-slot limit", self.func_span());
            return None;
        };
        let slot = self
            .builder
            .create_sized_stack_slot(cranelift_codegen::ir::StackSlotData {
                kind: cranelift_codegen::ir::StackSlotKind::ExplicitSlot,
                size,
                align_shift: layout.align.max(1).trailing_zeros() as u8,
                key: None,
            });
        let indirect = matches!(
            ar_ty,
            ArType::Named(_, _)
                | ArType::Array(_, _)
                | ArType::Tuple(_)
                | ArType::Option(_)
                | ArType::Result(_, _)
                | ArType::Coroutine(_)
                | ArType::Poll(_)
                | ArType::Range(_)
        );
        Some((slot, clif_ty, layout.size, layout.align, indirect))
    }

    fn gen_layout_args(
        &mut self,
        payload_ty: arandu_semantics::types::TypeId,
        size: u64,
        align: u64,
    ) -> (Value, Value, Value) {
        let drop_glue = match self.type_info.resolve_type_id(payload_ty) {
            ArType::Named(_, _) => self
                .type_info
                .destructor_instances
                .get(&payload_ty)
                .and_then(|destructor| {
                    let name =
                        format!("__ar_drop_{}_{}", destructor.file_id, destructor.local_id.0);
                    self.func_ids.get(&name).copied()
                })
                .map(|id| {
                    let function = self.module.declare_func_in_func(id, self.builder.func);
                    self.builder.ins().func_addr(self.ptr_type, function)
                })
                .unwrap_or_else(|| self.builder.ins().iconst(self.ptr_type, 0)),
            _ => self.builder.ins().iconst(self.ptr_type, 0),
        };
        (
            self.builder.ins().iconst(self.ptr_type, size as i64),
            self.builder.ins().iconst(self.ptr_type, align as i64),
            drop_glue,
        )
    }

    fn translate_gen_insert(
        &mut self,
        value: &AmirOperand,
        payload_ty: arandu_semantics::types::TypeId,
    ) -> Value {
        let Some((slot, clif_ty, size, align, indirect)) = self.gen_payload_slot(payload_ty) else {
            return self.poison_i32();
        };
        let payload = self.translate_operand(value, Some(clif_ty));
        let address = if indirect {
            payload
        } else {
            self.builder
                .ins()
                .stack_store(self.ptr_type, payload, slot, 0);
            self.builder.ins().stack_addr(self.ptr_type, slot, 0)
        };
        let (size, align, drop_glue) = self.gen_layout_args(payload_ty, size, align);
        let Some(&id) = self.func_ids.get("ar_gen_insert_raw") else {
            self.record_ice("missing ar_gen_insert_raw runtime import", self.func_span());
            return self.poison_i32();
        };
        let function = self.module.declare_func_in_func(id, self.builder.func);
        let call = self
            .builder
            .ins()
            .call(function, &[address, size, align, drop_glue]);
        self.builder.inst_results(call)[0]
    }

    fn translate_gen_read(
        &mut self,
        name: &str,
        gen_ref: &AmirOperand,
        payload_ty: arandu_semantics::types::TypeId,
    ) -> Value {
        use cranelift_codegen::ir::{TrapCode, types::I64};
        let Some((slot, clif_ty, size, align, indirect)) = self.gen_payload_slot(payload_ty) else {
            return self.poison_i32();
        };
        let handle = self.translate_operand(gen_ref, Some(I64));
        let address = self.builder.ins().stack_addr(self.ptr_type, slot, 0);
        let (size, align, _) = self.gen_layout_args(payload_ty, size, align);
        let Some(&id) = self.func_ids.get(name) else {
            self.record_ice(format!("missing {name} runtime import"), self.func_span());
            return self.poison_i32();
        };
        let function = self.module.declare_func_in_func(id, self.builder.func);
        let call = self
            .builder
            .ins()
            .call(function, &[handle, address, size, align]);
        let succeeded = self.builder.inst_results(call)[0];
        self.builder
            .ins()
            .trapz(succeeded, TrapCode::unwrap_user(2));
        if indirect {
            address
        } else {
            self.builder
                .ins()
                .stack_load(self.ptr_type, clif_ty, slot, 0)
        }
    }

    fn translate_gen_write(
        &mut self,
        name: &str,
        gen_ref: &AmirOperand,
        value: &AmirOperand,
        payload_ty: arandu_semantics::types::TypeId,
        returns_handle: bool,
    ) -> Value {
        use cranelift_codegen::ir::{TrapCode, types::I64};
        let Some((slot, clif_ty, size, align, indirect)) = self.gen_payload_slot(payload_ty) else {
            return self.poison_i32();
        };
        let handle = self.translate_operand(gen_ref, Some(I64));
        let payload = self.translate_operand(value, Some(clif_ty));
        let address = if indirect {
            payload
        } else {
            self.builder
                .ins()
                .stack_store(self.ptr_type, payload, slot, 0);
            self.builder.ins().stack_addr(self.ptr_type, slot, 0)
        };
        let (size, align, drop_glue) = self.gen_layout_args(payload_ty, size, align);
        let Some(&id) = self.func_ids.get(name) else {
            self.record_ice(format!("missing {name} runtime import"), self.func_span());
            return self.poison_i32();
        };
        let function = self.module.declare_func_in_func(id, self.builder.func);
        let call = self
            .builder
            .ins()
            .call(function, &[handle, address, size, align, drop_glue]);
        let result = self.builder.inst_results(call)[0];
        self.builder.ins().trapz(result, TrapCode::unwrap_user(2));
        if returns_handle { result } else { handle }
    }

    /// `Len` for array (constant), `str` fat-pointer (SSA pair), slice (memory fat ptr).
    fn translate_len(&mut self, op: &AmirOperand, expected_ty: Option<Type>) -> Value {
        let op_ty = self.get_operand_ar_type(op);
        let i64_ty = cranelift_codegen::ir::types::I64;
        let result_ty = expected_ty.unwrap_or(self.ptr_type);

        match op_ty {
            ArType::Array(len, _) => {
                let v = self.builder.ins().iconst(i64_ty, len as i64);
                self.cast_int_width(v, result_ty)
            }
            ArType::Primitive(Primitive::Str) => {
                // Dual-value Str ABI: reuse the str operand path for temps + literals.
                let (_, len_val) = self.translate_str_operand(op);
                self.cast_int_width(len_val, result_ty)
            }
            ArType::Slice(_) => {
                // Slice fat pointer in memory: {ptr @0, len @pointer_width}.
                let base = self.translate_operand(op, Some(self.ptr_type));
                let len_off = self.ptr_type.bytes() as i32;
                let len_val = self.builder.ins().load(
                    i64_ty,
                    cranelift_codegen::ir::MemFlagsData::new(),
                    base,
                    len_off,
                );
                self.cast_int_width(len_val, result_ty)
            }
            _ => {
                self.record_ice(
                    format!("Len not supported for type {op_ty:?}"),
                    self.func_span(),
                );
                self.poison_i32()
            }
        }
    }

    /// Byte-count heap allocation via `malloc` (RC-RVALUE-GAPS).
    fn translate_alloc(&mut self, op: &AmirOperand) -> Value {
        let size_val = self.translate_operand(op, Some(self.ptr_type));
        let Some(malloc_id) = self.malloc_func_id() else {
            return self.poison_i32();
        };
        let malloc_ref = self
            .module
            .declare_func_in_func(malloc_id, self.builder.func);
        let call = self.builder.ins().call(malloc_ref, &[size_val]);
        self.builder.inst_results(call)[0]
    }

    fn cast_int_width(&mut self, val: Value, target: Type) -> Value {
        let src = self.builder.func.dfg.value_type(val);
        if src == target {
            return val;
        }
        if src.bits() < target.bits() {
            self.builder.ins().uextend(target, val)
        } else if src.bits() > target.bits() {
            self.builder.ins().ireduce(target, val)
        } else {
            val
        }
    }

    pub(super) fn translate_unary_op(&mut self, op: UnaryOp, val: Value) -> Value {
        let ty = self.builder.func.dfg.value_type(val);
        let is_float = ty.is_float();

        match op {
            UnaryOp::Neg => {
                if is_float {
                    self.builder.ins().fneg(val)
                } else {
                    self.builder.ins().ineg(val)
                }
            }
            UnaryOp::Not => {
                let zero = self.builder.ins().iconst(ty, 0);
                self.builder
                    .ins()
                    .icmp(cranelift_codegen::ir::condcodes::IntCC::Equal, val, zero)
            }
            UnaryOp::BitNot => self.builder.ins().bnot(val),
            UnaryOp::Await => {
                // Handled in translate_rvalue (needs expected_ty for load width).
                self.record_ice(
                    "Unary Await must be lowered via rvalue path with expected_ty",
                    self.func_span(),
                );
                self.poison_i32()
            }
            // F2.0: Ref/RefMut are lowered as Borrow rvalues, not Unary.
            // Deref of a pointer-valued SSA: load pointee as expected return type.
            UnaryOp::Ref | UnaryOp::RefMut => {
                self.record_ice(
                    "Unary Ref/RefMut should lower as Borrow, not Unary",
                    self.func_span(),
                );
                self.poison_i32()
            }
            UnaryOp::Deref => {
                // `val` is a pointer; load a machine word (int-sized) by default.
                let load_ty = if ty.is_int() || ty.is_float() {
                    ty
                } else {
                    self.ptr_type
                };
                // When the value is already a pointer type, load through it.
                let ptr = val;
                self.builder
                    .ins()
                    .load(load_ty, cranelift_codegen::ir::MemFlagsData::new(), ptr, 0)
            }
            // `UnaryOp` is `#[non_exhaustive]` across crate boundaries.
            _ => {
                self.record_error(
                    arandu_semantics::DiagCode::U001FeatureNotSupported,
                    "unsupported unary operator in Cranelift backend",
                    self.func_span(),
                );
                self.poison_i32()
            }
        }
    }
}
