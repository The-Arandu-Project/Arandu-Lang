use super::{CEmitter, sanitize_c_ident};
use arandu_middle::amir::{AmirConstant, AmirFunc, AmirOperand, AmirPlace, AmirProjection};
use arandu_middle::literal_pool::AmirLiteralEntry;
use arandu_middle::types::{ArType, Primitive};

impl<'a> CEmitter<'a> {
    pub(super) fn format_operand_str(&self, op: &AmirOperand) -> String {
        match op {
            AmirOperand::Copy(t) | AmirOperand::Move(t) => format!("t{}", t.as_usize()),
            AmirOperand::FunctionRef(id) | AmirOperand::GlobalRef(id) => {
                sanitize_c_ident(&self.symbols.get(*id).name)
            }
            AmirOperand::Constant(c) => match c {
                AmirConstant::Pool(id) => match self.program.literal_pool.get(*id) {
                    AmirLiteralEntry::Int(v) => {
                        arandu_middle::literal_pool::int_literal_c_source(v)
                            .unwrap_or_else(|| v.to_string())
                    }
                    AmirLiteralEntry::Float(v) => {
                        arandu_middle::literal_pool::float_literal_c_source(v)
                            .unwrap_or_else(|| v.to_string())
                    }
                    AmirLiteralEntry::Str(_) => {
                        // Prefer named constant when available; compound literal fallback
                        // is handled in format_operand for pool constants.
                        "((ArStr){ .ptr = (const uint8_t*)\"\", .len = 0 })".to_string()
                    }
                    AmirLiteralEntry::Char(v) => {
                        let scalar = v.chars().next().unwrap_or('\0') as u32;
                        format!("UINT32_C({scalar})")
                    }
                },
                AmirConstant::Bool(b) => {
                    if *b {
                        "true".to_string()
                    } else {
                        "false".to_string()
                    }
                }
                AmirConstant::Nil => "NULL".to_string(),
            },
        }
    }

    pub(super) fn format_operand(&self, op: &AmirOperand, _func: &AmirFunc) -> String {
        // Delegates to `format_operand_str` for most operands. Pool string literals are a
        // special case: they must be emitted as an `ArStr` fat-pointer (ptr + len) rather
        // than a raw pointer, using a compound-literal array cast.
        match op {
            AmirOperand::Constant(AmirConstant::Pool(id)) => {
                match self.program.literal_pool.get(*id) {
                    AmirLiteralEntry::Str(s) => {
                        // LayoutEngine Str: named constant or compound literal {ptr, len}.
                        let _ = s;
                        format!("AR_STR_{}", id.0)
                    }
                    _ => self.format_operand_str(op),
                }
            }
            _ => self.format_operand_str(op),
        }
    }

    pub(super) fn format_type(&self, ty: &ArType) -> String {
        match ty {
            ArType::Primitive(Primitive::I8) => "int8_t".to_string(),
            ArType::Primitive(Primitive::I16) => "int16_t".to_string(),
            ArType::Primitive(Primitive::I32) => "int32_t".to_string(),
            ArType::Primitive(Primitive::I64) => "int64_t".to_string(),
            ArType::Primitive(Primitive::U8) | ArType::Primitive(Primitive::Byte) => {
                "uint8_t".to_string()
            }
            ArType::Primitive(Primitive::U16) => "uint16_t".to_string(),
            ArType::Primitive(Primitive::U32) => "uint32_t".to_string(),
            ArType::Primitive(Primitive::U64) => "uint64_t".to_string(),
            ArType::Primitive(Primitive::F32) => "float".to_string(),
            ArType::Primitive(Primitive::F64) => "double".to_string(),
            ArType::Primitive(Primitive::Uint) => {
                if self.layout.pointer_width() == 8 {
                    "uint64_t".to_string()
                } else {
                    "uint32_t".to_string()
                }
            }
            ArType::IntLiteral => {
                if self.layout.pointer_width() == 8 {
                    "int64_t".to_string()
                } else {
                    "int32_t".to_string()
                }
            }
            ArType::Primitive(Primitive::Int) => {
                if self.layout.pointer_width() == 8 {
                    "int64_t".to_string()
                } else {
                    "int32_t".to_string()
                }
            }
            ArType::Primitive(Primitive::Bool) => "bool".to_string(),
            ArType::Primitive(Primitive::Char) => "uint32_t".to_string(),
            ArType::Primitive(Primitive::Str) => "ArStr".to_string(),
            ArType::Primitive(Primitive::Float) | ArType::FloatLiteral => "double".to_string(),
            ArType::Void => "void".to_string(),
            ArType::Ptr(inner) | ArType::Ref(inner) | ArType::RefMut(inner) => {
                format!("{}*", self.format_type(&self.interner.resolve(*inner)))
            }
            ArType::GenRef => "int64_t".to_string(),
            ArType::Named(id, _) => sanitize_c_ident(&self.symbols.get(*id).name),
            ArType::Slice(inner) => {
                let inner_name = self.format_type(&self.interner.resolve(*inner));
                format!("ArType_Slice_{}", sanitize_c_ident(&inner_name))
            }
            ArType::Array(len, inner) => {
                let inner_name = self.format_type(&self.interner.resolve(*inner));
                format!("ArType_Array_{}_{}", len, sanitize_c_ident(&inner_name))
            }
            ArType::Nullable(inner) => {
                let inner_name = self.format_type(&self.interner.resolve(*inner));
                format!("ArType_Nullable_{}", sanitize_c_ident(&inner_name))
            }
            ArType::Option(inner) => {
                let inner_name = self.format_type(&self.interner.resolve(*inner));
                format!("ArType_Option_{}", sanitize_c_ident(&inner_name))
            }
            ArType::Result(ok, err) => {
                let ok_name = self.format_type(&self.interner.resolve(*ok));
                let err_name = self.format_type(&self.interner.resolve(*err));
                format!(
                    "ArType_Result_{}_{}",
                    sanitize_c_ident(&ok_name),
                    sanitize_c_ident(&err_name)
                )
            }
            ArType::Tuple(tys) => {
                let mut name = "ArType_Tuple".to_string();
                for &t in tys {
                    name.push('_');
                    name.push_str(&self.format_type(&self.interner.resolve(t)));
                }
                sanitize_c_ident(&name)
            }
            ArType::Func(params, ret) => {
                let mut name = "ArFunc".to_string();
                for &p in params {
                    name.push('_');
                    name.push_str(&self.format_type(&self.interner.resolve(p)));
                }
                name.push_str("_to_");
                name.push_str(&self.format_type(&self.interner.resolve(*ret)));
                sanitize_c_ident(&name)
            }
            _ => format!("ArType_{}", sanitize_c_ident(&format!("{:?}", ty))),
        }
    }

