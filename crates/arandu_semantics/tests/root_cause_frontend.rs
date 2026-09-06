#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Regression tests for root-cause frontend fixes (RC-SET, RC-GUARD, RC-NEST,
//! RC-F64, RC-ERR-NIL). Each case previously required example workarounds.

use arandu_semantics::{lower_to_hir, resolve_for_test, type_check};

fn check_ok(src: &str) {
    let program = arandu_parser::parse(src).expect("parse");
    let resolution = resolve_for_test(0, &program);
    let mut tc = type_check(
        resolution,
        &program,
        arandu_semantics::TargetInfo { pointer_width: 64 },
    );
    assert!(
        tc.diagnostics
            .iter()
            .all(|d| d.severity != arandu_middle::Severity::Error),
        "typeck errors: {:?}",
        tc.diagnostics
    );
    let hir = lower_to_hir(&mut tc, &program).expect("lower");
    hir.validate_invariants(&hir.pool, &tc.symbols)
        .expect("hir invariants");
}

#[test]
fn rc_set_keyword_assignment() {
    check_ok(
        r#"
func main() {
    let mut x: int = 0
    set x = 1
    set x = x + 2
}
"#,
    );
}

#[test]
fn rc_match_guard() {
    check_ok(
        r#"
enum Token {
    Number(int),
    Word(str),
    End,
}
func describe(token: Token): str {
    return match token {
        Token.Number(value) if value > 0 => "positive number"
        Token.Number(_) => "number"
        Token.Word(text) => "word ${text}"
        Token.End => "end"
    }
}
"#,
    );
}

#[test]
fn rc_nested_method_into_namespace_call() {
    check_ok(
        r#"
import io
struct User {
    name: str
}
func User.greet(shared self): str {
    return self.name
}
func main() {
    let user = User { name: "Ana" }
    io.println(user.greet())
}
"#,
    );
}

#[test]
fn rc_extern_f64_in_unsafe_stmt() {
    check_ok(
        r#"
extern "C" {
    func cos(value: f64): f64
    func sin(value: f64): f64
}
func length(x: f64, y: f64): f64 {
    unsafe {
        let a: f64 = cos(x)
        let b: f64 = sin(y)
        return a + b
    }
}
"#,
    );
}

#[test]
fn rc_result_err_nil_compare() {
    check_ok(
        r#"
import err
import io
func readName(path: str): Result<str, Err> {
    if path == "" {
        return Result.Err(err.new("missing"))
    }
    return Result.Ok("ok")
}
func main() {
    let name, e = readName("p")
    if e != nil {
        io.println("error")
        return
    }
    io.println(name)
}
"#,
    );
}

#[test]
fn rc_interp_accepts_int() {
    // ToStr v0.1: int is formatable in interpolation.
    check_ok(
        r#"
func main() {
    let n: int = 1
    let s = "n=${n}"
}
"#,
    );
}

#[test]
fn rc_println_accepts_primitives() {
    check_ok(
        r#"
import io
func main() {
    io.println(1)
    io.println(true)
    io.println(1.5)
    let n: int = 42
    io.println(n)
}
"#,
    );
}

#[test]
fn rc_interp_rejects_struct() {
    let src = r#"
struct Point {
    x: int
}
func main() {
    let p = Point { x: 1 }
    let s = "p=${p}"
}
"#;
    let program = arandu_parser::parse(src).expect("parse");
    let resolution = resolve_for_test(0, &program);
    let tc = type_check(
        resolution,
        &program,
        arandu_semantics::TargetInfo { pointer_width: 64 },
    );
    assert!(
        tc.diagnostics.iter().any(|d| {
            d.code == arandu_middle::DiagCode::T034CannotFormat
                || d.message.contains("cannot format value of type")
        }),
        "expected T034 for struct in interp, got {:?}",
        tc.diagnostics
    );
}

#[test]
fn rc_println_rejects_struct() {
    let src = r#"
import io
struct Point {
    x: int
}
func main() {
    let p = Point { x: 1 }
    io.println(p)
}
"#;
    let program = arandu_parser::parse(src).expect("parse");
    let resolution = resolve_for_test(0, &program);
    let tc = type_check(
        resolution,
        &program,
        arandu_semantics::TargetInfo { pointer_width: 64 },
    );
    assert!(
        tc.diagnostics.iter().any(|d| {
            d.code == arandu_middle::DiagCode::T034CannotFormat
                || d.message.contains("cannot format value of type")
        }),
        "expected T034 for println(struct), got {:?}",
        tc.diagnostics
    );
}

#[test]
fn rc_to_str_method_accepts_primitives() {
    check_ok(
        r#"
import io
func main() {
    let n: int = 1
    let b: bool = true
    let f: float = 1.5
    let c: char = 'x'
    let u: uint = 2
    let i8v: i8 = 3
    let u64v: u64 = 4
    let s1 = n.to_str()
    let s2 = b.to_str()
    let s3 = f.to_str()
    let s4 = c.to_str()
    let s5 = u.to_str()
    let s6 = i8v.to_str()
    let s7 = u64v.to_str()
    let s8 = "hi".to_str()
    io.println(s1)
}
"#,
    );
}

#[test]
fn rc_to_str_method_rejects_struct() {
    let src = r#"
struct Point {
    x: int
}
func main() {
    let p = Point { x: 1 }
    let s = p.to_str()
}
"#;
    let program = arandu_parser::parse(src).expect("parse");
    let resolution = resolve_for_test(0, &program);
    let tc = type_check(
        resolution,
        &program,
        arandu_semantics::TargetInfo { pointer_width: 64 },
    );
    assert!(
        tc.diagnostics
            .iter()
            .any(|d| d.code == arandu_middle::DiagCode::T034CannotFormat),
        "expected T034 for Point.to_str(), got {:?}",
        tc.diagnostics
    );
}

#[test]
fn rc_to_str_matrix_interp_and_println() {
    // Full primitive matrix for interp + println (typeck + HIR only).
    check_ok(
        r#"
import io
func main() {
    let b: bool = false
    let c: char = 'A'
    let n: int = -7
    let u: uint = 3
    let f: float = 2.5
    let i16v: i16 = -1
    let u32v: u32 = 9
    io.println(b)
    io.println(c)
    io.println(n)
    io.println(u)
    io.println(f)
    io.println(i16v)
    io.println(u32v)
    let s = "m=${b}|${c}|${n}|${u}|${f}|${i16v}|${u32v}"
    io.println(s)
}
"#,
    );
}
