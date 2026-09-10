#![cfg(target_pointer_width = "64")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use arandu_backend_cranelift::CraneliftBackend;
use arandu_middle::amir::{AmirConstant, AmirOperand, AmirProgram, AmirRvalue, AmirStmt};
use arandu_middle::layout::DataLayout;
use arandu_middle::ops::BinaryOp;
use arandu_semantics::{
    CodegenBackend, OptLevel, TypeCheckResult, lower_to_amir, lower_to_hir,
    optimize_amir_checked_with_level, resolve_for_test, type_check,
};
use std::env;
use std::fmt::Write as _;
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
    let mut tc = type_check(
        resolution,
        &program,
        arandu_semantics::TargetInfo { pointer_width: 64 },
    );
    assert!(
        tc.diagnostics.is_empty(),
        "type check failed: {:?}",
        tc.diagnostics
    );

    let hir = lower_to_hir(&mut tc, &program).expect("HIR lowering failed");
    let amir = lower_to_amir(&tc, &hir, 64).expect("AMIR lowering failed");
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

fn execute_c(name: &str, amir: &AmirProgram, tc: &TypeCheckResult) -> i32 {
    execute_c_output(name, amir, tc).0
}

fn execute_c_output(name: &str, amir: &AmirProgram, tc: &TypeCheckResult) -> (i32, String) {
    // 1. Generate C (no debug dumps — keep tests pure / CI-friendly).
    let mut c_code = emit_c(amir, tc);

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
        .output()
        .unwrap_or_else(|_| {
            panic!(
                "failed to invoke C compiler '{}'. Parity tests require a C compiler in PATH.",
                cc
            )
        });

    assert!(
        compile_status.status.success(),
        "C compilation failed for {name}: {}",
        String::from_utf8_lossy(&compile_status.stderr)
    );

    let output = Command::new(&exe_file)
        .output()
        .expect("failed to run compiled executable");

    assert!(
        output.status.success(),
        "C program crashed for {name}: status={}, stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );

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

    (actual_result, stdout)
}

fn test_execution_result(name: &str, src: &str) -> (i32, i32) {
    let (amir, tc) = compile_src(src);
    let actual_result = execute_c(name, &amir, &tc);

    // 2. Run via Cranelift
    let expected = execute_cranelift(&amir, &tc);

    assert_eq!(
        expected, actual_result,
        "Execution mismatch for {}! Cranelift={}, C={}",
        name, expected, actual_result
    );
    (expected, actual_result)
}

fn generated_integer_fixture() -> (String, i32) {
    const CASES: i64 = 64;
    let mut source = String::new();
    let mut expected = 0i64;
    let mut state = 0x6a09_e667_f3bc_c909u64;

    for index in 0..CASES {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
        let a = i64::from(state.to_le_bytes()[1] % 31) - 15;
        let b = i64::from(state.to_le_bytes()[3] % 19) - 9;
        let c = i64::from(state.to_le_bytes()[5] % 23) - 11;
        let divisor = i64::from(state.to_le_bytes()[7] % 7) + 1;
        let value = ((a * b) + c) / divisor;
        let threshold = i64::from(state.to_le_bytes()[0] % 11) - 5;
        let selected = if value >= threshold {
            value + index
        } else {
            threshold - value
        };
        expected += selected;
        source.push_str(&format!(
            "func generated{index}(): int {{\n\
             let a: int = {a}\n\
             let b: int = {b}\n\
             let c: int = {c}\n\
             let value: int = ((a * b) + c) / {divisor}\n\
             if value >= {threshold} {{ return value + {index} }}\n\
             return {threshold} - value\n\
             }}\n"
        ));
    }
    source.push_str("func main(): int {\n    return ");
    for index in 0..CASES {
        if index != 0 {
            source.push_str(" + ");
        }
        source.push_str(&format!("generated{index}()"));
    }
    source.push_str("\n}\n");

    (
        source,
        i32::try_from(expected).expect("bounded oracle result"),
    )
}

#[derive(Clone, Copy, Debug)]
enum IntegerOp {
    Add,
    Subtract,
    Multiply,
    Xor,
}

struct IntegerProgram<const N: usize> {
    operations: [IntegerOp; N],
    operands: [i64; N],
    initial: i64,
    threshold: i64,
    branch_delta: i64,
    loop_delta: i64,
    loop_count: i64,
}

impl<const N: usize> IntegerProgram<N> {
    fn from_seed(seed: u64) -> Self {
        let mut state = seed;
        let mut next = || {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            state.to_le_bytes()[4]
        };
        let operations = std::array::from_fn(|_| match next() % 4 {
            0 => IntegerOp::Add,
            1 => IntegerOp::Subtract,
            2 => IntegerOp::Multiply,
            _ => IntegerOp::Xor,
        });
        let operands = std::array::from_fn(|index| match operations[index] {
            IntegerOp::Multiply => i64::from(next() % 5) - 2,
            IntegerOp::Xor => i64::from(next() % 32),
            IntegerOp::Add | IntegerOp::Subtract => i64::from(next() % 17) - 8,
        });
        Self {
            operations,
            operands,
            initial: i64::from(next() % 41) - 20,
            threshold: i64::from(next() % 31) - 15,
            branch_delta: i64::from(next() % 13) - 6,
            loop_delta: i64::from(next() % 7) - 3,
            loop_count: i64::from(next() % 4),
        }
    }

    fn evaluate(&self) -> i32 {
        let mut value = self.initial;
        for (&operation, &operand) in self.operations.iter().zip(&self.operands) {
            value = match operation {
                IntegerOp::Add => value + operand,
                IntegerOp::Subtract => value - operand,
                IntegerOp::Multiply => value * operand,
                IntegerOp::Xor => value ^ operand,
            };
        }
        if value >= self.threshold {
            value += self.branch_delta;
        } else {
            value -= self.branch_delta;
        }
        value += self.loop_delta * self.loop_count;
        i32::try_from(value).expect("bounded structural integer oracle")
    }

    fn emit_source(&self) -> String {
        let mut source = String::with_capacity(768);
        self.emit_function(&mut source, "main");
        source
    }

    fn emit_function(&self, source: &mut String, name: &str) {
        writeln!(source, "func {name}(): int {{").unwrap();
        writeln!(source, "    let mut value: int = {}", self.initial).unwrap();
        for (&operation, &operand) in self.operations.iter().zip(&self.operands) {
            let operator = match operation {
                IntegerOp::Add => '+',
                IntegerOp::Subtract => '-',
                IntegerOp::Multiply => '*',
                IntegerOp::Xor => '^',
            };
            writeln!(source, "    set value = value {operator} ({operand})").unwrap();
        }
        writeln!(source, "    if value >= {} {{", self.threshold).unwrap();
        writeln!(
            source,
            "        set value = value + ({})",
            self.branch_delta
        )
        .unwrap();
        writeln!(source, "    }} else {{").unwrap();
        writeln!(
            source,
            "        set value = value - ({})",
            self.branch_delta
        )
        .unwrap();
        writeln!(source, "    }}").unwrap();
        writeln!(source, "    let mut index: int = 0").unwrap();
        writeln!(source, "    while index < {} {{", self.loop_count).unwrap();
        writeln!(source, "        set value = value + ({})", self.loop_delta).unwrap();
        writeln!(source, "        set index = index + 1").unwrap();
        writeln!(source, "    }}").unwrap();
        writeln!(source, "    return value").unwrap();
        writeln!(source, "}}").unwrap();
    }
}

const STRUCTURAL_INTEGER_SEEDS: [u64; 8] = [
    0,
    1,
    0x243f_6a88_85a3_08d3,
    0x1319_8a2e_0370_7344,
    0xa409_3822_299f_31d0,
    0x082e_fa98_ec4e_6c89,
    u64::MAX - 1,
    u64::MAX,
];

fn structural_integer_suite() -> String {
    let mut source = String::with_capacity(STRUCTURAL_INTEGER_SEEDS.len() * 768);
    let mut expected = [0i32; STRUCTURAL_INTEGER_SEEDS.len()];
    for (index, seed) in STRUCTURAL_INTEGER_SEEDS.into_iter().enumerate() {
        let program = IntegerProgram::<12>::from_seed(seed);
        program.emit_function(&mut source, &format!("generated{index}"));
        expected[index] = program.evaluate();
    }
    writeln!(source, "func main(): int {{").unwrap();
    for (index, expected) in expected.into_iter().enumerate() {
        writeln!(
            source,
            "    if generated{index}() != {expected} {{ return {} }}",
            index + 1
        )
        .unwrap();
    }
    writeln!(source, "    return 0").unwrap();
    writeln!(source, "}}").unwrap();
    source
}

fn test_execution_parity(name: &str, src: &str) {
    let _ = test_execution_result(name, src);
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
fn generated_integer_programs_match_independent_oracle() {
    let (source, expected) = generated_integer_fixture();
    let (jit, c) = test_execution_result("generated_integer_oracle", &source);
    assert_eq!(
        jit, expected,
        "Cranelift disagreed with the independent oracle"
    );
    assert_eq!(c, expected, "C disagreed with the independent oracle");
}

#[test]
fn optimization_levels_preserve_the_generated_integer_oracle() {
    let (source, expected) = generated_integer_fixture();

    for level in [OptLevel::O0, OptLevel::O1, OptLevel::O2, OptLevel::Os] {
        let (mut amir, tc) = compile_src(&source);
        optimize_amir_checked_with_level(
            &mut amir,
            tc.symbols.as_ref(),
            &tc.type_info.type_interner,
            level,
        )
        .unwrap_or_else(|error| panic!("{level:?} optimization rejected valid AMIR: {error:?}"));

        let jit = execute_cranelift(&amir, &tc);
        let c = execute_c(&format!("generated_integer_{level:?}"), &amir, &tc);
        assert_eq!(jit, expected, "{level:?} Cranelift result changed");
        assert_eq!(c, expected, "{level:?} C result changed");
    }
}

#[test]
fn structural_integer_programs_agree_across_optimization_levels() {
    for seed in STRUCTURAL_INTEGER_SEEDS {
        let program = IntegerProgram::<12>::from_seed(seed);
        let source = program.emit_source();
        let expected = program.evaluate();

        for level in [OptLevel::O0, OptLevel::O1, OptLevel::O2, OptLevel::Os] {
            let (mut amir, tc) = compile_src(&source);
            optimize_amir_checked_with_level(
                &mut amir,
                tc.symbols.as_ref(),
                &tc.type_info.type_interner,
                level,
            )
            .unwrap_or_else(|error| {
                panic!("seed {seed:#018x} {level:?} rejected valid AMIR: {error:?}\n{source}")
            });
            let actual = execute_cranelift(&amir, &tc);
            assert_eq!(
                actual, expected,
                "seed {seed:#018x} changed under {level:?}\n{source}"
            );
        }
    }
}

#[test]
fn structural_integer_suite_agrees_between_backends_and_opt_levels() {
    let source = structural_integer_suite();

    for level in [OptLevel::O0, OptLevel::O1, OptLevel::O2, OptLevel::Os] {
        let (mut amir, tc) = compile_src(&source);
        optimize_amir_checked_with_level(
            &mut amir,
            tc.symbols.as_ref(),
            &tc.type_info.type_interner,
            level,
        )
        .unwrap_or_else(|error| panic!("{level:?} rejected structural suite: {error:?}"));

        let jit = execute_cranelift(&amir, &tc);
        let c = execute_c(&format!("structural_integer_{level:?}"), &amir, &tc);
        assert_eq!(jit, 0, "{level:?} Cranelift failed generated case {jit}");
        assert_eq!(c, 0, "{level:?} C failed generated case {c}");
    }
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

#[test]
fn coroutine_value_uses_pointer_abi_in_both_backends() {
    let result = test_execution_result(
        "coroutine_pointer_abi",
        r#"
extern "C" { func ar_co_block_on_i64(state: ptr[u8]): int }
async func answer(): int { return 42 }
func main(): int {
    let job = answer()
    return unsafe { ar_co_block_on_i64(job as ptr[u8]) }
}
"#,
    );
    assert_eq!(result, (42, 42));
}

#[test]
fn cooperative_task_table_matches_in_both_backends() {
    let result = test_execution_result(
        "rt_task_table",
        r#"
extern "C" {
    func ar_rt_spawn_i64(state: ptr[u8]): int
    func ar_rt_join_i64(handle: int): int
    func ar_rt_cancel_i64(handle: int): void
}
async func answer(): int { return 42 }
func main(): int {
    let job = answer()
    let handle = unsafe { ar_rt_spawn_i64(job as ptr[u8]) }
    return unsafe { ar_rt_join_i64(handle) }
}
"#,
    );
    assert_eq!(result, (42, 42));
}

#[test]
fn cooperative_cancel_before_join_recovers_slot_in_c() {
    let result = test_execution_result(
        "rt_task_cancel",
        r#"
extern "C" {
    func ar_rt_spawn_i64(state: ptr[u8]): int
    func ar_rt_join_i64(handle: int): int
    func ar_rt_cancel_i64(handle: int): void
}
async func answer(): int { return 42 }
func main(): int {
    let job = answer()
    let handle = unsafe { ar_rt_spawn_i64(job as ptr[u8]) }
    unsafe { ar_rt_cancel_i64(handle) }
    let job2 = answer()
    let handle2 = unsafe { ar_rt_spawn_i64(job2 as ptr[u8]) }
    if handle2 != handle {
        return 1
    }
    return unsafe { ar_rt_join_i64(handle2) }
}
"#,
    );
    assert_eq!(result, (42, 42));
}

#[test]
fn cooperative_cached_join_reuses_result_without_polling() {
    let result = test_execution_result(
        "rt_task_cached",
        r#"
extern "C" {
    func ar_rt_spawn_i64(state: ptr[u8]): int
    func ar_rt_join_i64(handle: int): int
    func ar_rt_cancel_i64(handle: int): void
}
async func answer(): int { return 42 }
func main(): int {
    let job = answer()
    let handle = unsafe { ar_rt_spawn_i64(job as ptr[u8]) }
    let first = unsafe { ar_rt_join_i64(handle) }
    let cached = unsafe { ar_rt_join_i64(handle) }
    if first != 42 || cached != 42 {
        return 1
    }
    unsafe { ar_rt_cancel_i64(handle) }
    return 0
}
"#,
    );
    assert_eq!(result, (0, 0));
}

#[test]
fn owned_job_lifecycle_preserves_fields_and_cleanup_in_c() {
    let source = include_str!("../../arandu_cli/tests/fixtures/owned_result_lifecycle.aru");
    for optimized in [false, true] {
        let (mut amir, tc) = compile_src(source);
        if optimized {
            optimize_amir_checked_with_level(
                &mut amir,
                tc.symbols.as_ref(),
                &tc.type_info.type_interner,
                OptLevel::O2,
            )
            .expect("valid owned job must optimize");
        }
        let name = if optimized {
            "owned_job_opt"
        } else {
            "owned_job"
        };
        let (status, stdout) = execute_c_output(name, &amir, &tc);
        assert_eq!(status, 0);
        assert_eq!(
            stdout.replace("\r\n", "\n"),
            "30\n20\n0\n",
            "optimized={optimized}"
        );
    }
}

/// SL_P Fase 4: a compiler-shaped job thunk written as an ordinary generic
/// function. `dispatch` reads a `Job<R>` payload from `context`, runs it and
/// writes the `R` result into `result` — the exact `WorkThunk` ABI
/// (`(ptr, ptr) -> i32`). `main` feeds it blobs through the `alloc`/`free`
/// builtins so the same source type-checks and lowers in both backends.
const GENERIC_WORK_THUNK_SRC: &str = r#"
module std.core.workthunk

extern "arandu-intrinsic" {
    func ptrRead<T>(p: ptr[T]) : T
    func ptrWrite<T>(p: ptr[T], val: T) : void
}

interface Job<R> {
    func run(shared self): R
}

struct Stats { code: int, comment: int, blank: int }
struct CountJob { amount: int }

func CountJob.run(shared self): Stats {
    return Stats { code: self.amount, comment: 2, blank: 3 }
}

func dispatch<R, C: Job<R>>(context: ptr[C], result: ptr[R]): i32 {
    let job = unsafe { ptrRead<C>(context) }
    let out = job.run()
    unsafe { ptrWrite<R>(result, out) }
    return 0
}

func main(): int {
    let job = CountJob { amount: 37 }
    let c = alloc(8) as ptr[CountJob]
    unsafe { ptrWrite<CountJob>(c, job) }
    let r = alloc(24) as ptr[Stats]
    let rc = dispatch<Stats, CountJob>(c, r)
    let out = unsafe { ptrRead<Stats>(r) }
    if rc != 0 { return 9 }
    if out.code != 37 { return 1 }
    if out.comment != 2 { return 2 }
    if out.blank != 3 { return 3 }
    unsafe { free(c) }
    unsafe { free(r) }
    return 0
}
"#;

#[test]
fn generic_work_thunk_runs_identically_in_c_and_cranelift() {
    // Runs the real production pipeline (`monomorphize_program`) on both
    // backends; `execute_c` wraps the emitted C with a C main and compares the
    // exit code with the Cranelift-run Arandu `main`.
    let (amir, tc) = compile_src_mono(GENERIC_WORK_THUNK_SRC);
    let actual_result = execute_c("generic_work_thunk", &amir, &tc);
    let expected = execute_cranelift(&amir, &tc);
    assert_eq!(
        expected, actual_result,
        "Execution mismatch for generic_work_thunk! Cranelift={expected}, C={actual_result}"
    );
    assert_eq!(expected, 0);
}

/// Host-side mirror of the Arandu structs fed to/read from the thunk blobs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
struct CountJobHost {
    amount: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
struct StatsHost {
    code: i64,
    comment: i64,
    blank: i64,
}

/// Compile the generic thunk source and return the exact host name under which
/// the monomorphized `dispatch<Stats, CountJob>` instance was registered.
fn compile_generic_work_thunk() -> (AmirProgram, TypeCheckResult, String) {
    let (amir, tc) = compile_src_mono(GENERIC_WORK_THUNK_SRC);
    let instance_name = amir
        .funcs
        .iter()
        .filter_map(|f| {
            let name = tc.symbols.host_func_name(tc.symbols.get(f.symbol));
            name.starts_with("_A$dispatch$I_").then(|| name.to_string())
        })
        .next()
        .expect("monomorphized dispatch instance missing");
    (amir, tc, instance_name)
}

/// Same as [`compile_src`] but runs the monomorphization pass, matching the
/// production pipeline: generic callees become real instanced functions
/// (`_A$...`) instead of being inlined at the call site by AMIR lowering.
fn compile_src_mono(src: &str) -> (AmirProgram, TypeCheckResult) {
    let program = arandu_parser::parse(src).expect("parse failed");
    let resolution = resolve_for_test(0, &program);
    let mut tc = type_check(
        resolution,
        &program,
        arandu_semantics::TargetInfo { pointer_width: 64 },
    );
    assert!(
        tc.diagnostics.is_empty(),
        "type check failed: {:?}",
        tc.diagnostics
    );

    let mut hir = lower_to_hir(&mut tc, &program).expect("HIR lowering failed");
    let specialized =
        arandu_semantics::monomorphize_program(&mut tc, &mut hir).expect("monomorphization failed");
    assert!(
        specialized > 0,
        "generic dispatch instance must be specialized"
    );
    let amir = lower_to_amir(&tc, &hir, 64).expect("AMIR lowering failed");
    (amir, tc)
}

#[test]
fn generic_work_thunk_is_host_callable_at_workthunk_abi() {
    let (amir, tc, instance_name) = compile_generic_work_thunk();
    let backend = CraneliftBackend::try_new().unwrap();
    let compiled =
        CodegenBackend::compile(backend, &amir, tc.symbols.as_ref(), tc.type_info.as_ref())
            .expect("cranelift compile failed");

    let via_main = unsafe {
        let main_fn =
            arandu_semantics::CompiledCode::get_fn::<unsafe fn() -> i32>(&compiled, "main")
                .expect("main not found");
        main_fn()
    };
    assert_eq!(via_main, 0);

    let thunk: arandu_runtime::worker_runtime::WorkThunk = unsafe {
        arandu_semantics::CompiledCode::get_fn(&compiled, &instance_name)
            .expect("instance not exported under its host name")
    };

    let mut context = CountJobHost { amount: 37 };
    let mut result = std::mem::MaybeUninit::<StatsHost>::uninit();
    let status = unsafe {
        thunk(
            (&mut context as *mut CountJobHost).cast::<u8>(),
            result.as_mut_ptr().cast::<u8>(),
        )
    };
    assert_eq!(status, arandu_runtime::worker_runtime::WORK_COMPLETED);
    let stats = unsafe { result.assume_init() };
    assert_eq!(
        stats,
        StatsHost {
            code: 37,
            comment: 2,
            blank: 3
        }
    );
}

#[test]
fn generic_work_thunk_executes_inside_worker_pool_thread() {
    let (amir, tc, instance_name) = compile_generic_work_thunk();
    let backend = CraneliftBackend::try_new().unwrap();
    let compiled =
        CodegenBackend::compile(backend, &amir, tc.symbols.as_ref(), tc.type_info.as_ref())
            .expect("cranelift compile failed");
    let thunk: arandu_runtime::worker_runtime::WorkThunk = unsafe {
        arandu_semantics::CompiledCode::get_fn(&compiled, &instance_name)
            .expect("instance not exported under its host name")
    };

    let pool = arandu_runtime::worker_scheduler::WorkerPool::new(2, 4)
        .expect("pool with two workers must spawn");
    // SAFETY: `CountJobHost`/`StatsHost` mirror the Arandu layouts and the
    // thunk obeys the `WorkThunk` lifecycle contract.
    let task = unsafe {
        arandu_runtime::worker_runtime::WorkerTask::try_new::<CountJobHost, StatsHost>(
            CountJobHost { amount: 41 },
            thunk,
        )
        .expect("static-sized payload must be encodable")
    };
    let pending = pool
        .core()
        .submit(task)
        .expect("bounded admission must accept one task");
    let result = pending.wait().expect("task must complete before shutdown");
    let stats = result
        .try_take::<StatsHost>()
        .expect("typed result must extract");
    assert_eq!(
        stats,
        StatsHost {
            code: 41,
            comment: 2,
            blank: 3
        }
    );
}
