#![cfg(target_pointer_width = "64")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use arandu_backend_cranelift::CraneliftBackend;
use arandu_semantics::literal_pool::AmirLiteralEntry;
use arandu_semantics::{DiagCode, lower_to_amir, lower_to_hir, resolve_for_test, type_check};
use std::sync::Arc;

fn compile_src(
    src: &str,
) -> (
    arandu_semantics::amir::AmirProgram,
    arandu_semantics::SymbolTable,
    arandu_semantics::TypeInfo,
) {
    let program = arandu_parser::parse(src).expect("parse failed");
    let resolution = resolve_for_test(0, &program);
    let mut tc = type_check(resolution, &program);
    let hir = lower_to_hir(&mut tc, &program).expect("HIR lowering failed");
    let amir = lower_to_amir(&tc, &hir).expect("AMIR lowering failed");
    (
        amir,
        Arc::unwrap_or_clone(tc.symbols),
        Arc::unwrap_or_clone(tc.type_info),
    )
}

fn backend_for_test() -> CraneliftBackend {
    CraneliftBackend::try_new().expect("JIT setup should not fail in test environment")
}

#[test]
fn jit_constant_i32() {
    let src = "func main(): int { return 42; }";
    let (amir, symbols, type_info) = compile_src(src);
    let backend = backend_for_test();
    let module = backend.compile(&amir, &symbols, &type_info).unwrap();

    let result: i32 = unsafe {
        let f: unsafe fn() -> i32 = module.get_fn("main").unwrap();
        f()
    };
    assert_eq!(result, 42);
}

#[test]
fn jit_add_i32() {
    let src = "func add(a: int, b: int): int { return a + b; }";
    let (amir, symbols, type_info) = compile_src(src);
    let backend = backend_for_test();
    let module = backend.compile(&amir, &symbols, &type_info).unwrap();

    let result: i32 = unsafe {
        let f: unsafe fn(i32, i32) -> i32 = module.get_fn("add").unwrap();
        f(10, 32)
    };
    assert_eq!(result, 42);
}

#[test]
fn jit_control_flow() {
    let src = r#"
    func max(a: int, b: int): int {
        let mut res = 0
        if a > b {
            res = a
        } else {
            res = b
        }
        return res
    }
    "#;
    let (amir, symbols, type_info) = compile_src(src);
    let backend = backend_for_test();
    let module = backend.compile(&amir, &symbols, &type_info).unwrap();

    let result: i32 = unsafe {
        let f: unsafe fn(i32, i32) -> i32 = module.get_fn("max").unwrap();
        f(10, 20)
    };
    assert_eq!(result, 20);

    let result2: i32 = unsafe {
        let f: unsafe fn(i32, i32) -> i32 = module.get_fn("max").unwrap();
        f(42, 7)
    };
    assert_eq!(result2, 42);
}

#[test]
fn jit_unsigned_comparison() {
    let src = r#"
    func is_gt(a: u32, b: u32): bool {
        return a > b;
    }
    "#;
    let (amir, symbols, type_info) = compile_src(src);
    let backend = backend_for_test();
    let module = backend.compile(&amir, &symbols, &type_info).unwrap();

    let result: bool = unsafe {
        let f: unsafe fn(u32, u32) -> bool = module.get_fn("is_gt").unwrap();
        f(4294967295, 0)
    };
    assert!(result);
}

#[test]
fn jit_unsigned_div() {
    let src = r#"
    func half(a: u32): u32 {
        return a / 2;
    }
    "#;
    let (amir, symbols, type_info) = compile_src(src);
    let backend = backend_for_test();
    let module = backend.compile(&amir, &symbols, &type_info).unwrap();

    let result: u32 = unsafe {
        let f: unsafe fn(u32) -> u32 = module.get_fn("half").unwrap();
        f(4_294_967_295)
    };
    // u32::MAX / 2 = 2_147_483_647; signed interpretation (-1 / 2) would be 0.
    assert_eq!(result, 2_147_483_647);
}

#[test]
fn jit_unsigned_mod() {
    let src = r#"
    func rem(a: u32, b: u32): u32 {
        return a % b;
    }
    "#;
    let (amir, symbols, type_info) = compile_src(src);
    let backend = backend_for_test();
    let module = backend.compile(&amir, &symbols, &type_info).unwrap();

    let result: u32 = unsafe {
        let f: unsafe fn(u32, u32) -> u32 = module.get_fn("rem").unwrap();
        f(4_294_967_295, 4_294_967_294)
    };
    assert_eq!(result, 1);
}

#[test]
fn jit_unsigned_shift_right() {
    let src = r#"
    func shr(a: u32): u32 {
        return a >> 1;
    }
    "#;
    let (amir, symbols, type_info) = compile_src(src);
    let backend = backend_for_test();
    let module = backend.compile(&amir, &symbols, &type_info).unwrap();

    let result: u32 = unsafe {
        let f: unsafe fn(u32) -> u32 = module.get_fn("shr").unwrap();
        f(4_294_967_295)
    };
    // Logical shift: 0xFFFF_FFFF >> 1 = 0x7FFF_FFFF
    assert_eq!(result, 2_147_483_647);
}

#[test]
fn jit_signed_div() {
    let src = r#"
    func div(a: int, b: int): int {
        return a / b;
    }
    "#;
    let (amir, symbols, type_info) = compile_src(src);
    let backend = backend_for_test();
    let module = backend.compile(&amir, &symbols, &type_info).unwrap();

    let result: i64 = unsafe {
        let f: unsafe fn(i64, i64) -> i64 = module.get_fn("div").unwrap();
        f(-1, 2)
    };
    assert_eq!(result, 0);
}

