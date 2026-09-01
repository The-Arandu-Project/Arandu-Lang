#![cfg(target_pointer_width = "64")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use arandu_backend_cranelift::CraneliftBackend;
use arandu_middle::amir::{AmirConstant, AmirOperand, AmirProgram, AmirRvalue, AmirStmt};
use arandu_middle::layout::DataLayout;
use arandu_middle::ops::BinaryOp;
use arandu_semantics::{
    CodegenBackend, TypeCheckResult, lower_to_amir, lower_to_hir, resolve_for_test, type_check,
};
use std::env;
use std::fs;
use std::process::Command;

fn c_compiler(cc: &str) -> Command {
    let mut command = Command::new(cc);
    if env::var_os("ARANDU_C_SANITIZERS").is_some() {
        command.args([
            "-O1",
            "-g",
            "-fno-omit-frame-pointer",
            "-fsanitize=address,undefined",
        ]);
    }
    command
}

fn compile_src(src: &str) -> (AmirProgram, TypeCheckResult) {
    let program = arandu_parser::parse(src).expect("parse failed");
    let resolution = resolve_for_test(0, &program);
    let mut tc = type_check(resolution, &program);
    assert!(
        tc.diagnostics.is_empty(),
        "type check failed: {:?}",
        tc.diagnostics
    );

    let hir = lower_to_hir(&mut tc, &program).expect("HIR lowering failed");
    let amir = lower_to_amir(&tc, &hir).expect("AMIR lowering failed");
    (amir, tc)
}

fn execute_cranelift(amir: &AmirProgram, tc: &TypeCheckResult) -> i32 {
    let backend = CraneliftBackend::try_new().unwrap();
    let compiled =
        CodegenBackend::compile(backend, amir, tc.symbols.as_ref(), tc.type_info.as_ref())
            .expect("cranelift compile failed");

    unsafe {
        let main_fn =
            arandu_semantics::CompiledCode::get_fn::<unsafe fn() -> i32>(&compiled, "main")
                .expect("main not found");
        main_fn()
    }
}

fn emit_c(amir: &AmirProgram, tc: &TypeCheckResult) -> String {
    // Host parity only; Cranelift is host-only — see solidification matrix.
    arandu_backend_c::emit_c(
        amir,
        tc.symbols.as_ref(),
        tc.type_info.as_ref(),
        &tc.type_info.type_interner,
        arandu_middle::layout::DataLayout::host(),
    )
    .unwrap()
}

#[test]
fn c_backend_genref_runtime_has_monotonic_type_erased_storage() {
    let (amir, tc) = compile_src("func main(): int { return 0 }");
    let emitted = emit_c(&amir, &tc);

    assert!(emitted.contains("typedef struct ar_gen_entry"));
    assert!(emitted.contains("ar_gen_alloc_aligned"));
    assert!(emitted.contains("ar_gen_next_token == UINT64_MAX"));
    assert!(emitted.contains("ar_gen_shutdown_raw"));
    assert!(!emitted.contains("ar_gen_slots[256]"));
}

#[test]
fn c_backend_emits_opaque_black_box_barrier() {
    let (amir, tc) = compile_src(
        r#"
        func blackBox<T>(value: T): T { return value }
        func main(): int {
            return blackBox<int>(42)
        }
        "#,
    );
    let emitted = emit_c(&amir, &tc);
    assert!(emitted.contains("AR_BENCH_NOINLINE"));
    assert!(emitted.contains("ar_bench_black_box_i64((int64_t)("));
    test_execution_parity(
        "black_box_barrier",
        r#"
        func blackBox<T>(value: T): T { return value }
        func main(): int { return blackBox<int>(42) }
        "#,
    );
}

#[test]
fn explicit_destructor_runs_through_both_backend_pipelines() {
    test_execution_parity(
        "destructor_epilogue",
        r#"
struct Resource { handle: ptr[u8] }

@Destructor
func Resource.close(own self): void {}

func main(): int {
    let resource = Resource { handle: nil }
    return 0
}
"#,
    );
}

