use arandu_semantics::amir::{AmirOperand, TempId};
use arandu_semantics::passes::type_checker::types::{ArType, Primitive};
use cranelift_codegen::ir::{InstBuilder, Type};

use super::FunctionTranslator;

impl<M: cranelift_module::Module> FunctionTranslator<'_, '_, M> {
    pub(super) fn translate_call(
        &mut self,
        lhs: &Option<TempId>,
        callee: &AmirOperand,
        args: &[AmirOperand],
    ) {
        if let AmirOperand::FunctionRef(sym_id) = callee {
            let sym = self.symbol_table.get(*sym_id);
            let bare = sym.name.rsplit('.').next().unwrap_or(sym.name.as_str());
            // L6.1: mem intrinsics (short mono names + qualified std.core.mem.*).
            if self.translate_mem_intrinsic(bare, &sym.name, lhs, args) {
                return;
            }
        }

        let call_inst = match callee {
            AmirOperand::FunctionRef(sym_id) => {
                let sym = self.symbol_table.get(*sym_id);
                let func_id = match self.func_ids.get(sym.name.as_str()) {
                    Some(func_id) => *func_id,
                    None => {
                        self.record_ice(
                            format!("function '{}' was not declared in the JIT module", sym.name),
                            sym.span,
                        );
                        return;
                    }
                };
                let local_ref = self.module.declare_func_in_func(func_id, self.builder.func);

                let sig_id = self.builder.func.dfg.ext_funcs[local_ref].signature;
                let expected_tys: Vec<Type> = self.builder.func.dfg.signatures[sig_id]
                    .params
                    .iter()
                    .map(|param| param.value_type)
                    .collect();

                let mut clif_args = Vec::new();
                let mut clif_param_idx = 0;
                for arg in args {
                    let arg_ty = self.get_operand_ar_type(arg);
                    if matches!(arg_ty, ArType::Primitive(Primitive::Str)) {
                        let (ptr_val, len_val) = self.translate_str_operand(arg);
                        clif_args.push(ptr_val);
                        clif_args.push(len_val);
                        clif_param_idx += 2;
                    } else if matches!(arg_ty, ArType::Slice(_)) {
                        let (data, len) = self.translate_slice_operand(arg);
                        clif_args.push(data);
                        clif_args.push(len);
                        clif_param_idx += 2;
                    } else {
                        let expected = expected_tys.get(clif_param_idx).copied();
                        let val = self.translate_operand(arg, expected);
                        clif_args.push(val);
                        clif_param_idx += 1;
                    }
                }

                self.builder.ins().call(local_ref, &clif_args)
            }
            _ => {
                self.record_ice(
                    "indirect function calls are not implemented (and should have been rejected by the type checker)",
                    self.func_span(),
                );
                return;
            }
        };
        if let Some(lhs_temp) = lhs {
            let lhs_ty = self.temp_ar_ty(*lhs_temp);
            if matches!(&lhs_ty, ArType::Primitive(Primitive::Str)) {
                let results = self.builder.inst_results(call_inst);
                if results.len() >= 2 {
                    let res0 = results[0];
                    let res1 = results[1];
                    if let Some(&(var_ptr, var_len)) = self.str_temp_map.get(lhs_temp) {
                        self.builder.def_var(var_ptr, res0);
                        self.builder.def_var(var_len, res1);
                    }
                }
            } else if matches!(&lhs_ty, ArType::Slice(_)) {
                let results = self.builder.inst_results(call_inst);
                if results.len() >= 2 {
                    let descriptor = self.materialize_slice_descriptor(results[0], results[1]);
                    if let Some(&var) = self.temp_map.get(lhs_temp) {
                        self.builder.def_var(var, descriptor);
                    }
                }
            } else if let Some(&var) = self.temp_map.get(lhs_temp) {
                let results = self.builder.inst_results(call_inst);
                if !results.is_empty() {
                    let res0 = results[0];
                    self.builder.def_var(var, res0);
                }
            }
        }
    }