#[test]
fn jit_signed_mod() {
    let src = r#"
    func rem(a: int, b: int): int {
        return a % b;
    }
    "#;
    let (amir, symbols, type_info) = compile_src(src);
    let backend = backend_for_test();
    let module = backend.compile(&amir, &symbols, &type_info).unwrap();

    let result: i64 = unsafe {
        let f: unsafe fn(i64, i64) -> i64 = module.get_fn("rem").unwrap();
        f(-7, 3)
    };
    assert_eq!(result, -1);
}

#[test]
fn jit_signed_comparison() {
    let src = r#"
    func is_gt(a: int, b: int): bool {
        return a > b;
    }
    "#;
    let (amir, symbols, type_info) = compile_src(src);
    let backend = backend_for_test();
    let module = backend.compile(&amir, &symbols, &type_info).unwrap();

    let result: bool = unsafe {
        let f: unsafe fn(i64, i64) -> bool = module.get_fn("is_gt").unwrap();
        f(-1, 0)
    };
    assert!(!result);
}

#[test]
fn jit_signed_shift_right() {
    let src = r#"
    func shr(a: int): int {
        return a >> 1;
    }
    "#;
    let (amir, symbols, type_info) = compile_src(src);
    let backend = backend_for_test();
    let module = backend.compile(&amir, &symbols, &type_info).unwrap();

    let result: i64 = unsafe {
        let f: unsafe fn(i64) -> i64 = module.get_fn("shr").unwrap();
        f(-1)
    };
    // Arithmetic shift: -1 >> 1 = -1
    assert_eq!(result, -1);
}

#[test]
fn jit_float_add() {
    let src = "func add(a: float, b: float): float { return a + b; }";
    let (amir, symbols, type_info) = compile_src(src);
    let backend = backend_for_test();
    let module = backend.compile(&amir, &symbols, &type_info).unwrap();
    let result: f64 = unsafe {
        let f: unsafe fn(f64, f64) -> f64 = module.get_fn("add").unwrap();
        f(1.5, 2.5)
    };
    assert_eq!(result, 4.0);
}

#[test]
fn jit_float_mul() {
    let src = "func mul(a: float, b: float): float { return a * b; }";
    let (amir, symbols, type_info) = compile_src(src);
    let backend = backend_for_test();
    let module = backend.compile(&amir, &symbols, &type_info).unwrap();
    let result: f64 = unsafe {
        let f: unsafe fn(f64, f64) -> f64 = module.get_fn("mul").unwrap();
        f(3.0, 1.5)
    };
    assert_eq!(result, 4.5);
}

#[test]
fn jit_float_compare() {
    let src = "func is_gt(a: float, b: float): bool { return a > b; }";
    let (amir, symbols, type_info) = compile_src(src);
    let backend = backend_for_test();
    let module = backend.compile(&amir, &symbols, &type_info).unwrap();
    let result: bool = unsafe {
        let f: unsafe fn(f64, f64) -> bool = module.get_fn("is_gt").unwrap();
        f(3.0, 2.0)
    };
    assert!(result);
    let result: bool = unsafe {
        let f: unsafe fn(f64, f64) -> bool = module.get_fn("is_gt").unwrap();
        f(2.0, 3.0)
    };
    assert!(!result);
}

#[test]
fn jit_cross_function_call() {
    let src = r#"
    func helper(): int {
        return 42;
    }
    func main(): int {
        return helper();
    }
    "#;
    let (amir, symbols, type_info) = compile_src(src);
    let backend = backend_for_test();
    let module = backend.compile(&amir, &symbols, &type_info).unwrap();
    let result: i32 = unsafe {
        let f: unsafe fn() -> i32 = module.get_fn("main").unwrap();
        f()
    };
    assert_eq!(result, 42);
}

#[test]
fn jit_string_literal() {
    let src = r#"func hello(): str { return "hello jit"; }"#;
    let (amir, symbols, type_info) = compile_src(src);
    let backend = backend_for_test();
    let module = backend.compile(&amir, &symbols, &type_info).unwrap();
    let result: *const u8 = unsafe {
        let f: unsafe fn() -> *const u8 = module.get_fn("hello").unwrap();
        f()
    };
    assert!(!result.is_null());
}

#[test]
fn jit_string_interpolation() {
    // Verifies StringInterp lowering + Cranelift concat (malloc/memcpy).
    // Exercises both `${name}` and `$name` forms without relying on fat-pointer
    // return ABI details beyond "program runs and returns 0".
    let src = r#"
    func main(): int {
        let name = "Bruno"
        let a = "Oi, ${name}"
        let b = "Oi, $name"
        return 0
    }
    "#;
    let (amir, symbols, type_info) = compile_src(src);
    let backend = backend_for_test();
    let module = backend
        .compile(&amir, &symbols, &type_info)
        .expect("string interpolation should compile in Cranelift JIT");

    let code: i32 = unsafe {
        let f: unsafe fn() -> i32 = module.get_fn("main").unwrap();
        f()
    };
    assert_eq!(code, 0);
}

#[test]
fn jit_to_str_int_interp() {
    // ToStr v0.1: int in interpolation → host helper + StringInterp.
    let src = r#"
    func main(): int {
        let n: int = 42
        let s = "n=${n}"
        return 0
    }
    "#;
    let (amir, symbols, type_info) = compile_src(src);
    let backend = backend_for_test();
    let module = backend
        .compile(&amir, &symbols, &type_info)
        .expect("ToStr int interp should compile in Cranelift JIT");

    let code: i32 = unsafe {
        let f: unsafe fn() -> i32 = module.get_fn("main").unwrap();
        f()
    };
    assert_eq!(code, 0);
}

