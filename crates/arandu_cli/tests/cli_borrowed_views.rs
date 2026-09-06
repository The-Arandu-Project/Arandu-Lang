//! BV.3 product regressions for safe Slice/String views.
#![allow(clippy::expect_used)]

use std::fs;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

fn workspace_root() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn invoke(mode: &str, source: &str) -> Output {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "arandu-borrowed-view-{}-{id}.aru",
        std::process::id()
    ));
    fs::write(&path, source).expect("write borrowed-view fixture");
    let output = Command::new(env!("CARGO_BIN_EXE_arandu_cli"))
        .current_dir(workspace_root())
        .args([mode, path.to_str().expect("UTF-8 temp path")])
        .output()
        .expect("run Arandu check");
    let _ = fs::remove_file(path);
    output
}

fn check(source: &str) -> Output {
    invoke("check", source)
}

#[test]
fn vec_slice_is_a_zero_copy_borrowed_view() {
    let output = check(
        r#"module tests.borrowed_views.valid
import std.alloc.vec as vec
import std.core.slice as slice

func main(): int {
    let mut values = vec.new<int>()
    vec.push<int>(values, 7)
    let view = vec.asSlice<int>(values)
    return slice.len<int>(view) as int
}
"#,
    );
    assert!(
        output.status.success(),
        "valid borrowed slice rejected: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn jit_executes_slice_len_through_the_public_api() {
    let output = invoke(
        "run",
        r#"module tests.borrowed_views.run
import std.alloc.vec as vec
import std.core.slice as slice

func main(): int {
    let mut values = vec.new<int>()
    vec.push<int>(values, 7)
    let view = vec.asSlice<int>(values)
    return slice.len<int>(view) as int
}
"#,
    );
    assert_eq!(
        output.status.code(),
        Some(1),
        "JIT slice execution failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn jit_executes_borrowed_element_and_subslice() {
    let output = invoke("run", include_str!("fixtures/borrowed_views_bv3.aru"));
    assert_eq!(
        output.status.code(),
        Some(8),
        "JIT borrowed element/subslice failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn live_slice_blocks_owner_reallocation() {
    let output = check(
        r#"module tests.borrowed_views.reallocation
import std.alloc.vec as vec
import std.core.slice as slice

func main(): int {
    let mut values = vec.new<int>()
    vec.push<int>(values, 1)
    let view = vec.asSlice<int>(values)
    vec.push<int>(values, 2)
    return slice.len<int>(view) as int
}
"#,
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "reallocation with live view passed"
    );
    assert!(
        stderr.contains("O002") || stderr.contains("O003"),
        "expected ownership conflict, got: {stderr}"
    );
}

#[test]
fn slice_of_local_owner_cannot_escape() {
    let output = check(
        r#"module tests.borrowed_views.escape
import std.alloc.vec as vec

func escape(): []int {
    let mut values = vec.new<int>()
    vec.push<int>(values, 1)
    return vec.asSlice<int>(values)
}
"#,
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "dangling slice escaped");
    assert!(stderr.contains("O010"), "expected O010, got: {stderr}");
}

#[test]
fn jit_consumes_string_from_free_and_associated_ctor() {
    // `from` is a soft keyword: it must work as both the stdlib free function
    // (`strings.from`) and the associated constructor (`strings.String.from`).
    let output = invoke(
        "run",
        r#"module tests.borrowed_views.string_from
import std.alloc.string as strings

func main(): int {
    let a = strings.from("free")
    let b = strings.String.from("associated")
    let lenA = strings.len(a)
    let lenB = strings.len(b)
    if lenA != 4 {
        return 10
    }
    if lenB != 10 {
        return 11
    }
    return 12
}
"#,
    );
    let code = output.status.code();
    assert_eq!(
        code,
        Some(12),
        "JIT String.from (free/associated) failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn string_views_block_mutation() {
    let output = check(
        r#"module tests.borrowed_views.string
import std.alloc.string as strings
import std.core.slice as slice

func main(): int {
    let mut text = strings.new()
    let bytes = strings.asBytes(text)
    strings.pushScalar(text, 'a')
    return slice.len<u8>(bytes) as int
}
"#,
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "String mutated with a live byte view"
    );
    assert!(
        stderr.contains("O002") || stderr.contains("O003"),
        "expected ownership conflict, got: {stderr}"
    );
}

#[test]
fn jit_appends_utf8_and_reads_string_view() {
    let output = invoke(
        "run",
        r#"module tests.borrowed_views.string_run
import std.alloc.string as strings

func main(): int {
    let mut value = strings.new()
    if !strings.pushStr(value, "olá") {
        return 1
    }
    let view = strings.asStr(value)
    if *view == "olá" {
        return strings.len(value) as int
    }
    return 2
}
"#,
    );
    assert_eq!(
        output.status.code(),
        Some(4),
        "JIT String.pushStr/asStr failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn c_backend_erases_view_intrinsics_to_the_ptr_len_abi() {
    let output = invoke(
        "emit-c",
        r#"module tests.borrowed_views.c_backend
import std.alloc.vec as vec
import std.core.slice as slice

func main(): int {
    let mut values = vec.new<int>()
    vec.push<int>(values, 5)
    let view = vec.asSlice<int>(values)
    return slice.len<int>(view) as int
}
"#,
    );
    assert!(
        output.status.success(),
        "C emission failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let c = String::from_utf8_lossy(&output.stdout);
    assert!(
        c.matches("sliceFromRaw").count() <= 1,
        "slice constructor remained as a runtime call"
    );
    assert!(
        c.contains("ArType_Slice_"),
        "slice ABI type was not emitted"
    );
    assert!(
        !c.contains("*(T**)((uint8_t*)"),
        "generic field templates leaked into a monomorphic C function"
    );
}