#[test]
fn c_backend_genref_runtime_executes_beyond_legacy_capacity() {
    let (amir, tc) = compile_src("func main(): int { return 0 }");
    let emitted = emit_c(&amir, &tc);
    let source = format!(
        "#define main arandu_unused_main\n{emitted}\n#undef main\n{}",
        r#"
typedef struct __attribute__((aligned(64))) { uint64_t words[8]; } AlignedProbe;
static int probe_drops = 0;
static void drop_probe(void *payload) {
    AlignedProbe *probe = (AlignedProbe *)payload;
    if (((uintptr_t)probe & 63U) != 0) abort();
    ++probe_drops;
    if (probe_drops == 1) ar_gen_shutdown_raw();
}

int main(void) {
    uint64_t handles[1024];
    for (int64_t i = 0; i < 1024; ++i) {
        int64_t value = i + 7;
        handles[i] = ar_gen_upsert_raw(0, &value, sizeof(value), _Alignof(int64_t), NULL);
    }
    for (int64_t i = 0; i < 1024; ++i) {
        int64_t value = 0;
        if (!ar_gen_get_raw(handles[i], &value, sizeof(value), _Alignof(int64_t)) || value != i + 7) return 1;
        value = i + 9;
        if (!ar_gen_set_raw(handles[i], &value, sizeof(value), _Alignof(int64_t), NULL)) return 2;
        value = 0;
        if (!ar_gen_get_raw(handles[i], &value, sizeof(value), _Alignof(int64_t)) || value != i + 9) return 3;
    }
    for (int64_t i = 0; i < 1024; ++i) {
        int64_t value = 0;
        if (!ar_gen_remove_raw(handles[i], &value, sizeof(value), _Alignof(int64_t)) || value != i + 9) return 4;
        if (ar_gen_get_raw(handles[i], &value, sizeof(value), _Alignof(int64_t))) return 5;
    }
    for (int64_t i = 0; i < 1024; ++i) {
        int64_t value = i + 11;
        uint64_t next = ar_gen_insert_raw(&value, sizeof(value), _Alignof(int64_t), NULL);
        if (next == 0 || next <= handles[1023]) return 6;
    }
    ar_gen_shutdown_raw();
    AlignedProbe first = {{1}};
    AlignedProbe second = {{2}};
    if (!ar_gen_insert_raw(&first, sizeof(first), _Alignof(AlignedProbe), drop_probe)) return 7;
    if (!ar_gen_insert_raw(&second, sizeof(second), _Alignof(AlignedProbe), drop_probe)) return 8;
    ar_gen_shutdown_raw();
    if (probe_drops != 2) return 9;
    return 0;
}
"#
    );
    let out_dir = env::temp_dir().join("arandu_c_tests");
    fs::create_dir_all(&out_dir).unwrap();
    let c_file = out_dir.join("genref_dynamic_capacity.c");
    let exe_file = out_dir.join("genref_dynamic_capacity.exe");
    fs::write(&c_file, source).unwrap();
    let cc = env::var("CC").unwrap_or_else(|_| "gcc".to_string());
    let compiled = c_compiler(&cc)
        .arg(&c_file)
        .arg("-o")
        .arg(&exe_file)
        .status()
        .unwrap_or_else(|_| panic!("failed to invoke C compiler '{cc}'"));
    assert!(
        compiled.success(),
        "C GenRef stress fixture did not compile"
    );
    let status = Command::new(&exe_file)
        .status()
        .expect("failed to run C GenRef stress fixture");
    assert!(status.success(), "C GenRef stress fixture failed: {status}");
}

fn assert_backend_rejection_parity(
    amir: &AmirProgram,
    tc: &TypeCheckResult,
    expected_marker: &str,
) {
    let c_error = arandu_backend_c::emit_c(
        amir,
        tc.symbols.as_ref(),
        tc.type_info.as_ref(),
        &tc.type_info.type_interner,
        DataLayout::host(),
    )
    .unwrap_err();
    let jit_error = CraneliftBackend::try_new()
        .unwrap()
        .compile(amir, tc.symbols.as_ref(), tc.type_info.as_ref())
        .err()
        .expect("Cranelift must reject malformed AMIR before producing a module");

    assert_eq!(c_error.code, arandu_middle::DiagCode::ICEGEN002);
    assert_eq!(jit_error.code, c_error.code);
    assert_eq!(jit_error.message, c_error.message);
    assert!(c_error.message.contains(expected_marker));
}