#[test]
fn jit_to_str_bool_and_float() {
    let src = r#"
    func main(): int {
        let a = "b=${true}"
        let b = "f=${1.5}"
        return 0
    }
    "#;
    let (amir, symbols, type_info) = compile_src(src);
    let backend = backend_for_test();
    let module = backend
        .compile(&amir, &symbols, &type_info)
        .expect("ToStr bool/float interp should compile");

    let code: i32 = unsafe {
        let f: unsafe fn() -> i32 = module.get_fn("main").unwrap();
        f()
    };
    assert_eq!(code, 0);
}

#[test]
fn jit_io_println_to_str() {
    // Prelude io.println + ToStr of int (does not assert stdout; just exit 0).
    let src = r#"
    import io
    func main(): int {
        io.println(42)
        io.println("x=${1}")
        return 0
    }
    "#;
    let (amir, symbols, type_info) = compile_src(src);
    let backend = backend_for_test();
    let module = backend
        .compile(&amir, &symbols, &type_info)
        .expect("io.println + ToStr should compile");

    let code: i32 = unsafe {
        let f: unsafe fn() -> i32 = module.get_fn("main").unwrap();
        f()
    };
    assert_eq!(code, 0);
}

#[test]
fn jit_to_str_method_and_matrix() {
    let src = r#"
    import io
    func main(): int {
        let n: int = 42
        let b: bool = false
        let c: char = 'A'
        let u: uint = 3
        let f: float = 2.5
        let i8v: i8 = -1
        let u32v: u32 = 8
        let s = n.to_str()
        io.println(s)
        io.println(b.to_str())
        io.println(c)
        io.println(u)
        io.println(f)
        io.println(i8v)
        io.println(u32v)
        io.println("m=${b}|${n}|${f}")
        return 0
    }
    "#;
    let (amir, symbols, type_info) = compile_src(src);
    let backend = backend_for_test();
    let module = backend
        .compile(&amir, &symbols, &type_info)
        .expect("to_str method matrix should compile");

    let code: i32 = unsafe {
        let f: unsafe fn() -> i32 = module.get_fn("main").unwrap();
        f()
    };
    assert_eq!(code, 0);
}

#[test]
fn format_f64_v01_specials_and_integers() {
    use arandu_backend_cranelift::to_str_runtime::format_f64_v01;
    assert_eq!(format_f64_v01(2.0), "2");
    assert_eq!(format_f64_v01(-3.0), "-3");
    assert_eq!(format_f64_v01(f64::NAN), "nan");
    assert_eq!(format_f64_v01(f64::INFINITY), "inf");
    assert_eq!(format_f64_v01(f64::NEG_INFINITY), "-inf");
    assert!(format_f64_v01(1.5).starts_with("1.5"));
}

/// A3.6: host pending_once then block_on yields Ready(payload).
#[test]
fn jit_a3_6_pending_once_block_on() {
    unsafe {
        let s = arandu_backend_cranelift::poll_runtime::ar_co_pending_once_i64(42);
        let mut out = 0i64;
        assert_eq!(
            arandu_backend_cranelift::poll_runtime::ar_co_poll_i64(s, &mut out),
            1,
            "first poll Pending"
        );
        assert_eq!(
            arandu_backend_cranelift::poll_runtime::ar_co_poll_i64(s, &mut out),
            0
        );
        assert_eq!(out, 42);
        let s2 = arandu_backend_cranelift::poll_runtime::ar_co_pending_once_i64(7);
        assert_eq!(
            arandu_backend_cranelift::poll_runtime::ar_co_block_on_i64(s2),
            7
        );
    }
}

/// A3.6: await through CoroutineReady (disc=0 fast path) still returns 42.
#[test]
fn jit_a3_6_await_ready_layout() {
    let src = r#"
    func main(): int {
        let x = async { 42 }
        return await x
    }
    "#;
    let (amir, symbols, type_info) = compile_src(src);
    let backend = backend_for_test();
    let module = backend.compile(&amir, &symbols, &type_info).unwrap();
    let result: i32 = unsafe {
        let f: unsafe fn() -> i32 = module.get_fn("main").unwrap();
        f()
    };
    assert_eq!(result, 42);
}

/// A3.3: async block → stack-ready coroutine + await.
#[test]
fn jit_a3_3_stack_async_block() {
    let src = r#"
    func main(): int {
        let x = async { 42 }
        return await x
    }
    "#;
    let (amir, symbols, type_info) = compile_src(src);
    let has_stack = amir.funcs.iter().any(|f| {
        f.blocks.iter().any(|b| {
            for stmt in f.block_stmts(b.id) {
                if let arandu_semantics::amir::AmirStmt::Assign {
                    rhs: arandu_semantics::amir::AmirRvalue::CoroutineReady { stack: true, .. },
                    ..
                } = stmt
                {
                    return true;
                }
            }
            false
        })
    });
    assert!(has_stack, "expected CoroutineReady {{ stack: true }}");
    let backend = backend_for_test();
    let module = backend.compile(&amir, &symbols, &type_info).unwrap();
    let result: i32 = unsafe {
        let f: unsafe fn() -> i32 = module.get_fn("main").unwrap();
        f()
    };
    assert_eq!(result, 42);
}

