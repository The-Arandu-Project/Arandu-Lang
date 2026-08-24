//! Low-level Cranelift JIT compilation internals.
//!
//! [`AranduJit`] drives the Cranelift JIT module lifecycle: it declares all
//! functions, defines each one via [`FunctionTranslator`], finalizes the
//! in-memory compilation, and returns a [`CompiledModule`] ready for execution.

use crate::abi::build_signature;
use crate::translator::FunctionTranslator;
use arandu_base::span::Span;
use arandu_semantics::amir::AmirProgram;
use arandu_semantics::passes::type_checker::types::{ArType, Primitive};
use arandu_semantics::{DiagCode, Diagnostic, SymbolTable};
use cranelift_codegen::isa::OwnedTargetIsa;
use cranelift_codegen::settings::{self, Configurable};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{FuncId, Linkage, Module};
use rustc_hash::FxHashMap;
use std::sync::OnceLock;

fn codegen_ice(message: impl Into<String>) -> Diagnostic {
    Diagnostic::ice(DiagCode::ICEGEN001, message, Span::new(0, 0, 0))
}

/// Host ISA is process-global and immutable. Building it once avoids re-running
/// `cranelift_native` + flag setup on every `run` / compile (debug: tens of ms).
fn cached_host_isa() -> Result<OwnedTargetIsa, Diagnostic> {
    static ISA: OnceLock<Result<OwnedTargetIsa, String>> = OnceLock::new();
    match ISA.get_or_init(|| {
        let mut flag_builder = settings::builder();
        for (key, val) in [
            ("use_colocated_libcalls", "false"),
            ("is_pic", "false"),
            // Fastest compile for interactive JIT; release optimizers live elsewhere.
            ("opt_level", "none"),
        ] {
            if let Err(e) = flag_builder.set(key, val) {
                return Err(format!("failed to set Cranelift flag {key}={val}: {e}"));
            }
        }
        let isa_builder = match cranelift_native::builder() {
            Ok(b) => b,
            Err(e) => return Err(format!("Failed to create Cranelift isa builder: {e}")),
        };
        match isa_builder.finish(settings::Flags::new(flag_builder)) {
            Ok(isa) => Ok(isa),
            Err(e) => Err(format!("Failed to build Cranelift isa: {e}")),
        }
    }) {
        Ok(isa) => Ok(std::sync::Arc::clone(isa)),
        Err(msg) => Err(codegen_ice(msg.clone())),
    }
}

/// Stateful Cranelift JIT context.
///
/// Wraps a [`JITModule`] and orchestrates the full compilation of an
/// [`AmirProgram`]: function declaration, translation, and memory finalization.
/// Consumed by [`AranduJit::compile_program`] — create a fresh instance for
/// each compilation.
pub struct AranduJit {
    pub module: JITModule,
}

