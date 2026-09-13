//! Enum construction, discriminant inspection, and payload extraction.

use arandu_semantics::amir::AmirOperand;
use arandu_semantics::passes::type_checker::types::{ArType, Primitive};
use cranelift_codegen::ir::{InstBuilder, Type, Value};

use super::super::FunctionTranslator;

impl<M: cranelift_module::Module> FunctionTranslator<'_, '_, M> {
    pub(super) fn translate_enum_construct(
        &mut self,
        variant_tag: usize,
        payload: Option<&AmirOperand>,
        expected_ar_type: Option<&ArType>,
    ) -> Value {
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

        let tag_val = self.builder.ins().iconst(self.ptr_type, variant_tag as i64);
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
                    .and_then(|variants| variants.get(variant_tag).cloned())
                    .and_then(|shape| shape.payload_ty)
                }
                ArType::Result(ok, err) => match variant_tag {
                    0 => Some(*ok),
                    1 => Some(*err),
                    _ => None,
                },
                // Option.Some = 1; Poll.Ready = 0.
                ArType::Option(inner) if variant_tag == 1 => Some(*inner),
                ArType::Poll(inner) if variant_tag == 0 => Some(*inner),
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
                        format!("missing concrete payload type for enum variant tag {variant_tag}"),
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

    pub(super) fn translate_discriminant(&mut self, value: &AmirOperand) -> Value {
        let ptr_val = self.translate_operand(value, Some(self.ptr_type));
        self.builder.ins().load(
            self.ptr_type,
            cranelift_codegen::ir::MemFlagsData::new(),
            ptr_val,
            0,
        )
    }

    pub(super) fn translate_enum_payload(
        &mut self,
        value: &AmirOperand,
        variant: &arandu_semantics::SymbolId,
        index: usize,
        expected_ty: Option<Type>,
    ) -> Value {
        let ptr_val = self.translate_operand(value, Some(self.ptr_type));
        let pointer_width = self.ptr_type.bytes() as u64;

        let base_ty = match value {
            AmirOperand::Copy(temp_id) | AmirOperand::Move(temp_id) => self.temp_ar_ty(*temp_id),
            _ => arandu_semantics::types::ArType::Error,
        };
        let enum_ty = match base_ty {
            arandu_semantics::types::ArType::Ptr(inner) => self.type_info.resolve_type_id(inner),
            other => other,
        };
        let enum_id = match enum_ty {
            ArType::Named(enum_id, _) => enum_id,
            _ => arandu_semantics::SymbolId::DUMMY,
        };

        let mut payload_offset = 0;
        if let Some(variants) = arandu_semantics::layout::StructLayoutProvider::get_enum_variants(
            self.type_info,
            enum_id,
        ) {
            let tag = self
                .type_info
                .enum_variant_tags
                .get(variant)
                .copied()
                .unwrap_or(0);
            if let Some(variant_shape) = variants.get(tag)
                && let Some(payload_ty_id) = variant_shape.payload_ty
            {
                let payload_ty = self.type_info.resolve_type_id(payload_ty_id);
                let payload_layout = self.checked_layout(&payload_ty);
                if index < payload_layout.field_offsets.len() {
                    payload_offset = payload_layout.field_offsets[index] as i32;
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
}