/// A3.1: suspend split inside async func (ready-only drive still returns 11).
#[test]
fn jit_a3_1_suspend_split() {
    let src = r#"
    async func inner(): int {
        return 1
    }
    async func outer(): int {
        let a = 10
        let b = await inner()
        return a + b
    }
    func main(): int {
        return await outer()
    }
    "#;
    let (amir, symbols, type_info) = compile_src(src);
    assert!(
        amir.funcs.iter().any(|f| {
            f.blocks.iter().any(|b| {
                matches!(
                    b.terminator,
                    arandu_semantics::amir::AmirTerminator::Suspend { .. }
                )
            })
        }),
        "expected Suspend terminator in outer"
    );
    let backend = backend_for_test();
    let module = backend.compile(&amir, &symbols, &type_info).unwrap();
    let result: i32 = unsafe {
        let f: unsafe fn() -> i32 = module.get_fn("main").unwrap();
        f()
    };
    assert_eq!(result, 11);
}

/// A3.0: async block + await (ready coroutine).
#[test]
fn jit_a3_async_block_await() {
    let src = r#"
    func main(): int {
        let x = async { 42 }
        return await x
    }
    "#;
    let (amir, symbols, type_info) = compile_src(src);
    let backend = backend_for_test();
    let module = backend.compile(&amir, &symbols, &type_info).unwrap();
    let result: i32 = unsafe {
        let f: unsafe fn() -> i32 = module.get_fn("main").unwrap();
        f()
    };
    assert_eq!(result, 42);
}

/// A3.0: async func return sugar + await.
#[test]
fn jit_a3_async_func_await() {
    let src = r#"
    async func answer(): int {
        return 7
    }
    func main(): int {
        return await answer()
    }
    "#;
    let (amir, symbols, type_info) = compile_src(src);
    let backend = backend_for_test();
    let module = backend.compile(&amir, &symbols, &type_info).unwrap();
    let result: i32 = unsafe {
        let f: unsafe fn() -> i32 = module.get_fn("main").unwrap();
        f()
    };
    assert_eq!(result, 7);
}

/// BC.4a: `&*p` reborrow through `AmirProjection::Deref` (use_var of pointer local).
#[test]
fn jit_bc4a_reborrow_deref() {
    let src = r#"
    func main(): int {
        let n = 42
        let p = &n
        let q = &*p
        return *q
    }
    "#;
    let (amir, symbols, type_info) = compile_src(src);
    let backend = backend_for_test();
    let module = backend.compile(&amir, &symbols, &type_info).unwrap();
    let result: i32 = unsafe {
        let f: unsafe fn() -> i32 = module.get_fn("main").unwrap();
        f()
    };
    assert_eq!(result, 42);
}

/// BC.4a: field borrow of materialised object (`&p.x`).
#[test]
fn jit_bc4a_field_borrow() {
    let src = r#"
    struct Point {
        x: int
        y: int
    }
    func main(): int {
        let p = Point { x: 11, y: 22 }
        let r = &p.x
        return *r
    }
    "#;
    let (amir, symbols, type_info) = compile_src(src);
    let backend = backend_for_test();
    let module = backend.compile(&amir, &symbols, &type_info).unwrap();
    let result: i32 = unsafe {
        let f: unsafe fn() -> i32 = module.get_fn("main").unwrap();
        f()
    };
    assert_eq!(result, 11);
}

#[test]
fn jit_struct_field_access() {
    let src = r#"
    struct Point {
        x: int
        y: int
    }
    func get_x(p: Point): int {
        return p.x
    }
    func get_y(p: Point): int {
        return p.y
    }
    "#;
    let (amir, symbols, type_info) = compile_src(src);
    let backend = backend_for_test();
    let module = backend.compile(&amir, &symbols, &type_info).unwrap();
    #[repr(C)]
    struct Point {
        x: i64,
        y: i64,
    }
    let p = Point { x: 10, y: 20 };
    let result: i32 = unsafe {
        let f: unsafe fn(*const Point) -> i32 = module.get_fn("get_x").unwrap();
        f(&p as *const Point)
    };
    assert_eq!(result, 10);
    let result: i32 = unsafe {
        let f: unsafe fn(*const Point) -> i32 = module.get_fn("get_y").unwrap();
        f(&p as *const Point)
    };
    assert_eq!(result, 20);
}

#[test]
fn jit_sub_i32() {
    let src = "func sub(a: int, b: int): int { return a - b; }";
    let (amir, symbols, type_info) = compile_src(src);
    let backend = backend_for_test();
    let module = backend.compile(&amir, &symbols, &type_info).unwrap();
    let result: i32 = unsafe {
        let f: unsafe fn(i32, i32) -> i32 = module.get_fn("sub").unwrap();
        f(10, 3)
    };
    assert_eq!(result, 7);
}

#[test]
fn jit_neg_i32() {
    let src = "func neg(a: int): int { return -a; }";
    let (amir, symbols, type_info) = compile_src(src);
    let backend = backend_for_test();
    let module = backend.compile(&amir, &symbols, &type_info).unwrap();
    let result: i32 = unsafe {
        let f: unsafe fn(i32) -> i32 = module.get_fn("neg").unwrap();
        f(42)
    };
    assert_eq!(result, -42);
}

#[test]
fn jit_equality() {
    let src = r#"
    func eq(a: int, b: int): bool { return a == b; }
    func neq(a: int, b: int): bool { return a != b; }
    "#;
    let (amir, symbols, type_info) = compile_src(src);
    let backend = backend_for_test();
    let module = backend.compile(&amir, &symbols, &type_info).unwrap();
    let result: bool = unsafe {
        let f: unsafe fn(i32, i32) -> bool = module.get_fn("eq").unwrap();
        f(3, 3)
    };
    assert!(result);
    let result: bool = unsafe {
        let f: unsafe fn(i32, i32) -> bool = module.get_fn("eq").unwrap();
        f(3, 4)
    };
    assert!(!result);
    let result: bool = unsafe {
        let f: unsafe fn(i32, i32) -> bool = module.get_fn("neq").unwrap();
        f(3, 4)
    };
    assert!(result);
}