impl AranduJit {
    /// Creates a new [`AranduJit`] with default Cranelift settings.
    ///
    /// Reuses a process-cached host [`OwnedTargetIsa`] (Arc clone). Each call
    /// still builds a fresh [`JITModule`] — modules cannot be reset after finalize.
    pub fn try_new() -> Result<Self, Diagnostic> {
        let isa = cached_host_isa()?;
        let mut builder = JITBuilder::with_isa(isa, cranelift_module::default_libcall_names());
        // ToStr v0.1 host helpers (malloc-backed fat strings).
        builder.symbol(
            "ar_jit_i64_to_str",
            crate::to_str_runtime::ar_jit_i64_to_str as *const u8,
        );
        builder.symbol(
            "ar_jit_u64_to_str",
            crate::to_str_runtime::ar_jit_u64_to_str as *const u8,
        );
        builder.symbol(
            "ar_jit_f64_to_str",
            crate::to_str_runtime::ar_jit_f64_to_str as *const u8,
        );
        builder.symbol(
            "ar_jit_bool_to_str",
            crate::to_str_runtime::ar_jit_bool_to_str as *const u8,
        );
        builder.symbol(
            "ar_jit_char_to_str",
            crate::to_str_runtime::ar_jit_char_to_str as *const u8,
        );
        // Prelude `io.println` (fat-pointer ABI: ptr + i64 len).
        builder.symbol(
            "io.println",
            crate::to_str_runtime::ar_jit_println as *const u8,
        );
        // Prelude `err.new(str) -> Err` (message handle = non-null ptr; fat-pointer str arg).
        builder.symbol(
            "err.new",
            crate::to_str_runtime::ar_jit_err_new as *const u8,
        );
        builder.symbol(
            "ar_jit_err_to_str",
            crate::to_str_runtime::ar_jit_err_to_str as *const u8,
        );
        // G4 type-erased compiler-managed GenRef ABI.
        builder.symbol(
            "ar_gen_insert_raw",
            arandu_runtime::gen_runtime_gold::ar_gen_insert_raw as *const u8,
        );
        builder.symbol(
            "ar_gen_get_raw",
            arandu_runtime::gen_runtime_gold::ar_gen_get_raw as *const u8,
        );
        builder.symbol(
            "ar_gen_set_raw",
            arandu_runtime::gen_runtime_gold::ar_gen_set_raw as *const u8,
        );
        builder.symbol(
            "ar_gen_upsert_raw",
            arandu_runtime::gen_runtime_gold::ar_gen_upsert_raw as *const u8,
        );
        builder.symbol(
            "ar_gen_remove_raw",
            arandu_runtime::gen_runtime_gold::ar_gen_remove_raw as *const u8,
        );
        builder.symbol(
            "ar_gen_shutdown_raw",
            arandu_runtime::gen_runtime_gold::ar_gen_shutdown_raw as *const u8,
        );
        // A3.6: coroutine poll / block_on (i64 payload MVP).
        builder.symbol(
            "ar_co_block_on_i64",
            crate::poll_runtime::ar_co_block_on_i64 as *const u8,
        );
        builder.symbol(
            "ar_co_poll_i64",
            crate::poll_runtime::ar_co_poll_i64 as *const u8,
        );
        builder.symbol(
            "ar_co_pending_once_i64",
            crate::poll_runtime::ar_co_pending_once_i64 as *const u8,
        );
        builder.symbol(
            "ar_co_make_ready_i64",
            crate::poll_runtime::ar_co_make_ready_i64 as *const u8,
        );
        // SL_R.0 cooperative runtime + SL_S path helpers
        builder.symbol(
            "ar_rt_spawn_i64",
            crate::rt_runtime::ar_rt_spawn_i64 as *const u8,
        );
        builder.symbol(
            "ar_rt_join_i64",
            crate::rt_runtime::ar_rt_join_i64 as *const u8,
        );
        builder.symbol(
            "ar_rt_block_on_i64",
            crate::rt_runtime::ar_rt_block_on_i64 as *const u8,
        );
        builder.symbol(
            "ar_rt_cancel_i64",
            crate::rt_runtime::ar_rt_cancel_i64 as *const u8,
        );
        builder.symbol(
            "ar_path_is_absolute",
            crate::rt_runtime::ar_path_is_absolute as *const u8,
        );
        builder.symbol(
            "ar_path_is_empty",
            crate::rt_runtime::ar_path_is_empty as *const u8,
        );
        builder.symbol("ar_path_join", crate::rt_runtime::ar_path_join as *const u8);
        builder.symbol(
            "ar_path_file_name",
            crate::rt_runtime::ar_path_file_name as *const u8,
        );
        builder.symbol("ar_str_len", crate::rt_runtime::ar_str_len as *const u8);
        builder.symbol(
            "ar_str_concat",
            crate::rt_runtime::ar_str_concat as *const u8,
        );
        builder.symbol(
            "ar_str_starts_with",
            crate::rt_runtime::ar_str_starts_with as *const u8,
        );
        builder.symbol(
            "ar_str_ends_with",
            crate::rt_runtime::ar_str_ends_with as *const u8,
        );
        builder.symbol(
            "ar_str_split_last",
            crate::rt_runtime::ar_str_split_last as *const u8,
        );
        // Minimal 0.1 optional OS surface (process / time / env)
        builder.symbol(
            "ar_process_exit",
            crate::os_runtime::ar_process_exit as *const u8,
        );
        builder.symbol(
            "ar_time_monotonic_ns",
            crate::os_runtime::ar_time_monotonic_ns as *const u8,
        );
        builder.symbol(
            "ar_env_args_len",
            crate::os_runtime::ar_env_args_len as *const u8,
        );
        builder.symbol(
            "ar_env_var_is_set",
            crate::os_runtime::ar_env_var_is_set as *const u8,
        );
        // std.alloc.vec:
        // - Product path (pure-buffer): malloc / realloc / buf_free only.
        // - Handle API (new/push/get/…): unit-test / legacy GenArena-style table.
        builder.symbol(
            "ar_vec_malloc",
            crate::vec_runtime::ar_vec_malloc as *const u8,
        );
        builder.symbol(
            "ar_vec_buf_free",
            crate::vec_runtime::ar_vec_buf_free as *const u8,
        );
        builder.symbol(
            "ar_vec_realloc",
            crate::vec_runtime::ar_vec_realloc as *const u8,
        );
        builder.symbol("ar_vec_new", crate::vec_runtime::ar_vec_new as *const u8);
        builder.symbol("ar_vec_push", crate::vec_runtime::ar_vec_push as *const u8);
        builder.symbol("ar_vec_len", crate::vec_runtime::ar_vec_len as *const u8);
        builder.symbol("ar_vec_has", crate::vec_runtime::ar_vec_has as *const u8);
        builder.symbol("ar_vec_get", crate::vec_runtime::ar_vec_get as *const u8);
        builder.symbol("ar_vec_put", crate::vec_runtime::ar_vec_put as *const u8);
        builder.symbol("ar_vec_pop", crate::vec_runtime::ar_vec_pop as *const u8);
        builder.symbol(
            "ar_vec_clear",
            crate::vec_runtime::ar_vec_clear as *const u8,
        );
        builder.symbol(
            "ar_vec_destroy",
            crate::vec_runtime::ar_vec_destroy as *const u8,
        );
        // SL_R.2 reactor (epoll + timerfd)
        builder.symbol(
            "ar_rt_reactor_create",
            crate::reactor_runtime::ar_rt_reactor_create as *const u8,
        );
        builder.symbol(
            "ar_rt_reactor_destroy",
            crate::reactor_runtime::ar_rt_reactor_destroy as *const u8,
        );
        builder.symbol(
            "ar_rt_reactor_sleep_ms",
            crate::reactor_runtime::ar_rt_reactor_sleep_ms as *const u8,
        );
        builder.symbol(
            "ar_rt_reactor_arm_timer_ms",
            crate::reactor_runtime::ar_rt_reactor_arm_timer_ms as *const u8,
        );
        builder.symbol(
            "ar_rt_reactor_poll_ms",
            crate::reactor_runtime::ar_rt_reactor_poll_ms as *const u8,
        );
        builder.symbol(
            "ar_rt_reactor_backend",
            crate::reactor_runtime::ar_rt_reactor_backend as *const u8,
        );
        builder.symbol(
            "ar_rt_reactor_register_socket",
            crate::reactor_runtime::ar_rt_reactor_register_socket as *const u8,
        );
        // SL_R Waker
        builder.symbol(
            "ar_rt_waker_create",
            crate::waker_runtime::ar_rt_waker_create as *const u8,
        );
        builder.symbol(
            "ar_rt_waker_wake",
            crate::waker_runtime::ar_rt_waker_wake as *const u8,
        );
        builder.symbol(
            "ar_rt_waker_wait",
            crate::waker_runtime::ar_rt_waker_wait as *const u8,
        );
        builder.symbol(
            "ar_rt_waker_destroy",
            crate::waker_runtime::ar_rt_waker_destroy as *const u8,
        );
        // SL_R sockets
        builder.symbol(
            "ar_rt_tcp_listen",
            crate::socket_runtime::ar_rt_tcp_listen as *const u8,
        );
        builder.symbol(
            "ar_rt_tcp_accept",
            crate::socket_runtime::ar_rt_tcp_accept as *const u8,
        );
        builder.symbol(
            "ar_rt_tcp_connect",
            crate::socket_runtime::ar_rt_tcp_connect as *const u8,
        );
        builder.symbol(
            "ar_rt_tcp_read",
            crate::socket_runtime::ar_rt_tcp_read as *const u8,
        );
        builder.symbol(
            "ar_rt_tcp_write",
            crate::socket_runtime::ar_rt_tcp_write as *const u8,
        );
        builder.symbol(
            "ar_rt_tcp_close",
            crate::socket_runtime::ar_rt_tcp_close as *const u8,
        );
        builder.symbol(
            "ar_rt_tcp_set_nonblocking",
            crate::socket_runtime::ar_rt_tcp_set_nonblocking as *const u8,
        );
        builder.symbol(
            "ar_rt_tcp_wait",
            crate::socket_runtime::ar_rt_tcp_wait as *const u8,
        );
        builder.symbol(
            "ar_rt_tcp_wait_wake",
            crate::socket_runtime::ar_rt_tcp_wait_wake as *const u8,
        );
        builder.symbol(
            "ar_rt_tcp_read_async",
            crate::socket_runtime::ar_rt_tcp_read_async as *const u8,
        );
        builder.symbol(
            "ar_rt_tcp_write_async",
            crate::socket_runtime::ar_rt_tcp_write_async as *const u8,
        );
        // SL_R.1 supervisor
        builder.symbol(
            "ar_rt_supervisor_create",
            crate::supervisor_runtime::ar_rt_supervisor_create as *const u8,
        );
        builder.symbol(
            "ar_rt_supervisor_destroy",
            crate::supervisor_runtime::ar_rt_supervisor_destroy as *const u8,
        );
        builder.symbol(
            "ar_rt_supervisor_spawn",
            crate::supervisor_runtime::ar_rt_supervisor_spawn as *const u8,
        );
        builder.symbol(
            "ar_rt_supervisor_spawn_str",
            crate::supervisor_runtime::ar_rt_supervisor_spawn_str as *const u8,
        );
        builder.symbol(
            "ar_rt_supervisor_poll",
            crate::supervisor_runtime::ar_rt_supervisor_poll as *const u8,
        );
        builder.symbol(
            "ar_rt_supervisor_wait",
            crate::supervisor_runtime::ar_rt_supervisor_wait as *const u8,
        );
        builder.symbol(
            "ar_rt_supervisor_kill",
            crate::supervisor_runtime::ar_rt_supervisor_kill as *const u8,
        );
        let module = JITModule::new(builder);

        Ok(Self { module })
    }

