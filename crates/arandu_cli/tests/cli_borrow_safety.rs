//! Black-box regressions for borrow safety at function boundaries.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::fs;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

fn temporary_source(name: &str, source: &str) -> std::path::PathBuf {
    let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
    let directory =
        std::env::temp_dir().join(format!("arandu-borrow-safety-{}-{id}", std::process::id()));
    fs::create_dir_all(&directory).expect("create isolated test directory");
    let path = directory.join(name);
    fs::write(&path, source).expect("write Arandu test source");
    path
}

fn check_source(source: &std::path::Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_arandu_cli"))
        .args(["check", source.to_str().expect("UTF-8 temporary path")])
        .output()
        .expect("run Arandu CLI")
}

fn remove_fixture(source: &std::path::Path) {
    if let Some(directory) = source.parent() {
        let _ = fs::remove_dir_all(directory);
    }
}

#[test]
fn local_borrow_and_dereference_remain_valid() {
    let source = temporary_source(
        "local_borrow.aru",
        r#"module local_borrow

func main(): int {
    let value = 42
    let borrowed = ref value
    return *borrowed
}
"#,
    );

    let output = check_source(&source);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "valid local borrow failed: {stderr}"
    );
    remove_fixture(&source);
}

#[test]
fn returning_a_borrow_of_a_local_is_rejected() {
    let source = temporary_source(
        "local_escape.aru",
        r#"module local_escape

func escape(): ref int {
    let value = 42
    return ref value
}
"#,
    );

    let output = check_source(&source);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "dangling local borrow was accepted"
    );
    assert!(
        stderr.contains("O010"),
        "expected O010 for a dangling local borrow, got: {stderr}"
    );
    remove_fixture(&source);
}

#[test]
fn returning_a_borrow_of_a_parameter_preserves_its_origin() {
    let source = temporary_source(
        "parameter_escape.aru",
        r#"module parameter_escape

func borrowParameter(value: ref int): ref int {
    return value
}

func main(): void {
    let value = 42
    let borrowed = borrowParameter(value)
    let _ = *borrowed
}
"#,
    );

    let output = check_source(&source);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success(),
        "declared parameter borrow was rejected: {stderr}"
    );

    remove_fixture(&source);
}

#[test]
fn multiple_return_origins_are_proved_from_control_flow() {
    let source = temporary_source(
        "ambiguous_parameter_escape.aru",
        r#"module ambiguous_parameter_escape

func choose(flag: bool, left: ref int, right: ref int): ref int {
    if flag {
        return left
    }
    return right
}

func main(): void {
    let left = 1
    let right = 2
    let borrowed = choose(true, left, right)
    let _ = *borrowed
}
"#,
    );

    let output = check_source(&source);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "flow-proved origin union was rejected: {stderr}"
    );
    remove_fixture(&source);
}

#[test]
fn returned_borrow_keeps_the_owner_borrowed_across_a_call() {
    let source = temporary_source(
        "call_return_liveness.aru",
        r#"module call_return_liveness

func borrowParameter(value: ref int): ref int {
    return value
}

func main(): int {
    let mut value = 42
    let borrowed = borrowParameter(value)
    value = 43
    return *borrowed
}
"#,
    );

    let output = check_source(&source);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "owner was moved while its returned borrow remained live"
    );
    assert!(
        stderr.contains("O002") || stderr.contains("O003"),
        "expected a borrow conflict after the call, got: {stderr}"
    );
    remove_fixture(&source);
}

#[test]
fn borrowed_return_provenance_forwards_through_functions() {
    let source = temporary_source(
        "forwarded_return.aru",
        r#"module forwarded_return

func borrowParameter(value: ref int): ref int {
    return value
}

func forward(value: ref int): ref int {
    return borrowParameter(value)
}

func main(): int {
    let value = 42
    let borrowed = forward(value)
    return *borrowed
}
"#,
    );

    let output = check_source(&source);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "borrow provenance did not cross the forwarding call: {stderr}"
    );
    remove_fixture(&source);
}

#[test]
fn exclusive_borrow_return_uses_the_same_provenance_contract() {
    let source = temporary_source(
        "exclusive_return.aru",
        r#"module exclusive_return

func borrowExclusive(value: mut ref int): mut ref int {
    return value
}

func main(): int {
    let mut value = 42
    let borrowed = borrowExclusive(value)
    return *borrowed
}
"#,
    );

    let output = check_source(&source);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "exclusive borrowed return lost its provenance: {stderr}"
    );
    remove_fixture(&source);
}

#[test]
fn generic_specialization_copies_the_borrow_summary() {
    let source = temporary_source(
        "generic_return.aru",
        r#"module generic_return

func identity<T>(value: ref T): ref T {
    return value
}

func main(): int {
    let value = 42
    let borrowed = identity<int>(value)
    return *borrowed
}
"#,
    );

    let output = check_source(&source);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "generic specialization lost its borrow summary: {stderr}"
    );
    remove_fixture(&source);
}