#[test]
fn jit_less_than() {
    let src = r#"
    func lt(a: int, b: int): bool { return a < b; }
    func lte(a: int, b: int): bool { return a <= b; }
    "#;
    let (amir, symbols, type_info) = compile_src(src);
    let backend = backend_for_test();
    let module = backend.compile(&amir, &symbols, &type_info).unwrap();
    let result: bool = unsafe {
        let f: unsafe fn(i32, i32) -> bool = module.get_fn("lt").unwrap();
        f(2, 3)
    };
    assert!(result);
    let result: bool = unsafe {
        let f: unsafe fn(i32, i32) -> bool = module.get_fn("lt").unwrap();
        f(3, 2)
    };
    assert!(!result);
    let result: bool = unsafe {
        let f: unsafe fn(i32, i32) -> bool = module.get_fn("lte").unwrap();
        f(3, 3)
    };
    assert!(result);
}

#[test]
fn jit_greater_equal() {
    let src = r#"
    func gte(a: int, b: int): bool { return a >= b; }
    "#;
    let (amir, symbols, type_info) = compile_src(src);
    let backend = backend_for_test();
    let module = backend.compile(&amir, &symbols, &type_info).unwrap();
    let result: bool = unsafe {
        let f: unsafe fn(i32, i32) -> bool = module.get_fn("gte").unwrap();
        f(5, 3)
    };
    assert!(result);
    let result: bool = unsafe {
        let f: unsafe fn(i32, i32) -> bool = module.get_fn("gte").unwrap();
        f(3, 5)
    };
    assert!(!result);
    let result: bool = unsafe {
        let f: unsafe fn(i32, i32) -> bool = module.get_fn("gte").unwrap();
        f(3, 3)
    };
    assert!(result);
}

#[test]
fn jit_logical_not() {
    let src = r#"
    func not(a: bool): bool { return !a; }
    "#;
    let (amir, symbols, type_info) = compile_src(src);
    let backend = backend_for_test();
    let module = backend.compile(&amir, &symbols, &type_info).unwrap();
    let result: bool = unsafe {
        let f: unsafe fn(bool) -> bool = module.get_fn("not").unwrap();
        f(true)
    };
    assert!(!result);
    let result: bool = unsafe {
        let f: unsafe fn(bool) -> bool = module.get_fn("not").unwrap();
        f(false)
    };
    assert!(result);
}

#[test]
fn jit_logical_or_and() {
    let src = r#"
    func or(a: bool, b: bool): bool { return a || b; }
    func and(a: bool, b: bool): bool { return a && b; }
    "#;
    let (amir, symbols, type_info) = compile_src(src);
    let backend = backend_for_test();
    let module = backend.compile(&amir, &symbols, &type_info).unwrap();
    let result: bool = unsafe {
        let f: unsafe fn(bool, bool) -> bool = module.get_fn("or").unwrap();
        f(true, false)
    };
    assert!(result);
    let result: bool = unsafe {
        let f: unsafe fn(bool, bool) -> bool = module.get_fn("or").unwrap();
        f(false, false)
    };
    assert!(!result);
    let result: bool = unsafe {
        let f: unsafe fn(bool, bool) -> bool = module.get_fn("and").unwrap();
        f(true, true)
    };
    assert!(result);
    let result: bool = unsafe {
        let f: unsafe fn(bool, bool) -> bool = module.get_fn("and").unwrap();
        f(true, false)
    };
    assert!(!result);
}

#[test]
fn jit_bitwise_and_or_xor() {
    let src = r#"
    func band(a: int, b: int): int { return a & b; }
    func bor(a: int, b: int): int { return a | b; }
    func bxor(a: int, b: int): int { return a ^ b; }
    "#;
    let (amir, symbols, type_info) = compile_src(src);
    let backend = backend_for_test();
    let module = backend.compile(&amir, &symbols, &type_info).unwrap();
    let result: i32 = unsafe {
        let f: unsafe fn(i32, i32) -> i32 = module.get_fn("band").unwrap();
        f(0xFF, 0x0F)
    };
    assert_eq!(result, 0x0F);
    let result: i32 = unsafe {
        let f: unsafe fn(i32, i32) -> i32 = module.get_fn("bor").unwrap();
        f(0xF0, 0x0F)
    };
    assert_eq!(result, 0xFF);
    let result: i32 = unsafe {
        let f: unsafe fn(i32, i32) -> i32 = module.get_fn("bxor").unwrap();
        f(0xFF, 0x0F)
    };
    assert_eq!(result, 0xF0);
}

#[test]
fn jit_bitwise_not() {
    let src = "func bnot(a: int): int { return ~a; }";
    let (amir, symbols, type_info) = compile_src(src);
    let backend = backend_for_test();
    let module = backend.compile(&amir, &symbols, &type_info).unwrap();
    let result: i32 = unsafe {
        let f: unsafe fn(i32) -> i32 = module.get_fn("bnot").unwrap();
        f(0x0F)
    };
    assert_eq!(result, !0x0F);
}