    pub(super) fn format_place(&mut self, place: &AmirPlace, func: &AmirFunc) -> String {
        let local_idx = place.local.as_usize();
        let mut current_ty = self.local_ty(func, place.local);
        let mut path = format!("l{}", local_idx);

        for proj in &place.projections {
            match proj {
                // BC.4a: place through pointer value — lvalue is `*path`.
                AmirProjection::Deref => {
                    current_ty = match &current_ty {
                        ArType::Ptr(inner)
                        | ArType::Ref(inner)
                        | ArType::RefMut(inner)
                        | ArType::Nullable(inner) => self.interner.resolve(*inner),
                        other => other.clone(),
                    };
                    path = format!("(*{})", path);
                }
                AmirProjection::Field(field_symbol_id) => {
                    // After Deref, current_ty is the pointee; unwrap residual ptr-likes.
                    let struct_ty = match &current_ty {
                        ArType::Ptr(inner)
                        | ArType::Ref(inner)
                        | ArType::RefMut(inner)
                        | ArType::Nullable(inner) => self.interner.resolve(*inner),
                        other => other.clone(),
                    };
                    let struct_id = match &struct_ty {
                        ArType::Named(id, _) => *id,
                        _ => arandu_middle::SymbolId::DUMMY,
                    };
                    let layout = self.checked_layout(&struct_ty);
                    let field_name = self
                        .symbols
                        .get(*field_symbol_id)
                        .name
                        .rsplit('.')
                        .next()
                        .unwrap_or("");
                    let field_idx = match self.provider.get_struct_field_indices(struct_id) {
                        Some(indices) => indices.get(field_name).copied().unwrap_or(0),
                        None => 0,
                    };
                    let offset = layout.field_offsets.get(field_idx).copied().unwrap_or(0);

                    let field_ty = self.instantiated_field_ty(&struct_ty, field_name);
                    let field_c_ty = self.format_type(&field_ty);
                    // If path is already a pointer (heap/ptr local without Deref), GEP from
                    // the pointer value; otherwise take address of the stack lvalue.
                    if matches!(
                        current_ty,
                        ArType::Ptr(_) | ArType::Ref(_) | ArType::RefMut(_) | ArType::Nullable(_)
                    ) {
                        path = format!("*({}*)((uint8_t*){} + {})", field_c_ty, path, offset);
                    } else {
                        path = format!("*({}*)((uint8_t*)&{} + {})", field_c_ty, path, offset);
                    }
                    current_ty = field_ty;
                }
                AmirProjection::Index(index_op) => {
                    let elem_ty = match &current_ty {
                        ArType::Array(_, inner)
                        | ArType::Slice(inner)
                        | ArType::Ptr(inner)
                        | ArType::Ref(inner)
                        | ArType::RefMut(inner) => self.interner.resolve(*inner),
                        _ => ArType::Error,
                    };
                    let elem_c_ty = self.format_type(&elem_ty);
                    let index_str = self.format_operand(index_op, func);

                    if matches!(
                        current_ty,
                        ArType::Ptr(_) | ArType::Ref(_) | ArType::RefMut(_)
                    ) {
                        path = format!("(({}*){})[{}]", elem_c_ty, path, index_str);
                    } else if matches!(current_ty, ArType::Slice(_)) {
                        path = format!(
                            "(( {}* )(*(void**)((uint8_t*)&{} + 0)))[{}]",
                            elem_c_ty, path, index_str
                        );
                    } else {
                        path = format!("(({}*)&{})[{}]", elem_c_ty, path, index_str);
                    }
                    current_ty = elem_ty;
                }
            }
        }
        path
    }
}