fn test_execution_parity(name: &str, src: &str) {
    let (amir, tc) = compile_src(src);

    // 1. Generate C (no debug dumps — keep tests pure / CI-friendly).
    let mut c_code = emit_c(&amir, &tc);

    // CEmitter emits `int32_t main(void)`. We rename it to `arandu_main` via a preprocessor
    // macro so we can wrap it in a standard C `main` that captures and prints the return
    // value for parity comparison with the Cranelift result.
    c_code = format!("#define main arandu_main\n{}\n#undef main\n", c_code);
    c_code.push_str(
        r#"
#include <stdio.h>
int main() {
    int32_t res = arandu_main();
    printf("%d\n", res);
    return 0;
}
"#,
    );

    let out_dir = env::temp_dir().join("arandu_c_tests");
    fs::create_dir_all(&out_dir).unwrap();
    let c_file = out_dir.join(format!("{}.c", name));
    let exe_file = out_dir.join(format!("{}.exe", name)); // .exe works on windows

    fs::write(&c_file, c_code).unwrap();

    // Compiler selection: use $CC env var if set, otherwise fallback to gcc.
    let cc = env::var("CC").unwrap_or_else(|_| "gcc".to_string());

    // `-lm` for ToStr float helpers (`isnan`/`isinf` via math.h).
    let compile_status = c_compiler(&cc)
        .arg(&c_file)
        .arg("-o")
        .arg(&exe_file)
        .arg("-lm")
        .status()
        .unwrap_or_else(|_| {
            panic!(
                "failed to invoke C compiler '{}'. Parity tests require a C compiler in PATH.",
                cc
            )
        });

    assert!(
        compile_status.success(),
        "C compilation failed for {}",
        name
    );

    let output = Command::new(&exe_file)
        .output()
        .expect("failed to run compiled executable");

    assert!(output.status.success(), "C program crashed for {}", name);

    // Last line is the harness exit code (`printf("%d\n", res)`). Earlier lines
    // may be `io.println` output (ToStr product path).
    let stdout = String::from_utf8(output.stdout).unwrap();
    let last_line = stdout
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim();
    let actual_result: i32 = last_line
        .parse()
        .unwrap_or_else(|_| panic!("failed to parse C exit line as integer: {stdout:?}"));

    // 2. Run via Cranelift
    let expected = execute_cranelift(&amir, &tc);

    assert_eq!(
        expected, actual_result,
        "Execution mismatch for {}! Cranelift={}, C={}",
        name, expected, actual_result
    );
}

#[test]
fn generated_test_registry_entrypoint_compiles_and_executes() {
    let (amir, tc) = compile_src("func smoke(): void {}");
    let mut source = emit_c(&amir, &tc);
    let mut registry = arandu_codegen::testing::TestRegistry::default();
    registry.insert(arandu_codegen::testing::TestEntry {
        id: "sample::test::smoke::smoke".into(),
        function: "smoke".into(),
    });
    source.push_str(&registry.emit_c_entrypoint());

    let out_dir = env::temp_dir().join("arandu_c_tests");
    fs::create_dir_all(&out_dir).unwrap();
    let c_file = out_dir.join("generated_test_harness.c");
    let exe_file = out_dir.join("generated_test_harness.exe");
    fs::write(&c_file, source).unwrap();

    let cc = env::var("CC").unwrap_or_else(|_| "gcc".to_string());
    let compiled = c_compiler(&cc)
        .arg(&c_file)
        .arg("-o")
        .arg(&exe_file)
        .arg("-lm")
        .status()
        .unwrap_or_else(|_| panic!("failed to invoke C compiler '{cc}'"));
    assert!(
        compiled.success(),
        "generated C test harness did not compile"
    );
    let status = Command::new(&exe_file)
        .status()
        .expect("failed to execute generated C test harness");
    assert!(
        status.success(),
        "generated C test harness failed: {status}"
    );
}

#[test]
fn c_emission_is_byte_deterministic() {
    let src = r#"
        struct Pair { left: int; right: int }
        func main(): int {
            let pair = Pair { left: 20, right: 22 }
            return pair.left + pair.right
        }
    "#;
    let (amir, tc) = compile_src(src);

    let first = emit_c(&amir, &tc);
    let second = emit_c(&amir, &tc);
    let (fresh_amir, fresh_tc) = compile_src(src);
    let fresh = emit_c(&fresh_amir, &fresh_tc);

    assert_eq!(first.as_bytes(), second.as_bytes());
    assert_eq!(first.as_bytes(), fresh.as_bytes());
}