    /// Compiles all functions in `program` to native machine code.
    ///
    /// Three-phase compilation:
    /// 1. Declare all functions (enabling mutual recursion).
    /// 2. Define/translate each function body via [`FunctionTranslator`].
    /// 3. Finalize in-memory code and return a [`CompiledModule`].
    #[tracing::instrument(
        level = "trace",
        target = "arandu_backend_cranelift",
        skip(self, program, symbols, type_info)
    )]
    pub fn compile_program(
        mut self,
        program: &AmirProgram,
        symbols: &SymbolTable,
        type_info: &arandu_semantics::TypeInfo,
    ) -> Result<CompiledModule, Diagnostic> {
        if let Some(issue) =
            arandu_semantics::validate_amir_program(program, symbols, &type_info.type_interner)
                .into_iter()
                .next()
        {
            return Err(issue);
        }
        let mut func_ids = FxHashMap::default();
        let default_call_conv = self.module.isa().default_call_conv();
        let ptr_type = self.module.target_config().pointer_type();

        // Declare malloc as import
        let mut malloc_sig = cranelift_codegen::ir::Signature::new(default_call_conv);
        malloc_sig
            .params
            .push(cranelift_codegen::ir::AbiParam::new(ptr_type));
        malloc_sig
            .returns
            .push(cranelift_codegen::ir::AbiParam::new(ptr_type));
        let malloc_id = self
            .module
            .declare_function("malloc", Linkage::Import, &malloc_sig)
            .map_err(|err| codegen_ice(format!("failed to declare malloc: {err:?}")))?;
        func_ids.insert("malloc".to_string(), malloc_id);

        // Declare free as import
        let mut free_sig = cranelift_codegen::ir::Signature::new(default_call_conv);
        free_sig
            .params
            .push(cranelift_codegen::ir::AbiParam::new(ptr_type));
        let free_id = self
            .module
            .declare_function("free", Linkage::Import, &free_sig)
            .map_err(|err| codegen_ice(format!("failed to declare free: {err:?}")))?;
        func_ids.insert("free".to_string(), free_id);

        // G4 raw ABI: pointers and layout widths follow the JIT target.
        let usize_ty = ptr_type;
        let mut insert_sig = cranelift_codegen::ir::Signature::new(default_call_conv);
        for ty in [ptr_type, usize_ty, usize_ty, ptr_type] {
            insert_sig
                .params
                .push(cranelift_codegen::ir::AbiParam::new(ty));
        }
        insert_sig
            .returns
            .push(cranelift_codegen::ir::AbiParam::new(
                cranelift_codegen::ir::types::I64,
            ));
        let mut get_sig = cranelift_codegen::ir::Signature::new(default_call_conv);
        for ty in [
            cranelift_codegen::ir::types::I64,
            ptr_type,
            usize_ty,
            usize_ty,
        ] {
            get_sig
                .params
                .push(cranelift_codegen::ir::AbiParam::new(ty));
        }
        get_sig.returns.push(cranelift_codegen::ir::AbiParam::new(
            cranelift_codegen::ir::types::I8,
        ));
        let mut set_sig = cranelift_codegen::ir::Signature::new(default_call_conv);
        for ty in [
            cranelift_codegen::ir::types::I64,
            ptr_type,
            usize_ty,
            usize_ty,
            ptr_type,
        ] {
            set_sig
                .params
                .push(cranelift_codegen::ir::AbiParam::new(ty));
        }
        set_sig.returns.push(cranelift_codegen::ir::AbiParam::new(
            cranelift_codegen::ir::types::I8,
        ));
        let mut upsert_sig = set_sig.clone();
        upsert_sig.returns.clear();
        upsert_sig
            .returns
            .push(cranelift_codegen::ir::AbiParam::new(
                cranelift_codegen::ir::types::I64,
            ));
        for (name, signature) in [
            ("ar_gen_insert_raw", &insert_sig),
            ("ar_gen_get_raw", &get_sig),
            ("ar_gen_set_raw", &set_sig),
            ("ar_gen_upsert_raw", &upsert_sig),
            ("ar_gen_remove_raw", &get_sig),
        ] {
            let id = self
                .module
                .declare_function(name, Linkage::Import, signature)
                .map_err(|err| codegen_ice(format!("failed to declare {name}: {err:?}")))?;
            func_ids.insert(name.to_string(), id);
        }
        let shutdown_sig = cranelift_codegen::ir::Signature::new(default_call_conv);
        let shutdown_id = self
            .module
            .declare_function("ar_gen_shutdown_raw", Linkage::Import, &shutdown_sig)
            .map_err(|err| {
                codegen_ice(format!("failed to declare ar_gen_shutdown_raw: {err:?}"))
            })?;
        func_ids.insert("ar_gen_shutdown_raw".to_string(), shutdown_id);

        // A3.6: block_on(state) -> i64
        let mut block_on_sig = cranelift_codegen::ir::Signature::new(default_call_conv);
        block_on_sig
            .params
            .push(cranelift_codegen::ir::AbiParam::new(ptr_type));
        block_on_sig
            .returns
            .push(cranelift_codegen::ir::AbiParam::new(
                cranelift_codegen::ir::types::I64,
            ));
        let block_on_id = self
            .module
            .declare_function("ar_co_block_on_i64", Linkage::Import, &block_on_sig)
            .map_err(|err| codegen_ice(format!("failed to declare ar_co_block_on_i64: {err:?}")))?;
        func_ids.insert("ar_co_block_on_i64".to_string(), block_on_id);

        // A3.6: poll(state, *out) -> i32
        let mut poll_sig = cranelift_codegen::ir::Signature::new(default_call_conv);
        poll_sig
            .params
            .push(cranelift_codegen::ir::AbiParam::new(ptr_type));
        poll_sig
            .params
            .push(cranelift_codegen::ir::AbiParam::new(ptr_type));
        poll_sig.returns.push(cranelift_codegen::ir::AbiParam::new(
            cranelift_codegen::ir::types::I32,
        ));
        let poll_id = self
            .module
            .declare_function("ar_co_poll_i64", Linkage::Import, &poll_sig)
            .map_err(|err| codegen_ice(format!("failed to declare ar_co_poll_i64: {err:?}")))?;
        func_ids.insert("ar_co_poll_i64".to_string(), poll_id);

        // A3.6 / SL_R tests: make_ready(payload:i64) -> *u8
        let mut make_ready_sig = cranelift_codegen::ir::Signature::new(default_call_conv);
        make_ready_sig
            .params
            .push(cranelift_codegen::ir::AbiParam::new(
                cranelift_codegen::ir::types::I64,
            ));
        make_ready_sig
            .returns
            .push(cranelift_codegen::ir::AbiParam::new(ptr_type));
        let make_ready_id = self
            .module
            .declare_function("ar_co_make_ready_i64", Linkage::Import, &make_ready_sig)
            .map_err(|err| {
                codegen_ice(format!("failed to declare ar_co_make_ready_i64: {err:?}"))
            })?;
        func_ids.insert("ar_co_make_ready_i64".to_string(), make_ready_id);

        // SL_R.0 + SL_S path host imports (also registered in JITBuilder::symbol).
        let mut rt_block_sig = cranelift_codegen::ir::Signature::new(default_call_conv);
        rt_block_sig
            .params
            .push(cranelift_codegen::ir::AbiParam::new(ptr_type));
        rt_block_sig
            .returns
            .push(cranelift_codegen::ir::AbiParam::new(
                cranelift_codegen::ir::types::I64,
            ));
        for name in ["ar_rt_block_on_i64", "ar_co_block_on_i64"] {
            if !func_ids.contains_key(name) {
                let id = self
                    .module
                    .declare_function(name, Linkage::Import, &rt_block_sig)
                    .map_err(|err| codegen_ice(format!("failed to declare {name}: {err:?}")))?;
                func_ids.insert(name.to_string(), id);
            }
        }
        let mut rt_spawn_sig = cranelift_codegen::ir::Signature::new(default_call_conv);
        rt_spawn_sig
            .params
            .push(cranelift_codegen::ir::AbiParam::new(ptr_type));
        rt_spawn_sig
            .returns
            .push(cranelift_codegen::ir::AbiParam::new(
                cranelift_codegen::ir::types::I64,
            ));
        {
            let name = "ar_rt_spawn_i64";
            let id = self
                .module
                .declare_function(name, Linkage::Import, &rt_spawn_sig)
                .map_err(|err| codegen_ice(format!("failed to declare {name}: {err:?}")))?;
            func_ids.insert(name.to_string(), id);
        }
        let mut rt_join_sig = cranelift_codegen::ir::Signature::new(default_call_conv);
        rt_join_sig
            .params
            .push(cranelift_codegen::ir::AbiParam::new(
                cranelift_codegen::ir::types::I64,
            ));
        rt_join_sig
            .returns
            .push(cranelift_codegen::ir::AbiParam::new(
                cranelift_codegen::ir::types::I64,
            ));
        {
            let name = "ar_rt_join_i64";
            let id = self
                .module
                .declare_function(name, Linkage::Import, &rt_join_sig)
                .map_err(|err| codegen_ice(format!("failed to declare {name}: {err:?}")))?;
            func_ids.insert(name.to_string(), id);
        }
        let mut rt_cancel_sig = cranelift_codegen::ir::Signature::new(default_call_conv);
        rt_cancel_sig
            .params
            .push(cranelift_codegen::ir::AbiParam::new(
                cranelift_codegen::ir::types::I64,
            ));
        let cancel_id = self
            .module
            .declare_function("ar_rt_cancel_i64", Linkage::Import, &rt_cancel_sig)
            .map_err(|err| codegen_ice(format!("failed to declare ar_rt_cancel_i64: {err:?}")))?;
        func_ids.insert("ar_rt_cancel_i64".to_string(), cancel_id);

        let mut path_sig = cranelift_codegen::ir::Signature::new(default_call_conv);
        // fat str: ptr + i64 len
        path_sig
            .params
            .push(cranelift_codegen::ir::AbiParam::new(ptr_type));
        path_sig.params.push(cranelift_codegen::ir::AbiParam::new(
            cranelift_codegen::ir::types::I64,
        ));
        path_sig.returns.push(cranelift_codegen::ir::AbiParam::new(
            cranelift_codegen::ir::types::I64,
        ));
        for name in [
            "ar_path_is_absolute",
            "ar_path_is_empty",
            "ar_env_var_is_set",
        ] {
            let id = self
                .module
                .declare_function(name, Linkage::Import, &path_sig)
                .map_err(|err| codegen_ice(format!("failed to declare {name}: {err:?}")))?;
            func_ids.insert(name.to_string(), id);
        }
        // path join / file_name: (str, str) → str  or  (str) → str
        // str expands to [ptr, i64] params and [ptr, i64] returns (ArFatStr).
        {
            let mut join_sig = cranelift_codegen::ir::Signature::new(default_call_conv);
            #[cfg(windows)]
            {
                join_sig.call_conv = cranelift_codegen::isa::CallConv::SystemV;
            }
            for _ in 0..2 {
                join_sig
                    .params
                    .push(cranelift_codegen::ir::AbiParam::new(ptr_type));
                join_sig.params.push(cranelift_codegen::ir::AbiParam::new(
                    cranelift_codegen::ir::types::I64,
                ));
            }
            join_sig
                .returns
                .push(cranelift_codegen::ir::AbiParam::new(ptr_type));
            join_sig.returns.push(cranelift_codegen::ir::AbiParam::new(
                cranelift_codegen::ir::types::I64,
            ));
            let id = self
                .module
                .declare_function("ar_path_join", Linkage::Import, &join_sig)
                .map_err(|err| codegen_ice(format!("failed to declare ar_path_join: {err:?}")))?;
            func_ids.insert("ar_path_join".to_string(), id);

            let mut file_sig = cranelift_codegen::ir::Signature::new(default_call_conv);
            #[cfg(windows)]
            {
                file_sig.call_conv = cranelift_codegen::isa::CallConv::SystemV;
            }
            file_sig
                .params
                .push(cranelift_codegen::ir::AbiParam::new(ptr_type));
            file_sig.params.push(cranelift_codegen::ir::AbiParam::new(
                cranelift_codegen::ir::types::I64,
            ));
            file_sig
                .returns
                .push(cranelift_codegen::ir::AbiParam::new(ptr_type));
            file_sig.returns.push(cranelift_codegen::ir::AbiParam::new(
                cranelift_codegen::ir::types::I64,
            ));
            let id = self
                .module
                .declare_function("ar_path_file_name", Linkage::Import, &file_sig)
                .map_err(|err| {
                    codegen_ice(format!("failed to declare ar_path_file_name: {err:?}"))
                })?;
            func_ids.insert("ar_path_file_name".to_string(), id);

            // std.core.str thin hosts
            let mut len_sig = cranelift_codegen::ir::Signature::new(default_call_conv);
            len_sig
                .params
                .push(cranelift_codegen::ir::AbiParam::new(ptr_type));
            len_sig.params.push(cranelift_codegen::ir::AbiParam::new(
                cranelift_codegen::ir::types::I64,
            ));
            len_sig.returns.push(cranelift_codegen::ir::AbiParam::new(
                cranelift_codegen::ir::types::I64,
            ));
            let id = self
                .module
                .declare_function("ar_str_len", Linkage::Import, &len_sig)
                .map_err(|err| codegen_ice(format!("failed to declare ar_str_len: {err:?}")))?;
            func_ids.insert("ar_str_len".to_string(), id);

            // concat / starts / ends / split_last reuse join_sig or file_sig shapes
            let id = self
                .module
                .declare_function("ar_str_concat", Linkage::Import, &join_sig)
                .map_err(|err| codegen_ice(format!("failed to declare ar_str_concat: {err:?}")))?;
            func_ids.insert("ar_str_concat".to_string(), id);

            let mut pref_sig = cranelift_codegen::ir::Signature::new(default_call_conv);
            for _ in 0..2 {
                pref_sig
                    .params
                    .push(cranelift_codegen::ir::AbiParam::new(ptr_type));
                pref_sig.params.push(cranelift_codegen::ir::AbiParam::new(
                    cranelift_codegen::ir::types::I64,
                ));
            }
            pref_sig.returns.push(cranelift_codegen::ir::AbiParam::new(
                cranelift_codegen::ir::types::I64,
            ));
            for name in ["ar_str_starts_with", "ar_str_ends_with"] {
                let id = self
                    .module
                    .declare_function(name, Linkage::Import, &pref_sig)
                    .map_err(|err| codegen_ice(format!("failed to declare {name}: {err:?}")))?;
                func_ids.insert(name.to_string(), id);
            }
            let id = self
                .module
                .declare_function("ar_str_split_last", Linkage::Import, &join_sig)
                .map_err(|err| {
                    codegen_ice(format!("failed to declare ar_str_split_last: {err:?}"))
                })?;
            func_ids.insert("ar_str_split_last".to_string(), id);
        }

        // Minimal OS: exit(void), monotonic_ns/args_len (i64 -> / <-)
        {
            let mut exit_sig = cranelift_codegen::ir::Signature::new(default_call_conv);
            exit_sig.params.push(cranelift_codegen::ir::AbiParam::new(
                cranelift_codegen::ir::types::I64,
            ));
            // No returns: noreturn host (process::exit).
            let id = self
                .module
                .declare_function("ar_process_exit", Linkage::Import, &exit_sig)
                .map_err(|err| {
                    codegen_ice(format!("failed to declare ar_process_exit: {err:?}"))
                })?;
            func_ids.insert("ar_process_exit".to_string(), id);
        }
        {
            let mut noarg_i64 = cranelift_codegen::ir::Signature::new(default_call_conv);
            noarg_i64.returns.push(cranelift_codegen::ir::AbiParam::new(
                cranelift_codegen::ir::types::I64,
            ));
            for name in ["ar_time_monotonic_ns", "ar_env_args_len"] {
                let id = self
                    .module
                    .declare_function(name, Linkage::Import, &noarg_i64)
                    .map_err(|err| codegen_ice(format!("failed to declare {name}: {err:?}")))?;
                func_ids.insert(name.to_string(), id);
            }
        }

        // Vec host: new() -> i64; free(id); clear(id); len/has/get/put/push/pop
        {
            let mut noarg_i64 = cranelift_codegen::ir::Signature::new(default_call_conv);
            noarg_i64.returns.push(cranelift_codegen::ir::AbiParam::new(
                cranelift_codegen::ir::types::I64,
            ));
            let id = self
                .module
                .declare_function("ar_vec_new", Linkage::Import, &noarg_i64)
                .map_err(|err| codegen_ice(format!("failed to declare ar_vec_new: {err:?}")))?;
            func_ids.insert("ar_vec_new".to_string(), id);
        }
        {
            let mut one_i64 = cranelift_codegen::ir::Signature::new(default_call_conv);
            one_i64.params.push(cranelift_codegen::ir::AbiParam::new(
                cranelift_codegen::ir::types::I64,
            ));
            for name in ["ar_vec_destroy", "ar_vec_clear"] {
                let id = self
                    .module
                    .declare_function(name, Linkage::Import, &one_i64)
                    .map_err(|err| codegen_ice(format!("failed to declare {name}: {err:?}")))?;
                func_ids.insert(name.to_string(), id);
            }
            let mut one_ret = one_i64.clone();
            one_ret.returns.push(cranelift_codegen::ir::AbiParam::new(
                cranelift_codegen::ir::types::I64,
            ));
            let id = self
                .module
                .declare_function("ar_vec_len", Linkage::Import, &one_ret)
                .map_err(|err| codegen_ice(format!("failed to declare ar_vec_len: {err:?}")))?;
            func_ids.insert("ar_vec_len".to_string(), id);
        }
        {
            let mut two = cranelift_codegen::ir::Signature::new(default_call_conv);
            two.params.push(cranelift_codegen::ir::AbiParam::new(
                cranelift_codegen::ir::types::I64,
            ));
            two.params.push(cranelift_codegen::ir::AbiParam::new(
                cranelift_codegen::ir::types::I64,
            ));
            // push(id, val) void
            let id = self
                .module
                .declare_function("ar_vec_push", Linkage::Import, &two)
                .map_err(|err| codegen_ice(format!("failed to declare ar_vec_push: {err:?}")))?;
            func_ids.insert("ar_vec_push".to_string(), id);
            let mut two_ret = two.clone();
            two_ret.returns.push(cranelift_codegen::ir::AbiParam::new(
                cranelift_codegen::ir::types::I64,
            ));
            for name in ["ar_vec_has", "ar_vec_get"] {
                let id = self
                    .module
                    .declare_function(name, Linkage::Import, &two_ret)
                    .map_err(|err| codegen_ice(format!("failed to declare {name}: {err:?}")))?;
                func_ids.insert(name.to_string(), id);
            }
            let mut three_ret = two_ret.clone();
            three_ret.params.push(cranelift_codegen::ir::AbiParam::new(
                cranelift_codegen::ir::types::I64,
            ));
            let id = self
                .module
                .declare_function("ar_vec_put", Linkage::Import, &three_ret)
                .map_err(|err| codegen_ice(format!("failed to declare ar_vec_put: {err:?}")))?;
            func_ids.insert("ar_vec_put".to_string(), id);
            // pop(id) -> i64
            let mut one_ret = cranelift_codegen::ir::Signature::new(default_call_conv);
            one_ret.params.push(cranelift_codegen::ir::AbiParam::new(
                cranelift_codegen::ir::types::I64,
            ));
            one_ret.returns.push(cranelift_codegen::ir::AbiParam::new(
                cranelift_codegen::ir::types::I64,
            ));
            let id = self
                .module
                .declare_function("ar_vec_pop", Linkage::Import, &one_ret)
                .map_err(|err| codegen_ice(format!("failed to declare ar_vec_pop: {err:?}")))?;
            func_ids.insert("ar_vec_pop".to_string(), id);
        }
        // Raw buffer helpers for pure-Arandu Vec (L6.1)
        {
            let mut malloc_sig = cranelift_codegen::ir::Signature::new(default_call_conv);
            malloc_sig.params.push(cranelift_codegen::ir::AbiParam::new(
                cranelift_codegen::ir::types::I64,
            ));
            malloc_sig
                .returns
                .push(cranelift_codegen::ir::AbiParam::new(ptr_type));
            let id = self
                .module
                .declare_function("ar_vec_malloc", Linkage::Import, &malloc_sig)
                .map_err(|err| codegen_ice(format!("failed to declare ar_vec_malloc: {err:?}")))?;
            func_ids.insert("ar_vec_malloc".to_string(), id);
        }
        {
            let mut free_sig = cranelift_codegen::ir::Signature::new(default_call_conv);
            free_sig
                .params
                .push(cranelift_codegen::ir::AbiParam::new(ptr_type));
            free_sig.params.push(cranelift_codegen::ir::AbiParam::new(
                cranelift_codegen::ir::types::I64,
            ));
            let id = self
                .module
                .declare_function("ar_vec_buf_free", Linkage::Import, &free_sig)
                .map_err(|err| {
                    codegen_ice(format!("failed to declare ar_vec_buf_free: {err:?}"))
                })?;
            func_ids.insert("ar_vec_buf_free".to_string(), id);
        }
        {
            let mut realloc_sig = cranelift_codegen::ir::Signature::new(default_call_conv);
            realloc_sig
                .params
                .push(cranelift_codegen::ir::AbiParam::new(ptr_type));
            realloc_sig
                .params
                .push(cranelift_codegen::ir::AbiParam::new(
                    cranelift_codegen::ir::types::I64,
                ));
            realloc_sig
                .params
                .push(cranelift_codegen::ir::AbiParam::new(
                    cranelift_codegen::ir::types::I64,
                ));
            realloc_sig
                .returns
                .push(cranelift_codegen::ir::AbiParam::new(ptr_type));
            let id = self
                .module
                .declare_function("ar_vec_realloc", Linkage::Import, &realloc_sig)
                .map_err(|err| codegen_ice(format!("failed to declare ar_vec_realloc: {err:?}")))?;
            func_ids.insert("ar_vec_realloc".to_string(), id);
        }

        // SL_R.2 reactor host imports
        {
            let mut create_sig = cranelift_codegen::ir::Signature::new(default_call_conv);
            create_sig
                .returns
                .push(cranelift_codegen::ir::AbiParam::new(
                    cranelift_codegen::ir::types::I64,
                ));
            let id = self
                .module
                .declare_function("ar_rt_reactor_create", Linkage::Import, &create_sig)
                .map_err(|err| {
                    codegen_ice(format!("failed to declare ar_rt_reactor_create: {err:?}"))
                })?;
            func_ids.insert("ar_rt_reactor_create".to_string(), id);
        }
        {
            let mut destroy_sig = cranelift_codegen::ir::Signature::new(default_call_conv);
            destroy_sig
                .params
                .push(cranelift_codegen::ir::AbiParam::new(
                    cranelift_codegen::ir::types::I64,
                ));
            let id = self
                .module
                .declare_function("ar_rt_reactor_destroy", Linkage::Import, &destroy_sig)
                .map_err(|err| {
                    codegen_ice(format!("failed to declare ar_rt_reactor_destroy: {err:?}"))
                })?;
            func_ids.insert("ar_rt_reactor_destroy".to_string(), id);
        }
        // sleep/arm/poll: (reactor_id: i64, ms: i64) -> i64
        {
            let mut two_i64_ret_i64 = cranelift_codegen::ir::Signature::new(default_call_conv);
            two_i64_ret_i64
                .params
                .push(cranelift_codegen::ir::AbiParam::new(
                    cranelift_codegen::ir::types::I64,
                ));
            two_i64_ret_i64
                .params
                .push(cranelift_codegen::ir::AbiParam::new(
                    cranelift_codegen::ir::types::I64,
                ));
            two_i64_ret_i64
                .returns
                .push(cranelift_codegen::ir::AbiParam::new(
                    cranelift_codegen::ir::types::I64,
                ));
            for name in [
                "ar_rt_reactor_sleep_ms",
                "ar_rt_reactor_arm_timer_ms",
                "ar_rt_reactor_poll_ms",
                "ar_rt_waker_wait",
                "ar_rt_supervisor_poll",
                "ar_rt_supervisor_wait",
                "ar_rt_supervisor_kill",
            ] {
                let id = self
                    .module
                    .declare_function(name, Linkage::Import, &two_i64_ret_i64)
                    .map_err(|err| codegen_ice(format!("failed to declare {name}: {err:?}")))?;
                func_ids.insert(name.to_string(), id);
            }
        }
        // register_socket: (reactor_id: i64, sock_id: i64, events: i64, waker_id: i64) -> i64
        {
            let mut sig = cranelift_codegen::ir::Signature::new(default_call_conv);
            for _ in 0..4 {
                sig.params.push(cranelift_codegen::ir::AbiParam::new(
                    cranelift_codegen::ir::types::I64,
                ));
            }
            sig.returns.push(cranelift_codegen::ir::AbiParam::new(
                cranelift_codegen::ir::types::I64,
            ));
            let id = self
                .module
                .declare_function("ar_rt_reactor_register_socket", Linkage::Import, &sig)
                .map_err(|err| {
                    codegen_ice(format!(
                        "failed to declare ar_rt_reactor_register_socket: {err:?}"
                    ))
                })?;
            func_ids.insert("ar_rt_reactor_register_socket".to_string(), id);
        }
        // 0-arg -> i64
        {
            let mut sig = cranelift_codegen::ir::Signature::new(default_call_conv);
            sig.returns.push(cranelift_codegen::ir::AbiParam::new(
                cranelift_codegen::ir::types::I64,
            ));
            for name in [
                "ar_rt_reactor_backend",
                "ar_rt_waker_create",
                "ar_rt_supervisor_create",
            ] {
                if !func_ids.contains_key(name) {
                    let id = self
                        .module
                        .declare_function(name, Linkage::Import, &sig)
                        .map_err(|err| codegen_ice(format!("failed to declare {name}: {err:?}")))?;
                    func_ids.insert(name.to_string(), id);
                }
            }
        }
        // 1-arg i64 -> void / i64
        {
            let mut void_sig = cranelift_codegen::ir::Signature::new(default_call_conv);
            void_sig.params.push(cranelift_codegen::ir::AbiParam::new(
                cranelift_codegen::ir::types::I64,
            ));
            for name in [
                "ar_rt_waker_wake",
                "ar_rt_waker_destroy",
                "ar_rt_supervisor_destroy",
                "ar_rt_tcp_close",
            ] {
                let id = self
                    .module
                    .declare_function(name, Linkage::Import, &void_sig)
                    .map_err(|err| codegen_ice(format!("failed to declare {name}: {err:?}")))?;
                func_ids.insert(name.to_string(), id);
            }
            let mut ret_sig = cranelift_codegen::ir::Signature::new(default_call_conv);
            ret_sig.params.push(cranelift_codegen::ir::AbiParam::new(
                cranelift_codegen::ir::types::I64,
            ));
            ret_sig.returns.push(cranelift_codegen::ir::AbiParam::new(
                cranelift_codegen::ir::types::I64,
            ));
            for name in ["ar_rt_tcp_listen", "ar_rt_tcp_accept", "ar_rt_tcp_connect"] {
                let id = self
                    .module
                    .declare_function(name, Linkage::Import, &ret_sig)
                    .map_err(|err| codegen_ice(format!("failed to declare {name}: {err:?}")))?;
                func_ids.insert(name.to_string(), id);
            }
        }
        // tcp_read/write: (sock, ptr, len) -> i64
        {
            let mut sig = cranelift_codegen::ir::Signature::new(default_call_conv);
            sig.params.push(cranelift_codegen::ir::AbiParam::new(
                cranelift_codegen::ir::types::I64,
            ));
            sig.params
                .push(cranelift_codegen::ir::AbiParam::new(ptr_type));
            sig.params.push(cranelift_codegen::ir::AbiParam::new(
                cranelift_codegen::ir::types::I64,
            ));
            sig.returns.push(cranelift_codegen::ir::AbiParam::new(
                cranelift_codegen::ir::types::I64,
            ));
            for name in [
                "ar_rt_tcp_read",
                "ar_rt_tcp_write",
                "ar_rt_tcp_read_async",
                "ar_rt_tcp_write_async",
            ] {
                let id = self
                    .module
                    .declare_function(name, Linkage::Import, &sig)
                    .map_err(|err| codegen_ice(format!("failed to declare {name}: {err:?}")))?;
                func_ids.insert(name.to_string(), id);
            }
        }
        // set_nonblocking / wait: (sock, events|flag [, timeout])
        {
            let mut two = cranelift_codegen::ir::Signature::new(default_call_conv);
            two.params.push(cranelift_codegen::ir::AbiParam::new(
                cranelift_codegen::ir::types::I64,
            ));
            two.params.push(cranelift_codegen::ir::AbiParam::new(
                cranelift_codegen::ir::types::I64,
            ));
            two.returns.push(cranelift_codegen::ir::AbiParam::new(
                cranelift_codegen::ir::types::I64,
            ));
            let id = self
                .module
                .declare_function("ar_rt_tcp_set_nonblocking", Linkage::Import, &two)
                .map_err(|err| {
                    codegen_ice(format!(
                        "failed to declare ar_rt_tcp_set_nonblocking: {err:?}"
                    ))
                })?;
            func_ids.insert("ar_rt_tcp_set_nonblocking".to_string(), id);

            let mut three = cranelift_codegen::ir::Signature::new(default_call_conv);
            for _ in 0..3 {
                three.params.push(cranelift_codegen::ir::AbiParam::new(
                    cranelift_codegen::ir::types::I64,
                ));
            }
            three.returns.push(cranelift_codegen::ir::AbiParam::new(
                cranelift_codegen::ir::types::I64,
            ));
            let id = self
                .module
                .declare_function("ar_rt_tcp_wait", Linkage::Import, &three)
                .map_err(|err| codegen_ice(format!("failed to declare ar_rt_tcp_wait: {err:?}")))?;
            func_ids.insert("ar_rt_tcp_wait".to_string(), id);

            let mut four = cranelift_codegen::ir::Signature::new(default_call_conv);
            for _ in 0..4 {
                four.params.push(cranelift_codegen::ir::AbiParam::new(
                    cranelift_codegen::ir::types::I64,
                ));
            }
            four.returns.push(cranelift_codegen::ir::AbiParam::new(
                cranelift_codegen::ir::types::I64,
            ));
            let id = self
                .module
                .declare_function("ar_rt_tcp_wait_wake", Linkage::Import, &four)
                .map_err(|err| {
                    codegen_ice(format!("failed to declare ar_rt_tcp_wait_wake: {err:?}"))
                })?;
            func_ids.insert("ar_rt_tcp_wait_wake".to_string(), id);
        }
        // supervisor_spawn: (sup, path_ptr, path_len, max_restarts) -> i64
        {
            let mut sig = cranelift_codegen::ir::Signature::new(default_call_conv);
            for _ in 0..4 {
                // params: i64, ptr, i64, i64 — first and last are i64; path is fat
            }
            sig.params.push(cranelift_codegen::ir::AbiParam::new(
                cranelift_codegen::ir::types::I64,
            ));
            sig.params
                .push(cranelift_codegen::ir::AbiParam::new(ptr_type));
            sig.params.push(cranelift_codegen::ir::AbiParam::new(
                cranelift_codegen::ir::types::I64,
            ));
            sig.params.push(cranelift_codegen::ir::AbiParam::new(
                cranelift_codegen::ir::types::I64,
            ));
            sig.returns.push(cranelift_codegen::ir::AbiParam::new(
                cranelift_codegen::ir::types::I64,
            ));
            for name in ["ar_rt_supervisor_spawn", "ar_rt_supervisor_spawn_str"] {
                let id = self
                    .module
                    .declare_function(name, Linkage::Import, &sig)
                    .map_err(|err| codegen_ice(format!("failed to declare {name}: {err:?}")))?;
                func_ids.insert(name.to_string(), id);
            }
        }

        // Declare fmod as import
        let mut fmod_sig = cranelift_codegen::ir::Signature::new(default_call_conv);
        fmod_sig.params.push(cranelift_codegen::ir::AbiParam::new(
            cranelift_codegen::ir::types::F64,
        ));
        fmod_sig.params.push(cranelift_codegen::ir::AbiParam::new(
            cranelift_codegen::ir::types::F64,
        ));
        fmod_sig.returns.push(cranelift_codegen::ir::AbiParam::new(
            cranelift_codegen::ir::types::F64,
        ));
        let fmod_id = self
            .module
            .declare_function("fmod", Linkage::Import, &fmod_sig)
            .map_err(|err| codegen_ice(format!("failed to declare fmod: {err:?}")))?;
        func_ids.insert("fmod".to_string(), fmod_id);

        // Declare memcpy as import (used by string interpolation concat)
        let mut memcpy_sig = cranelift_codegen::ir::Signature::new(default_call_conv);
        memcpy_sig
            .params
            .push(cranelift_codegen::ir::AbiParam::new(ptr_type));
        memcpy_sig
            .params
            .push(cranelift_codegen::ir::AbiParam::new(ptr_type));
        memcpy_sig
            .params
            .push(cranelift_codegen::ir::AbiParam::new(ptr_type));
        memcpy_sig
            .returns
            .push(cranelift_codegen::ir::AbiParam::new(ptr_type));
        let memcpy_id = self
            .module
            .declare_function("memcpy", Linkage::Import, &memcpy_sig)
            .map_err(|err| codegen_ice(format!("failed to declare memcpy: {err:?}")))?;
        func_ids.insert("memcpy".to_string(), memcpy_id);

        // Declare memcmp as import (used by `str` equality / inequality).
        let mut memcmp_sig = cranelift_codegen::ir::Signature::new(default_call_conv);
        memcmp_sig
            .params
            .push(cranelift_codegen::ir::AbiParam::new(ptr_type));
        memcmp_sig
            .params
            .push(cranelift_codegen::ir::AbiParam::new(ptr_type));
        memcmp_sig
            .params
            .push(cranelift_codegen::ir::AbiParam::new(ptr_type));
        memcmp_sig
            .returns
            .push(cranelift_codegen::ir::AbiParam::new(
                cranelift_codegen::ir::types::I32,
            ));
        let memcmp_id = self
            .module
            .declare_function("memcmp", Linkage::Import, &memcmp_sig)
            .map_err(|err| codegen_ice(format!("failed to declare memcmp: {err:?}")))?;
        func_ids.insert("memcmp".to_string(), memcmp_id);

        // ToStr v0.1: host helpers `(value, *mut i64 out_len) -> *mut u8`
        let i64_ty = cranelift_codegen::ir::types::I64;
        let f64_ty = cranelift_codegen::ir::types::F64;
        let i8_ty = cranelift_codegen::ir::types::I8;
        let i32_ty = cranelift_codegen::ir::types::I32;
        for (name, val_ty) in [
            ("ar_jit_i64_to_str", i64_ty),
            ("ar_jit_u64_to_str", i64_ty),
            ("ar_jit_f64_to_str", f64_ty),
            ("ar_jit_bool_to_str", i8_ty),
            ("ar_jit_char_to_str", i32_ty),
            // Err handle → fat str (ptr is the message buffer itself).
            ("ar_jit_err_to_str", ptr_type),
        ] {
            let mut sig = cranelift_codegen::ir::Signature::new(default_call_conv);
            sig.params
                .push(cranelift_codegen::ir::AbiParam::new(val_ty));
            sig.params
                .push(cranelift_codegen::ir::AbiParam::new(ptr_type));
            sig.returns
                .push(cranelift_codegen::ir::AbiParam::new(ptr_type));
            let id = self
                .module
                .declare_function(name, Linkage::Import, &sig)
                .map_err(|err| codegen_ice(format!("failed to declare {name}: {err:?}")))?;
            func_ids.insert(name.to_string(), id);
        }

        // 1. Declare all functions first to support cross-calls
        for func in &program.funcs {
            let sym = symbols.get(func.symbol);
            let param_types: Vec<_> = func
                .params
                .iter()
                .map(|&p| type_info.type_interner.resolve(func.temps[p.as_usize()].ty))
                .collect();
            let ret_ty = type_info.type_interner.resolve(func.return_type);
            let sig = build_signature(&param_types, &ret_ty, default_call_conv, ptr_type);

            let func_id = self
                .module
                .declare_function(&sym.name, Linkage::Export, &sig)
                .map_err(|err| {
                    codegen_ice(format!(
                        "failed to declare function '{}': {err:?}",
                        sym.name
                    ))
                })?;
            func_ids.insert(sym.name.to_string(), func_id);

            // Also find all NamespaceMember symbols that refer to this function (by matching name ending and span)
            // and map them to the same func_id!
            use arandu_semantics::SymbolKind;
            for s in symbols.iter() {
                if s.kind == SymbolKind::NamespaceMember
                    && s.name.ends_with(&format!(".{}", sym.name))
                    && s.span == sym.span
                {
                    func_ids.insert(s.name.to_string(), func_id);
                }
            }
        }

        // Declare one target-native drop shim per explicitly destructible
        // GenRef payload. The shim ABI is always `(ptr) -> void`; it forwards
        // the address-owned representation to the Arandu destructor without
        // deallocating the runtime-owned payload storage.
        let mut drop_shims = std::collections::BTreeMap::new();
        for func in &program.funcs {
            for stmt in func.stmts.payloads.iter() {
                let arandu_semantics::amir::AmirStmt::Assign { rhs, .. } = stmt else {
                    continue;
                };
                let payload_ty = match rhs {
                    arandu_semantics::amir::AmirRvalue::GenInsert { payload_ty, .. }
                    | arandu_semantics::amir::AmirRvalue::GenSet { payload_ty, .. }
                    | arandu_semantics::amir::AmirRvalue::GenUpsert { payload_ty, .. } => {
                        *payload_ty
                    }
                    _ => continue,
                };
                let ArType::Named(_, _) = type_info.resolve_type_id(payload_ty) else {
                    continue;
                };
                let Some(&destructor_symbol) = type_info.destructor_instances.get(&payload_ty)
                else {
                    continue;
                };
                let name = format!(
                    "__ar_drop_{}_{}",
                    destructor_symbol.file_id, destructor_symbol.local_id.0
                );
                if drop_shims.contains_key(&name) {
                    continue;
                }
                let mut signature = cranelift_codegen::ir::Signature::new(default_call_conv);
                signature
                    .params
                    .push(cranelift_codegen::ir::AbiParam::new(ptr_type));
                let shim_id = self
                    .module
                    .declare_function(&name, Linkage::Local, &signature)
                    .map_err(|err| {
                        codegen_ice(format!("failed to declare drop shim '{name}': {err:?}"))
                    })?;
                func_ids.insert(name.clone(), shim_id);
                drop_shims.insert(name, (shim_id, destructor_symbol, signature));
            }
        }

        // Declare all extern functions as imports
        for (&symbol_id, (param_types, return_type)) in &program.extern_funcs {
            let sym = symbols.get(symbol_id);
            if func_ids.contains_key(sym.name.as_str()) {
                continue;
            }
            let c_name = sym.name.split('.').next_back().unwrap_or(&sym.name);
            let func_id = if let Some(&existing_id) = func_ids.get(c_name) {
                existing_id
            } else {
                let sig = build_signature(param_types, return_type, default_call_conv, ptr_type);
                self.module
                    .declare_function(c_name, Linkage::Import, &sig)
                    .map_err(|err| {
                        codegen_ice(format!(
                            "failed to declare extern function '{}': {err:?}",
                            c_name
                        ))
                    })?
            };
            func_ids.insert(sym.name.to_string(), func_id);
            if c_name != sym.name {
                func_ids.insert(c_name.to_string(), func_id);
            }
        }

        // Builtin prelude host imports (fat-pointer `str` args).
        let str_ty = ArType::Primitive(Primitive::Str);
        let void_ty = ArType::Void;
        let err_ty = ArType::Err;
        if !func_ids.contains_key("io.println") {
            let sig = build_signature(
                std::slice::from_ref(&str_ty),
                &void_ty,
                default_call_conv,
                ptr_type,
            );
            let id = self
                .module
                .declare_function("io.println", Linkage::Import, &sig)
                .map_err(|err| codegen_ice(format!("failed to declare io.println: {err:?}")))?;
            func_ids.insert("io.println".to_string(), id);
        }
        // `err.new(str) -> Err` (Err = message pointer handle).
        if !func_ids.contains_key("err.new") {
            let sig = build_signature(
                std::slice::from_ref(&str_ty),
                &err_ty,
                default_call_conv,
                ptr_type,
            );
            let id = self
                .module
                .declare_function("err.new", Linkage::Import, &sig)
                .map_err(|err| codegen_ice(format!("failed to declare err.new: {err:?}")))?;
            func_ids.insert("err.new".to_string(), id);
        }

        // 2. Define/compile each function
        let mut context = self.module.make_context();

        for func in &program.funcs {
            let mut builder_context = FunctionBuilderContext::new();
            let sym = symbols.get(func.symbol);
            let func_id = func_ids[sym.name.as_str()];

            let param_types: Vec<_> = func
                .params
                .iter()
                .map(|&p| type_info.type_interner.resolve(func.temps[p.as_usize()].ty))
                .collect();
            let ret_ty = type_info.type_interner.resolve(func.return_type);
            let sig = build_signature(&param_types, &ret_ty, default_call_conv, ptr_type);
            context.func.signature = sig;

            {
                let builder = FunctionBuilder::new(&mut context.func, &mut builder_context);
                let mut translator = FunctionTranslator::new(
                    builder,
                    &mut self.module,
                    symbols,
                    &func_ids,
                    ptr_type,
                    &program.literal_pool,
                    func,
                    type_info,
                );
                translator.translate()?;
            }

            self.module
                .define_function(func_id, &mut context)
                .map_err(|err| {
                    codegen_ice(format!("failed to define function '{}': {err:?}", sym.name))
                })?;
            self.module.clear_context(&mut context);
        }

        for (name, (shim_id, destructor_symbol, signature)) in drop_shims {
            let destructor_name = symbols.get(destructor_symbol).name.as_str();
            let Some(&destructor_id) = func_ids.get(destructor_name) else {
                return Err(codegen_ice(format!(
                    "drop shim '{name}' references unavailable destructor '{destructor_name}'"
                )));
            };
            context.func.signature = signature;
            let mut builder_context = FunctionBuilderContext::new();
            {
                use cranelift_codegen::ir::InstBuilder;
                let mut builder = FunctionBuilder::new(&mut context.func, &mut builder_context);
                let entry = builder.create_block();
                builder.append_block_params_for_function_params(entry);
                builder.switch_to_block(entry);
                builder.seal_block(entry);
                let raw = builder.block_params(entry)[0];
                let destructor = self
                    .module
                    .declare_func_in_func(destructor_id, builder.func);
                builder.ins().call(destructor, &[raw]);
                builder.ins().return_(&[]);
                builder.seal_all_blocks();
            }
            self.module
                .define_function(shim_id, &mut context)
                .map_err(|err| {
                    codegen_ice(format!("failed to define drop shim '{name}': {err:?}"))
                })?;
            self.module.clear_context(&mut context);
        }

        // 3. Finalize in-memory compilation
        self.module
            .finalize_definitions()
            .map_err(|err| codegen_ice(format!("failed to finalize JIT definitions: {err:?}")))?;

        Ok(CompiledModule {
            module: self.module,
            func_ids,
        })
    }
}

