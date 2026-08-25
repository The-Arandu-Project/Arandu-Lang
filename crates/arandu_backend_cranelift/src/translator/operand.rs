use arandu_semantics::amir::{AmirConstant, AmirOperand};
use cranelift_codegen::ir::{InstBuilder, Type, Value};
use cranelift_module::Module;

use super::FunctionTranslator;

impl<M: cranelift_module::Module> FunctionTranslator<'_, '_, M> {
    pub(super) fn translate_str_operand(&mut self, operand: &AmirOperand) -> (Value, Value) {
        if self.error.is_some() {
            return (self.poison_i32(), self.poison_i32());
        }

        match operand {
            AmirOperand::Copy(temp_id) | AmirOperand::Move(temp_id) => {
                if let Some(&(var_ptr, var_len)) = self.str_temp_map.get(temp_id) {
                    let ptr_val = self.builder.use_var(var_ptr);
                    let len_val = self.builder.use_var(var_len);
                    (ptr_val, len_val)
                } else if let Some(&var) = self.temp_map.get(temp_id) {
                    let ptr_val = self.builder.use_var(var);
                    let len_val = self.builder.ins().iconst(self.ptr_type, 0);
                    (ptr_val, len_val)
                } else {
                    self.record_ice(
                        "use of undeclared AMIR temp in codegen",
                        self.temp_span(*temp_id),
                    );
                    (self.poison_i32(), self.poison_i32())
                }
            }
            AmirOperand::Constant(AmirConstant::Nil) => {
                // Empty string / null fat pointer (used when zeroing the Ok binding
                // on a Result.Err path of `let ok, err = …`).
                let ptr_val = self.builder.ins().iconst(self.ptr_type, 0);
                let len_val = self.builder.ins().iconst(self.ptr_type, 0);
                (ptr_val, len_val)
            }
            AmirOperand::Constant(AmirConstant::Pool(lit_id)) => {
                let entry = self.literal_pool.get(*lit_id);
                if let arandu_semantics::literal_pool::AmirLiteralEntry::Str(s) = entry {
                    let str_bytes = s.as_bytes();
                    let data_id = match self.module.declare_data(
                        &format!("str_lit_{}", lit_id.0),
                        cranelift_module::Linkage::Local,
                        false,
                        false,
                    ) {
                        Ok(data_id) => data_id,
                        Err(err) => {
                            self.record_ice(
                                format!("failed to declare string literal in JIT module: {err:?}"),
                                self.func_span(),
                            );
                            return (self.poison_i32(), self.poison_i32());
                        }
                    };
                    let mut data_ctx = cranelift_module::DataDescription::new();
                    data_ctx.define(str_bytes.to_vec().into_boxed_slice());
                    let _ = self.module.define_data(data_id, &data_ctx);
                    let local_data_ref =
                        self.module.declare_data_in_func(data_id, self.builder.func);
                    let ptr_val = self
                        .builder
                        .ins()
                        .symbol_value(self.ptr_type, local_data_ref);
                    let len_val = self.builder.ins().iconst(self.ptr_type, s.len() as i64);
                    (ptr_val, len_val)
                } else {
                    self.record_ice("expected string literal in pool", self.func_span());
                    (self.poison_i32(), self.poison_i32())
                }
            }
            _ => {
                self.record_ice(
                    "unsupported operand for translate_str_operand",
                    self.func_span(),
                );
                (self.poison_i32(), self.poison_i32())
            }
        }
    }