#[test]
fn c_backend_rejects_residual_null_coalesce_without_partial_success() {
    let (mut amir, tc) = compile_src("func main(): int { let x = 1; return x }");
    let assign = amir
        .funcs
        .iter_mut()
        .flat_map(|func| func.stmts.payloads.raw.iter_mut())
        .find_map(|stmt| match stmt {
            AmirStmt::Assign { rhs, .. } => Some(rhs),
            _ => None,
        })
        .expect("fixture must lower at least one assignment");
    *assign = AmirRvalue::Binary {
        op: BinaryOp::NullCoalesce,
        left: AmirOperand::Constant(AmirConstant::Bool(true)),
        right: AmirOperand::Constant(AmirConstant::Bool(false)),
    };

    let error = arandu_backend_c::emit_c(
        &amir,
        tc.symbols.as_ref(),
        tc.type_info.as_ref(),
        &tc.type_info.type_interner,
        DataLayout::host(),
    )
    .unwrap_err();
    assert_eq!(error.code, arandu_middle::DiagCode::ICEGEN001);
}

#[test]
fn c_backend_rejects_unsupported_len_without_partial_success() {
    let (mut amir, tc) = compile_src("func main(): int { let x = 1; return x }");
    let assign = amir
        .funcs
        .iter_mut()
        .flat_map(|func| func.stmts.payloads.raw.iter_mut())
        .find_map(|stmt| match stmt {
            AmirStmt::Assign { rhs, .. } => Some(rhs),
            _ => None,
        })
        .expect("fixture must lower at least one assignment");
    *assign = AmirRvalue::Len(AmirOperand::Constant(AmirConstant::Bool(true)));

    let error = arandu_backend_c::emit_c(
        &amir,
        tc.symbols.as_ref(),
        tc.type_info.as_ref(),
        &tc.type_info.type_interner,
        DataLayout::host(),
    )
    .unwrap_err();
    assert_eq!(error.code, arandu_middle::DiagCode::ICEGEN001);
    assert!(error.message.contains("Len"));
}

#[test]
fn both_backends_reject_the_same_invalid_ssa_edge() {
    let (mut amir, tc) = compile_src("func main(): int { let x = 1; return x }");
    amir.funcs[0].blocks[0].terminator = arandu_middle::amir::AmirTerminator::Goto {
        target: arandu_middle::amir::BlockId::from_usize(0),
        args: vec![AmirOperand::Copy(arandu_middle::amir::TempId::from_usize(
            0,
        ))],
    };

    assert_backend_rejection_parity(&amir, &tc, "SSA-EDGE");
}

#[test]
fn both_backends_reject_the_same_poison_type() {
    let (mut amir, tc) = compile_src("func main(): int { let x = 1; return x }");
    amir.funcs[0].temps[0].ty = tc.type_info.type_interner.error_type_id();

    assert_backend_rejection_parity(&amir, &tc, "TYP-1");
}

#[test]
fn both_backends_reject_the_same_out_of_bounds_statement_range() {
    let (mut amir, tc) = compile_src("func main(): int { let x = 1; return x }");
    let invalid_len = amir.funcs[0].stmts.len() + 1;
    amir.funcs[0].blocks[0].statements = arandu_middle::layout::DenseRange::new(0, invalid_len);

    assert_backend_rejection_parity(&amir, &tc, "IR-RANGE");
}

#[test]
fn parity_index_addressing_combined_with_shift() {
    test_execution_parity(
        "index_shift_addressing",
        r#"
        func main(): int {
            let values: [4]int = [3, 5, 7, 11]
            let index: int = 1 << 1
            return values[index] + (1024 >> 5)
        }
        "#,
    );
}

#[test]
fn parity_fibonacci() {
    let src = r#"
    func fib(n: int): int {
        if n <= 1 {
            return n
        }
        return fib(n - 1) + fib(n - 2)
    }
    
    func main(): int {
        return fib(10)
    }
    "#;
    test_execution_parity("fibonacci", src);
}

#[test]
fn parity_struct_layout() {
    let src = r#"
    struct Point {
        x: int
        y: byte
        z: int
    }
    
    func main(): int {
        let p = Point { x: 10, y: 5 as byte, z: 20 }
        return p.z
    }
    "#;
    test_execution_parity("struct_layout", src);
}