/// The result of a successful JIT compilation.
///
/// Holds the finalized [`JITModule`] and the mapping from function names to
/// their [`FuncId`]s. Use [`CompiledModule::get_fn`] to obtain callable
/// function pointers.
pub struct CompiledModule {
    module: JITModule,
    func_ids: FxHashMap<String, FuncId>,
}

impl CompiledModule {
    /// Returns a callable function pointer for the named function.
    ///
    /// # Safety
    /// The caller must guarantee that the type `F` exactly matches the
    /// signature of the compiled function. Mismatched signatures cause
    /// undefined behaviour.
    pub unsafe fn get_fn<F>(&self, name: &str) -> Option<F> {
        let id = self.func_ids.get(name)?;
        let ptr = self.module.get_finalized_function(*id);
        assert_eq!(
            std::mem::size_of::<F>(),
            std::mem::size_of::<*const u8>(),
            "Type F must be the size of a function pointer"
        );
        Some(unsafe { std::mem::transmute_copy(&ptr) })
    }

    /// Returns a callable function pointer for the named function, but first checks
    /// that the full signature (types and arity) matches the expected signature.
    ///
    /// # Safety
    /// The caller must still guarantee that the type `F` matches the signature's types.
    pub unsafe fn get_fn_checked<F>(
        &self,
        name: &str,
        expected_sig: &cranelift_codegen::ir::Signature,
    ) -> Result<F, arandu_semantics::JitError> {
        let id = self
            .func_ids
            .get(name)
            .ok_or(arandu_semantics::JitError::NotFound)?;
        let decl = self.module.declarations().get_function_decl(*id);
        if decl.signature != *expected_sig {
            return Err(arandu_semantics::JitError::SignatureMismatch {
                expected: format!("{:?}", expected_sig),
                actual: format!("{:?}", decl.signature),
            });
        }

        unsafe { self.get_fn::<F>(name) }.ok_or(arandu_semantics::JitError::NotFound)
    }
}
