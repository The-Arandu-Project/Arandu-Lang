use arandu_semantics::amir::AmirStmt;
use arandu_semantics::passes::type_checker::types::{ArType, Primitive};
use cranelift_codegen::ir::InstBuilder;
use cranelift_module::Module;

use super::FunctionTranslator;
use crate::types::{ClifType, clif_type};

impl FunctionTranslator<'_, '_> {
    #[tracing::instrument(level = "trace", target = "arandu_backend_cranelift", skip(self))]
    pub(super) fn translate_stmt(&mut self, stmt: &AmirStmt) {
        if self.error.is_some() {
            return;
        }
        match stmt {
            AmirStmt::Assign { lhs, rhs } => {
                let lhs_ty = self.temp_ar_ty(*lhs);
                if matches!(&lhs_ty, ArType::Primitive(Primitive::Str)) {
                    let (ptr_val, len_val) = self.translate_str_rvalue(rhs);
                    if let Some(&(var_ptr, var_len)) = self.str_temp_map.get(lhs) {
                        self.builder.def_var(var_ptr, ptr_val);
                        self.builder.def_var(var_len, len_val);
                    }
                } else {
                    let expected_ty = self.get_temp_clif_type(*lhs);
                    let lhs_ar = self.temp_ar_ty(*lhs);
                    let expected_ar_type = Some(&lhs_ar);
                    let val = self.translate_rvalue(rhs, expected_ty, expected_ar_type);
                    if let Some(&var) = self.temp_map.get(lhs) {
                        self.builder.def_var(var, val);
                    }
                }
            }
            AmirStmt::Store { lhs, rhs } => {
                let lhs_ty = self.local_ar_ty(lhs.local);
                if matches!(&lhs_ty, ArType::Primitive(Primitive::Str)) {
                    let (ptr_val, len_val) = self.translate_str_operand(rhs);
                    if lhs.projections.is_empty() {
                        if let Some(&(var_ptr, var_len)) = self.str_local_map.get(&lhs.local) {
                            self.builder.def_var(var_ptr, ptr_val);
                            self.builder.def_var(var_len, len_val);
                        }
                    } else {
                        let (base_ptr, offset) = self.translate_place_address_for_load(lhs);
                        self.builder.ins().store(
                            cranelift_codegen::ir::MemFlagsData::new(),
                            ptr_val,
                            base_ptr,
                            offset,
                        );
                        self.builder.ins().store(
                            cranelift_codegen::ir::MemFlagsData::new(),
                            len_val,
                            base_ptr,
                            offset + self.ptr_type.bytes() as i32,
                        );
                    }
                } else {
                    let lhs_ar = self
                        .current_func
                        .locals
                        .get(lhs.local.as_usize())
                        .map(|l| self.resolve_ty(l.ty))
                        .unwrap_or(ArType::Error);
                    let expected_ty = match clif_type(&lhs_ar, self.ptr_type) {
                        ClifType::Concrete(ty) => Some(ty),
                        ClifType::Void => None,
                    };
                    // Route through translate_rvalue so `T?` stores box scalars
                    // (`int? = 0` must not store a null pointer handle).
                    let val = self.translate_rvalue(
                        &arandu_semantics::amir::AmirRvalue::Use(*rhs),
                        expected_ty,
                        Some(&lhs_ar),
                    );
                    self.translate_store_place(lhs, val);
                }
            }

            AmirStmt::Call { lhs, callee, args } => {
                self.translate_call(lhs, callee, args);
            }
            AmirStmt::Free(op) => {
                let ptr_val = self.translate_operand(op, Some(self.ptr_type));
                self.emit_free_ptr(ptr_val);
            }
            AmirStmt::StorageLive(_) | AmirStmt::StorageDead(_) => {}
            AmirStmt::Destroy(place) => {
                if place.projections.is_empty() {
                    let ty = self.local_ar_ty(place.local);
                    let ty_id = self.current_func.locals[place.local.as_usize()].ty;
                    if let ArType::Named(_, _) = ty
                        && let Some(destructor) = self.type_info.destructor_instances.get(&ty_id)
                        && let Some(&var) = self.local_map.get(&place.local)
                    {
                        let name = self.symbol_table.get(*destructor).name.as_str();
                        if let Some(&id) = self.func_ids.get(name) {
                            let ptr_val = self.builder.use_var(var);
                            let function = self.module.declare_func_in_func(id, self.builder.func);
                            self.builder.ins().call(function, &[ptr_val]);
                        } else {
                            self.record_ice(
                                format!("missing @Destructor function '{name}'"),
                                self.local_span(place.local),
                            );
                        }
                    }
                }
            }
            AmirStmt::Nop => {}
        }
    }
}