#[test]
fn parity_str_literal() {
    let src = r#"
    func get_len(s: str): int {
        return 42 // fixed value; this test verifies str structs can be passed without crashing
    }
    func main(): int {
        return get_len("hello")
    }
    "#;
    test_execution_parity("str_literal", src);
}

#[test]
fn parity_string_interpolation() {
    // Builds an interpolated string and only checks that the program runs
    // end-to-end on both backends (C + Cranelift) without crash.
    let src = r#"
    func main(): int {
        let name = "Bruno"
        let msg = "Oi, ${name}"
        return 0
    }
    "#;
    test_execution_parity("string_interpolation", src);
}

#[test]
fn parity_enum_layout() {
    let src = r#"
    enum Status {
        Ok(int)
        Err(byte)
    }
    
    func main(): int {
        let r: Status = Status.Ok(42)
        let mut out: int = 0
        match r {
            Status.Ok(v) => { out = v; }
            Status.Err(_) => { out = -1; }
        }
        return out
    }
    "#;
    test_execution_parity("enum_layout", src);
}

#[test]
fn parity_ssa_pattern_bind() {
    let src = r#"
    enum Wrapper {
        Val(int)
    }
    
    func main(): int {
        let w: Wrapper = Wrapper.Val(123)
        let mut res: int = 0
        if w is Wrapper.Val(x) {
            res = x
        }
        return res
    }
    "#;
    test_execution_parity("ssa_pattern_bind", src);
}

#[test]
fn parity_ssa_pattern_bind_multi_arms() {
    let src = r#"
    enum Wrapper {
        Val(int)
        Other(int)
    }
    
    func main(): int {
        let w: Wrapper = Wrapper.Other(42)
        let mut res: int = 0
        match w {
            Wrapper.Val(x) => {
                res = x
            }
            Wrapper.Other(y) => {
                res = y
            }
        }
        return res
    }
    "#;
    test_execution_parity("ssa_pattern_bind_multi_arms", src);
}

#[test]
fn parity_array_index_access() {
    let src = r#"
    func dummy(xs: [3]int) {}

    func main(): int {
        let mut xs = [10, 20, 30]
        let idx = 1
        xs[idx] = 42
        dummy(xs)
        return 42
    }
    "#;
    test_execution_parity("array_index_access", src);
}

#[test]
fn parity_enum_multi_variant_switch() {
    let src = r#"
    enum Color {
        Red
        Green
        Blue
        Yellow(int)
    }
    
    func main(): int {
        let c: Color = Color.Yellow(100)
        let mut out: int = 0
        match c {
            Color.Red => { out = 1; }
            Color.Green => { out = 2; }
            Color.Blue => { out = 3; }
            Color.Yellow(v) => { out = v; }
        }
        return out
    }
    "#;
    test_execution_parity("enum_multi_variant_switch", src);
}

#[test]
fn parity_array_reassignment() {
    let src = r#"
    func main(): int {
        let mut arr = [10, 20, 30]
        arr = [99, 98, 97]
        return arr[1]
    }
    "#;
    test_execution_parity("array_reassignment", src);
}

#[test]
fn parity_control_flow_diamond() {
    let src = r#"
    func main(): int {
        let x = 10
        let mut out = 0
        if x > 5 {
            out = 1
        } else {
            out = 2
        }
        return out
    }
    "#;
    test_execution_parity("control_flow_diamond", src);
}

#[test]
fn parity_to_str_int_interp() {
    // ToStr v0.1: int formatted into string interp; both backends exit 0.
    let src = r#"
    func main(): int {
        let n: int = 42
        let s = "n=${n}"
        let t = "b=${true}"
        return 0
    }
    "#;
    test_execution_parity("to_str_int_interp", src);
}

#[test]
fn parity_io_println_to_str() {
    // Exercise the official `io.println` lowering. This parity harness compares
    // process status; stdout behavior has its own runtime contract tests.
    let src = r#"
    import io
    func main(): int {
        io.println(42)
        io.println("n=${7}")
        return 0
    }
    "#;
    test_execution_parity("io_println_to_str", src);
}

#[test]
fn parity_to_str_method_and_float() {
    let src = r#"
    import io
    func main(): int {
        let n: int = 10
        let f: float = 2.0
        io.println(n.to_str())
        io.println(f.to_str())
        return 0
    }
    "#;
    test_execution_parity("to_str_method_float", src);
}