#[test]
fn jit_int_match() {
    let src = r#"
    func classify(x: int): int {
        return match x {
            1 => 10
            2 => 20
            _ => 30
        }
    }
    "#;
    let (amir, symbols, type_info) = compile_src(src);
    let backend = backend_for_test();
    let module = backend.compile(&amir, &symbols, &type_info).unwrap();
    let result: i32 = unsafe {
        let f: unsafe fn(i32) -> i32 = module.get_fn("classify").unwrap();
        f(1)
    };
    assert_eq!(result, 10);
    let result: i32 = unsafe {
        let f: unsafe fn(i32) -> i32 = module.get_fn("classify").unwrap();
        f(2)
    };
    assert_eq!(result, 20);
    let result: i32 = unsafe {
        let f: unsafe fn(i32) -> i32 = module.get_fn("classify").unwrap();
        f(99)
    };
    assert_eq!(result, 30);
}

#[test]
fn jit_enum_match() {
    let src = r#"
    enum Color {
        Red,
        Green,
        Blue,
    }
    func pick(c: Color): int {
        return match c {
            Color.Red => 1
            Color.Green => 2
            Color.Blue => 3
        }
    }
    func test_red(): int {
        return pick(Color.Red);
    }
    func test_green(): int {
        return pick(Color.Green);
    }
    func test_blue(): int {
        return pick(Color.Blue);
    }
    "#;
    let (amir, symbols, type_info) = compile_src(src);
    let backend = backend_for_test();
    let module = backend.compile(&amir, &symbols, &type_info).unwrap();
    let result_red: i32 = unsafe {
        let f: unsafe fn() -> i32 = module.get_fn("test_red").unwrap();
        f()
    };
    assert_eq!(result_red, 1);
    let result_green: i32 = unsafe {
        let f: unsafe fn() -> i32 = module.get_fn("test_green").unwrap();
        f()
    };
    assert_eq!(result_green, 2);
    let result_blue: i32 = unsafe {
        let f: unsafe fn() -> i32 = module.get_fn("test_blue").unwrap();
        f()
    };
    assert_eq!(result_blue, 3);
}

#[test]
fn jit_enum_none_payload_never_read() {
    let src = r#"
    enum MaybeInt {
        None,
        Some(int),
    }
    func get_value(m: MaybeInt): int {
        return match m {
            MaybeInt.None => 0
            MaybeInt.Some(val) => val
        }
    }
    func run_loop(n: int): int {
        let mut i = 0;
        let mut sum = 0;
        while i < n {
            let m = MaybeInt.None;
            sum = sum + get_value(m);
            i = i + 1;
        }
        return sum;
    }
    "#;
    let (amir, symbols, type_info) = compile_src(src);
    let backend = backend_for_test();
    let module = backend.compile(&amir, &symbols, &type_info).unwrap();
    let result: i32 = unsafe {
        let f: unsafe fn(i32) -> i32 = module.get_fn("run_loop").unwrap();
        f(1000)
    };
    assert_eq!(result, 0);
}

#[test]
fn jit_enum_int_payload_uses_pointer_width() {
    let src = r#"
    enum Number {
        Value(int),
    }
    func main(): int {
        return match Number.Value(4294967303) {
            Number.Value(value) => value
        }
    }
    "#;
    let (amir, symbols, type_info) = compile_src(src);
    let backend = backend_for_test();
    let module = backend.compile(&amir, &symbols, &type_info).unwrap();
    let result: i64 = unsafe {
        let f: unsafe fn() -> i64 = module.get_fn("main").unwrap();
        f()
    };
    assert_eq!(result, 4_294_967_303);
}

#[test]
fn jit_tuple() {
    let src = r#"
    func pair(): (int, bool) {
        return 42, true;
    }
    func get_first(): int {
        let x, y = pair();
        return x;
    }
    func get_second(): bool {
        let x, y = pair();
        return y;
    }
    "#;
    let (amir, symbols, type_info) = compile_src(src);
    let backend = backend_for_test();
    let module = backend.compile(&amir, &symbols, &type_info).unwrap();
    let first: i32 = unsafe {
        let f: unsafe fn() -> i32 = module.get_fn("get_first").unwrap();
        f()
    };
    assert_eq!(first, 42);
    let second: bool = unsafe {
        let f: unsafe fn() -> bool = module.get_fn("get_second").unwrap();
        f()
    };
    assert!(second);
}

#[test]
fn jit_struct_literal() {
    let src = r#"
    struct Point {
        x: int
        y: int
    }
    func get_sum(): int {
        let p = Point { x: 10, y: 20 };
        return p.x + p.y;
    }
    "#;
    let (amir, symbols, type_info) = compile_src(src);
    let backend = backend_for_test();
    let module = backend.compile(&amir, &symbols, &type_info).unwrap();
    let sum: i32 = unsafe {
        let f: unsafe fn() -> i32 = module.get_fn("get_sum").unwrap();
        f()
    };
    assert_eq!(sum, 30);
}

#[test]
fn jit_returns_ice_on_invalid_literal_pool() {
    let (mut amir, symbols, type_info) = compile_src("func main(): int { return 42; }");
    for entry in &mut amir.literal_pool.entries {
        if let AmirLiteralEntry::Int(value) = entry {
            *value = "not_an_int".into();
            break;
        }
    }

    let backend = backend_for_test();
    let err = match backend.compile(&amir, &symbols, &type_info) {
        Err(err) => err,
        Ok(_) => panic!("expected codegen ICE for invalid literal pool"),
    };
    assert_eq!(err.code, DiagCode::ICEGEN001);
    assert!(
        err.message.contains("invalid integer literal"),
        "unexpected ICE message: {}",
        err.message
    );
}

