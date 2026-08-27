# Arandu Minimal 0.1 — gold examples

These programs define the **installable** language surface tracked by the
[master roadmap](../../docs/arandu-compiler-roadmap-v0.1.md).

## Contract

Every file here must pass:

```bash
arandu_cli check <file>
arandu_cli run   <file>   # exit code documented in CLI tests
```

**Do not** add examples that require experimental APIs (reactor, TCP, supervisor, dyn, …).

## Index

| File | Exit | Covers |
|------|------|--------|
| `m01_hello.aru` | 0 | prelude io, main |
| `m02_structs_enums.aru` | 3 | struct, enum, match |
| `m03_result_option.aru` | 7 | Result/Option sugar |
| `m04_generics_bounds.aru` | 10 | free-function generics (mono) |
| `m05_borrow_shared.aru` | 5 | shared self method |
| `m06_async_await.aru` | 42 | async func + await |
| `m07_async_spawn_join.aru` | 42 | std.runtime spawn/join |
| `m08_modules/main.aru` | 9 | multi-file via std (path+runtime) |
| `m09_interp_tostr.aru` | 0 | string interpolation |
| `m10_path_empty.aru` | 0 | std.path thin IN (PROMOTE-L4 join/file_name) |
| `m11_process_exit.aru` | 17 | std.process.exit host |
| `m12_time_env.aru` | 0 | std.time + std.env hosts |
| `m13_vec.aru` | 78 | std.alloc.vec (PROMOTE-L6) |
| `m14_mem_intrinsics.aru` | 46 | mem sizeOf/ptrOffset/Read/Write (L6.1) |
| `m15_vec_capacity.aru` | 21 | vec with_capacity / reserve / clear |
| `m16_gen_arena.aru` | 83 | std.alloc.gen_arena thin (insert/get/remove) |
| `m17_pod_copy.aru` | 60 | POD auto-copy named structs |
| `m18_vec_methods.aru` | 78 | Vec method-style push/get (receiver mono) |
| `m19_allocator.aru` | 112 | allocator_api thin global+bump |
| `m20_str.aru` | 0 | std.core.str concat/split_last |
| `m21_result_custom_e.aru` | 7 | Result custom E |
| `m22_iface_param.aru` | 42 | interface method via type param |
| `m23_match_result.aru` | 13 | bare Ok/Err on call scrutinee (no trailing-block swallow) |
| `m24_expect_or_abort.aru` | 13 | `Result.expectOrAbort` after `import std.core.result` |

## Default template (installer)

See `TEMPLATE_main.aru` — only IN surface.
