use cranelift_codegen::ir::{InstBuilder, Value};
use cranelift_module::FuncId;

use super::FunctionTranslator;

impl<M: cranelift_module::Module> FunctionTranslator<'_, '_, M> {
    pub(super) fn fmod_func_id(&mut self) -> Option<FuncId> {
        match self.func_ids.get("fmod") {
            Some(func_id) => Some(*func_id),
            None => {
                self.record_ice("fmod was not declared in the JIT module", self.func_span());
                None
            }
        }
    }

    pub(super) fn malloc_func_id(&mut self) -> Option<FuncId> {
        match self.func_ids.get("malloc") {
            Some(func_id) => Some(*func_id),
            None => {
                self.record_ice(
                    "malloc was not declared in the JIT module",
                    self.func_span(),
                );
                None
            }
        }
    }

    pub(super) fn free_func_id(&mut self) -> Option<FuncId> {
        match self.func_ids.get("free") {
            Some(func_id) => Some(*func_id),
            None => {
                self.record_ice("free was not declared in the JIT module", self.func_span());
                None
            }
        }
    }

    pub(super) fn memcpy_func_id(&mut self) -> Option<FuncId> {
        match self.func_ids.get("memcpy") {
            Some(func_id) => Some(*func_id),
            None => {
                self.record_ice(
                    "memcpy was not declared in the JIT module",
                    self.func_span(),
                );
                None
            }
        }
    }

    pub(super) fn memcmp_func_id(&mut self) -> Option<FuncId> {
        match self.func_ids.get("memcmp") {
            Some(func_id) => Some(*func_id),
            None => {
                self.record_ice(
                    "memcmp was not declared in the JIT module",
                    self.func_span(),
                );
                None
            }
        }
    }

    pub(super) fn emit_free_ptr(&mut self, ptr_val: Value) {
        let Some(free_func_id) = self.free_func_id() else {
            return;
        };
        let local_ref = self
            .module
            .declare_func_in_func(free_func_id, self.builder.func);

        #[cfg(debug_assertions)]
        {
            let poison_val = self.builder.ins().iconst(self.ptr_type, 0xDE_i64);
            self.builder.ins().store(
                cranelift_codegen::ir::MemFlagsData::new(),
                poison_val,
                ptr_val,
                0,
            );
        }

        self.builder.ins().call(local_ref, &[ptr_val]);
    }
}