/// Regression test: two enums sharing a variant name must not collide
/// on their discriminant tags.
///
/// ## What this guards against
///
/// The variant resolution fallback used to scan `enum_variant_tags` globally
/// by name — so `Color.Red` and `Status.Red` could silently resolve to
/// whichever `SymbolId` the hashmap returned first (non-deterministic with
/// standard HashMap; consistent-but-wrong with FxHashMap since it does not
/// randomize its seed per process, so a collision would mask itself in CI).
///
/// The fix registers both the definition-site SymbolId ("Red") *and* the
/// associated-member SymbolId ("Color.Red") in `enum_variant_tags` during
/// `collect_type_shapes`, so the direct `.get(symbol)` hit never falls
/// through to the name-based global scan.
///
/// ## Why this test is deterministic
///
/// Rather than relying on iteration order to expose the bug, the test
/// encodes the expected discriminant as the JIT return value and asserts
/// it numerically.  A regression produces tag 1 (the wrong variant) instead
/// of 0, failing the assert regardless of hashmap ordering.
#[test]
fn jit_enum_cross_variant_name_no_collision() {
    let src = r#"
        enum Color  { Red, Green }
        enum Status { Yellow, Red }

        func color_tag() : int {
            return match Color.Red {
                Color.Red   => 0
                Color.Green => 1
            }
        }

        func status_tag() : int {
            return match Status.Red {
                Status.Yellow => 0
                Status.Red    => 1
            }
        }
    "#;
    let (amir, symbols, type_info) = compile_src(src);
    let backend = backend_for_test();
    let module = backend.compile(&amir, &symbols, &type_info).unwrap();

    let color_tag: i32 = unsafe {
        let f: unsafe fn() -> i32 = module.get_fn("color_tag").unwrap();
        f()
    };
    let status_tag: i32 = unsafe {
        let f: unsafe fn() -> i32 = module.get_fn("status_tag").unwrap();
        f()
    };

    // Color.Red is declared first in Color → match arm 0.
    // Status.Red is declared SECOND in Status (Yellow first) → match arm 1.
    // The asymmetry is intentional: if the bug regresses and Color.Red is
    // resolved using Status.Red's discriminant (1) or vice-versa (0),
    // the wrong arm fires and the assert catches it — regardless of which
    // direction the cross-enum lookup goes and regardless of hashmap ordering.
    assert_eq!(color_tag, 0, "Color.Red must match arm 0 (tag 0 in Color)");
    assert_eq!(
        status_tag, 1,
        "Status.Red must match arm 1 (tag 1 in Status, Yellow is 0)"
    );
}

#[test]
fn jit_ice_indirect_call() {
    use arandu_semantics::amir::{AmirConstant, AmirOperand, AmirStmt, InstrId};
    let src = "func main(): int { return 0; }";
    let (mut amir, symbols, type_info) = compile_src(src);

    // Convert the first statement to an indirect call.
    let id = amir.funcs[0].blocks[0]
        .statements
        .iter_ids::<InstrId>()
        .next()
        .unwrap();

    *amir.funcs[0].stmts.get_mut(id).unwrap() = AmirStmt::Call {
        lhs: None,
        callee: AmirOperand::Constant(AmirConstant::Bool(true)),
        args: Default::default(),
    };

    let backend = backend_for_test();
    let err = match backend.compile(&amir, &symbols, &type_info) {
        Err(e) => e,
        Ok(_) => panic!("should return ICE"),
    };

    assert!(
        err.message
            .contains("indirect function calls are not implemented"),
        "msg: {}",
        err.message
    );
}

#[test]
fn jit_err_new_is_non_nil_handle() {
    // `err.new` returns a non-null message handle so `e != nil` works for Result.Err.
    let src = r#"
        import err

        func fail(): Result<int, Err> {
            return Result.Err(err.new("boom"))
        }

        func ok(): Result<int, Err> {
            return Result.Ok(7)
        }

        func main(): int {
            let v = ok()?
            let _, e = fail()
            if e != nil {
                return v
            }
            return 0
        }
    "#;
    let (amir, symbols, type_info) = compile_src(src);
    let backend = backend_for_test();
    let module = backend
        .compile(&amir, &symbols, &type_info)
        .expect("err.new Result path should compile");

    let result: i32 = unsafe {
        let f: unsafe fn() -> i32 = module.get_fn("main").unwrap();
        f()
    };
    assert_eq!(result, 7, "Err handle from err.new must be non-nil");
}

#[test]
fn jit_safe_field_and_null_coalesce() {
    let src = r#"
        struct Point {
            x: int
            y: int
        }

        func readX(p: Point?): int {
            return p?.x ?? 0
        }

        func main(): int {
            let missing: Point? = nil
            let a = readX(missing)
            let p: Point? = Point { x: 3, y: 9 }
            let b = readX(p)
            return a + b
        }
    "#;
    let (amir, symbols, type_info) = compile_src(src);
    let backend = backend_for_test();
    let module = backend
        .compile(&amir, &symbols, &type_info)
        .expect("safe field should compile");

    let result: i32 = unsafe {
        let f: unsafe fn() -> i32 = module.get_fn("main").unwrap();
        f()
    };
    assert_eq!(result, 3);
}

#[test]
fn jit_nullable_int_zero_not_nil() {
    let src = r#"
        func main(): int {
            let a: int? = nil
            let b: int? = 0
            let c: int? = 5
            let x = a ?? 99
            let y = b ?? 99
            let z = c ?? 99
            return x + y * 100 + z * 10000
        }
    "#;
    let (amir, symbols, type_info) = compile_src(src);
    let backend = backend_for_test();
    let module = backend
        .compile(&amir, &symbols, &type_info)
        .expect("int? should compile");
    let result: i32 = unsafe {
        let f: unsafe fn() -> i32 = module.get_fn("main").unwrap();
        f()
    };
    assert_eq!(result, 50099, "int? = 0 must not collapse to nil");
}