    pub(super) fn translate_operand(
        &mut self,
        operand: &AmirOperand,
        expected_ty: Option<Type>,
    ) -> Value {
        if self.error.is_some() {
            return self.poison_i32();
        }

        let mut val = match operand {
            AmirOperand::Copy(temp_id) | AmirOperand::Move(temp_id) => {
                match self.temp_map.get(temp_id) {
                    Some(var) => self.builder.use_var(*var),
                    None => {
                        // ZST temps (void / typeck error) have no Cranelift vars.
                        // `Err` is pointer-sized and must be declared like other scalars.
                        let ty = self.temp_ar_ty(*temp_id);
                        if matches!(
                            ty,
                            arandu_semantics::types::ArType::Void
                                | arandu_semantics::types::ArType::Error
                        ) {
                            return self.poison_i32();
                        }
                        self.record_ice(
                            "use of undeclared AMIR temp in codegen",
                            self.temp_span(*temp_id),
                        );
                        return self.poison_i32();
                    }
                }
            }
            AmirOperand::Constant(c) => match c {
                AmirConstant::Bool(b) => {
                    let imm = if *b { 1 } else { 0 };
                    self.builder
                        .ins()
                        .iconst(cranelift_codegen::ir::types::I8, imm)
                }
                AmirConstant::Nil => {
                    // Prefer the expected ABI type so `int?`/`Point?`/`Err?` compares
                    // against a zero of the same width as the left-hand side.
                    let ty = expected_ty.unwrap_or(cranelift_codegen::ir::types::I32);
                    self.builder.ins().iconst(ty, 0)
                }
                AmirConstant::Pool(lit_id) => {
                    let entry = self.literal_pool.get(*lit_id);
                    match entry {
                        arandu_semantics::literal_pool::AmirLiteralEntry::Int(s) => {
                            let val = match arandu_semantics::literal_pool::parse_int_literal(s) {
                                Some(v) => v as i64,
                                None => {
                                    self.record_ice(
                                        format!(
                                            "invalid integer literal in AMIR literal pool: '{s}'"
                                        ),
                                        self.func_span(),
                                    );
                                    return self.poison_i32();
                                }
                            };
                            let ty = expected_ty.unwrap_or(cranelift_codegen::ir::types::I32);
                            self.builder.ins().iconst(ty, val)
                        }
                        arandu_semantics::literal_pool::AmirLiteralEntry::Float(s) => {
                            let val = match arandu_semantics::literal_pool::parse_float_literal(s) {
                                Some(v) => v,
                                None => {
                                    self.record_ice(
                                        format!(
                                            "invalid float literal in AMIR literal pool: '{s}'"
                                        ),
                                        self.func_span(),
                                    );
                                    return self.poison_i32();
                                }
                            };
                            self.builder.ins().f64const(val)
                        }
                        arandu_semantics::literal_pool::AmirLiteralEntry::Str(s) => {
                            let str_bytes = s.as_bytes();
                            let data_id = match self.module.declare_data(
                                &format!("str_lit_{}", lit_id.0),
                                cranelift_module::Linkage::Local,
                                false,
                                false,
                            ) {
                                Ok(data_id) => data_id,
                                Err(err) => {
                                    self.record_ice(
                                        format!(
                                            "failed to declare string literal in JIT module: {err:?}"
                                        ),
                                        self.func_span(),
                                    );
                                    return self.poison_i32();
                                }
                            };
                            let mut data_ctx = cranelift_module::DataDescription::new();
                            data_ctx.define(str_bytes.to_vec().into_boxed_slice());
                            let _ = self.module.define_data(data_id, &data_ctx);
                            let local_data_ref =
                                self.module.declare_data_in_func(data_id, self.builder.func);
                            self.builder
                                .ins()
                                .symbol_value(self.ptr_type, local_data_ref)
                        }
                        arandu_semantics::literal_pool::AmirLiteralEntry::Char(s) => {
                            let val = s.chars().next().unwrap_or('\0') as i64;
                            self.builder
                                .ins()
                                .iconst(cranelift_codegen::ir::types::I32, val)
                        }
                    }
                }
            },
            AmirOperand::FunctionRef(sym_id) => {
                let sym = self.symbol_table.get(*sym_id);
                let func_id = match self.func_ids.get(sym.name.as_str()) {
                    Some(func_id) => *func_id,
                    None => {
                        self.record_ice(
                            format!("function '{}' was not declared in the JIT module", sym.name),
                            sym.span,
                        );
                        return self.poison_i32();
                    }
                };
                let local_ref = self.module.declare_func_in_func(func_id, self.builder.func);
                self.builder.ins().func_addr(self.ptr_type, local_ref)
            }
            AmirOperand::GlobalRef(_) => {
                self.record_ice(
                    "GlobalRef as operand should not appear after BC.2.2 zero-payload tuple fix.",
                    self.func_span(),
                );
                self.poison_i32()
            }
        };

        if let Some(target_ty) = expected_ty {
            let val_ty = self.builder.func.dfg.value_type(val);
            if val_ty != target_ty && val_ty.is_int() && target_ty.is_int() {
                if val_ty.bits() < target_ty.bits() {
                    val = self.builder.ins().sextend(target_ty, val);
                } else if val_ty.bits() > target_ty.bits() {
                    val = self.builder.ins().ireduce(target_ty, val);
                }
            }
        }

        val
    }
}