    /// Lower `ptrOffset` / `ptrRead` / `ptrWrite` / residual `sizeOf`/`alignOf`.
    ///
    /// Returns `true` when the call was handled as an intrinsic.
    fn translate_mem_intrinsic(
        &mut self,
        bare: &str,
        full_name: &str,
        lhs: &Option<TempId>,
        args: &[AmirOperand],
    ) -> bool {
        let kind = arandu_semantics::IntrinsicKind::from_name(bare)
            .or_else(|| arandu_semantics::IntrinsicKind::from_name(full_name));

        match kind {
            Some(arandu_semantics::IntrinsicKind::PtrRead) => {
                if args.is_empty() {
                    return true;
                }
                let ptr_val = self.translate_operand(&args[0], Some(self.ptr_type));
                let clif_ty = lhs
                    .and_then(|temp| self.get_temp_clif_type(temp))
                    .unwrap_or(self.ptr_type);
                let loaded_val = self.builder.ins().load(
                    clif_ty,
                    cranelift_codegen::ir::MemFlagsData::new(),
                    ptr_val,
                    0,
                );
                if let Some(lhs_temp) = lhs
                    && let Some(&var) = self.temp_map.get(lhs_temp)
                {
                    self.builder.def_var(var, loaded_val);
                }
                true
            }
            Some(arandu_semantics::IntrinsicKind::PtrWrite) => {
                if args.len() < 2 {
                    return true;
                }
                let ptr_val = self.translate_operand(&args[0], Some(self.ptr_type));
                let val_to_store = self.translate_operand(&args[1], None);
                self.builder.ins().store(
                    cranelift_codegen::ir::MemFlagsData::new(),
                    val_to_store,
                    ptr_val,
                    0,
                );
                true
            }
            Some(arandu_semantics::IntrinsicKind::PtrOffset) => {
                if args.len() < 2 {
                    return true;
                }
                let base = self.translate_operand(&args[0], Some(self.ptr_type));
                let idx = self.translate_operand(&args[1], Some(cranelift_codegen::ir::types::I32));
                // Element size from pointer pointee type of the base operand.
                let base_ty = self.get_operand_ar_type(&args[0]);
                let elem_ty = match &base_ty {
                    ArType::Ptr(inner) | ArType::Ref(inner) | ArType::RefMut(inner) => {
                        self.resolve_ty(*inner)
                    }
                    _ => ArType::Primitive(Primitive::Int),
                };
                let elem_layout = self.checked_layout(&elem_ty);
                let elem_size = elem_layout.size.max(1) as i64;
                let size_val = self.builder.ins().iconst(self.ptr_type, elem_size);
                // Widen idx to pointer width if necessary.
                let idx_ext = if self.ptr_type == cranelift_codegen::ir::types::I64 {
                    self.builder.ins().sextend(self.ptr_type, idx)
                } else {
                    idx
                };
                let byte_off = self.builder.ins().imul(idx_ext, size_val);
                let result = self.builder.ins().iadd(base, byte_off);
                if let Some(lhs_temp) = lhs
                    && let Some(&var) = self.temp_map.get(lhs_temp)
                {
                    self.builder.def_var(var, result);
                }
                true
            }
            Some(
                arandu_semantics::IntrinsicKind::SizeOf | arandu_semantics::IntrinsicKind::AlignOf,
            ) => {
                let is_size = kind == Some(arandu_semantics::IntrinsicKind::SizeOf);
                let ty = ArType::Primitive(Primitive::Int);
                let layout = self.checked_layout(&ty);
                let value = if is_size { layout.size } else { layout.align };
                let clif_ty = lhs
                    .and_then(|t| self.get_temp_clif_type(t))
                    .unwrap_or(self.ptr_type);
                let c = self.builder.ins().iconst(clif_ty, value as i64);
                if let Some(lhs_temp) = lhs
                    && let Some(&var) = self.temp_map.get(lhs_temp)
                {
                    self.builder.def_var(var, c);
                }
                true
            }
            _ => false,
        }
    }
}