#[test]
fn jit_catch_and_err_message() {
    let src = r#"
        import err
        import io

        func fail(): Result<int, Err> {
            return Result.Err(err.new("missingPath"))
        }

        func ok(): Result<int, Err> {
            return Result.Ok(7)
        }

        func main(): int {
            let a = fail() catch 3
            let b = ok() catch 0
            let _ = fail() catch |e| {
                io.println(e)
                0
            }
            return a * 10 + b
        }
    "#;
    let (amir, symbols, type_info) = compile_src(src);
    let backend = backend_for_test();
    let module = backend
        .compile(&amir, &symbols, &type_info)
        .expect("catch + err message should compile");
    let result: i32 = unsafe {
        let f: unsafe fn() -> i32 = module.get_fn("main").unwrap();
        f()
    };
    assert_eq!(result, 37);
}

#[test]
fn jit_enum_match_main() {
    let src = r#"
        enum Color {
            Red,
            Green,
            Blue,
        }

        func pick(c: Color): int {
            return match c {
                Color.Red => 1
                Color.Green => 2
                Color.Blue => 3
            }
        }

        func main(): int {
            return pick(Color.Green)
        }
    "#;
    let (amir, symbols, type_info) = compile_src(src);
    let backend = backend_for_test();
    let module = backend
        .compile(&amir, &symbols, &type_info)
        .expect("enum match should compile");

    let result: i32 = unsafe {
        let f: unsafe fn() -> i32 = module.get_fn("main").unwrap();
        f()
    };
    assert_eq!(result, 2);
}

/// F2.3.runtime: hand-built AMIR with GenInsert/GenGet roundtrip via host arena.
#[test]
fn jit_gen_insert_get_i64() {
    use arandu_base::span::Span;
    use arandu_semantics::amir::{
        AmirBasicBlock, AmirConstant, AmirFunc, AmirOperand, AmirProgram, AmirRvalue, AmirStmt,
        AmirStmtTable, AmirTemp, AmirTerminator, BlockId, GenArenaDomain, TempId,
    };
    use arandu_semantics::cfg::compute_cfg_edges;
    use arandu_semantics::layout::DenseRange;
    use arandu_semantics::literal_pool::AmirLiteralPool;
    use arandu_semantics::types::{ArType, Primitive, TypeInterner};
    use arandu_semantics::{SymbolKind, SymbolTable};

    let interner = TypeInterner::new();
    let int_ty = interner.intern(ArType::Primitive(Primitive::Int));
    let gen_ty = interner.intern(ArType::GenRef);

    let mut stmts = AmirStmtTable::new();
    // t1 = gen_insert(42)
    stmts.push(AmirStmt::Assign {
        lhs: TempId::from_usize(1),
        rhs: AmirRvalue::GenInsert {
            value: AmirOperand::Constant(AmirConstant::Pool(
                // use Nil then we need a constant - better use iconst path via Constant
                // Pool needs literal - use Constant from int via... AmirConstant only has Pool for ints
                arandu_semantics::literal_pool::LiteralId(0),
            )),
            payload_ty: int_ty,
            arena: GenArenaDomain::CompilerManaged,
            origin: Span::new(0, 0, 0),
        },
    });
    // t0 = gen_get(t1)  // return temp
    stmts.push(AmirStmt::Assign {
        lhs: TempId::from_usize(0),
        rhs: AmirRvalue::GenGet {
            gen_ref: AmirOperand::Copy(TempId::from_usize(1)),
            payload_ty: int_ty,
            arena: GenArenaDomain::CompilerManaged,
            origin: Span::new(0, 0, 0),
        },
    });
    let block = AmirBasicBlock {
        id: BlockId::from_usize(0),
        statements: DenseRange::new(0, 2),
        params: vec![],
        terminator: AmirTerminator::Return,
    };
    let blocks = vec![block];
    let cfg = compute_cfg_edges(&blocks);
    let mut pool = AmirLiteralPool::default();
    let lit = pool.intern_int("42");
    assert_eq!(lit.0, 0);

    let mut symbols = SymbolTable::new(0);
    let scope = symbols.global_scope();
    let main_sym = symbols
        .define(scope, "main", SymbolKind::Func, Span::new(0, 0, 0))
        .expect("define main");

    let func = AmirFunc {
        symbol: main_sym,
        return_type: int_ty,
        receiver: None,
        params: vec![],
        locals: vec![],
        temps: vec![
            AmirTemp {
                id: TempId::from_usize(0),
                ty: int_ty,
                is_copy: true,
                is_nullable: false,
                span: Span::new(0, 0, 0),
            },
            AmirTemp {
                id: TempId::from_usize(1),
                ty: gen_ty,
                is_copy: true,
                is_nullable: false,
                span: Span::new(0, 0, 0),
            },
        ],
        blocks,
        stmts,
        cfg,
    };
    let program = AmirProgram {
        funcs: vec![func],
        literal_pool: pool,
        extern_funcs: Default::default(),
    };
    let type_info = {
        let mut ti = arandu_semantics::TypeInfo::default();
        ti.type_interner = interner;
        ti
    };
    let backend = backend_for_test();
    let module = backend
        .compile(&program, &symbols, &type_info)
        .expect("gen JIT compile");
    let result: i64 = unsafe {
        let f: unsafe fn() -> i64 = module.get_fn("main").unwrap();
        f()
    };
    assert_eq!(result, 42);
}