#[test]
fn c_emit_to_str_helpers_present() {
    let src = r#"
    func main(): int {
        let n: int = 7
        let s = "x=${n}"
        return 0
    }
    "#;
    let (amir, tc) = compile_src(src);
    let c = emit_c(&amir, &tc);
    assert!(
        c.contains("ar_i64_to_str"),
        "expected ToStr helper in emit, got:\n{c}"
    );
    assert!(
        c.contains("to_str") || c.contains("ar_i64_to_str("),
        "expected ToStr call site"
    );
}

#[test]
fn c_emit_arstr_is_fat_pointer() {
    // S-C-AUDIT: ArStr matches LayoutEngine fat pointer (host 64 → int64_t len).
    let src = r#"
    func main(): int {
        let s = "hi"
        return 0
    }
    "#;
    let (amir, tc) = compile_src(src);
    let c = emit_c(&amir, &tc);
    assert!(
        c.contains("typedef struct { const uint8_t *ptr; int64_t len; } ArStr;"),
        "expected ArStr fat-pointer typedef, got headers:\n{}",
        c.lines().take(40).collect::<Vec<_>>().join("\n")
    );
    assert!(c.contains("AR_STR_"), "expected named string constants");
}

#[test]
fn c_emit_arstr_layout_32bit() {
    // S-C-32BIT: emit-only with W=4 (no Cranelift). ArStr.len is int32_t.
    let src = r#"
    func main(): int {
        let s = "hi"
        return 0
    }
    "#;
    let (amir, tc) = compile_src(src);
    let c = arandu_backend_c::emit_c(
        &amir,
        tc.symbols.as_ref(),
        tc.type_info.as_ref(),
        &tc.type_info.type_interner,
        DataLayout::ptr_width(4),
    )
    .unwrap();
    assert!(
        c.contains("typedef struct { const uint8_t *ptr; int32_t len; } ArStr;"),
        "expected 32-bit ArStr, headers:\n{}",
        c.lines().take(40).collect::<Vec<_>>().join("\n")
    );
    assert!(c.contains("static void *ar_vec_malloc(uint32_t size)"));
    assert!(c.contains(
        "typedef struct { uint8_t *data; uint32_t len; uint32_t capacity; } ArOwnedStringRuntime;"
    ));
    assert!(c.contains("static int32_t ar_str_len(ArStr s)"));
}

#[test]
fn c_emit_arstr_i686_sysv() {
    // DataLayout::i686_sysv: pointer 4; i64/f64 abi_align 4 — ArStr still {ptr, int32_t len}.
    let src = r#"
    func main(): int {
        let s = "hi"
        return 0
    }
    "#;
    let (amir, tc) = compile_src(src);
    let c = arandu_backend_c::emit_c(
        &amir,
        tc.symbols.as_ref(),
        tc.type_info.as_ref(),
        &tc.type_info.type_interner,
        DataLayout::i686_sysv(),
    )
    .unwrap();
    assert!(
        c.contains("typedef struct { const uint8_t *ptr; int32_t len; } ArStr;"),
        "i686 ArStr: {}",
        c.lines().take(30).collect::<Vec<_>>().join("\n")
    );
}

#[test]
fn c_emit_extern_declaration_present() {
    let src = r#"
    extern "C" {
        func my_custom_extern_func(x: int): int
    }
    func main(): int {
        unsafe {
            return my_custom_extern_func(42)
        }
    }
    "#;
    let (amir, tc) = compile_src(src);
    let c = emit_c(&amir, &tc);
    assert!(
        c.contains("int64_t my_custom_extern_func(int64_t);"),
        "expected custom extern function declaration, got:\n{}",
        c
    );
}

#[test]
fn parity_references_and_deref() {
    let src = r#"
    func takes_ref(p: &int): int {
        return *p
    }
    func main(): int {
        let x: int = 123
        return takes_ref(x)
    }
    "#;
    test_execution_parity("references_and_deref", src);
}

#[test]
fn parity_mixed_alignment_packing() {
    let src = r#"
    struct MixedLayout {
        a: byte
        b: int
        c: bool
        d: int
    }
    func main(): int {
        let m = MixedLayout { a: 42 as byte, b: 999999, c: true, d: 123456 }
        if (m.a as int) == 42 && m.b == 999999 && m.c && m.d == 123456 {
            return 0
        }
        return 1
    }
    "#;
    test_execution_parity("mixed_alignment_packing", src);
}
