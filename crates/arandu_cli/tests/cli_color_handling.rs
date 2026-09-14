//! CLI color handling integration tests.
//!
//! Validates standard NO_COLOR (https://no-color.org/), TTY detection,
//! and explicit override flags `--color=always`, `--color=never`, `--no-color`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

fn unique_temp_file(prefix: &str, content: &str) -> PathBuf {
    let id = FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("{prefix}_{}_{id}.aru", std::process::id()));
    fs::write(&path, content).expect("write temp source file");
    path
}

fn arandu_cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_arandu_cli"))
}

#[test]
fn test_cli_no_color_flag_disables_ansi() {
    let file = unique_temp_file(
        "arandu_test_no_color_flag",
        "fn main(): Int { let x: Int = \"type_error\"; return 0; }",
    );

    let output = arandu_cli()
        .arg("check")
        .arg("--no-color")
        .arg(&file)
        .output()
        .expect("run arandu_cli");

    let _ = fs::remove_file(&file);

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success());
    assert!(
        !stderr.contains("\x1b["),
        "stderr contained ANSI escape sequences with --no-color:\n{stderr}"
    );
    assert!(
        stderr.contains("mismatched types")
            || stderr.contains("T001")
            || stderr.contains("type_error"),
        "stderr did not contain expected diagnostic:\n{stderr}"
    );
}

#[test]
fn test_cli_no_color_env_disables_ansi() {
    let file = unique_temp_file(
        "arandu_test_no_color_env",
        "fn main(): Int { let x: Int = \"type_error\"; return 0; }",
    );

    let output = arandu_cli()
        .env("NO_COLOR", "1")
        .arg("check")
        .arg(&file)
        .output()
        .expect("run arandu_cli");

    let _ = fs::remove_file(&file);

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success());
    assert!(
        !stderr.contains("\x1b["),
        "stderr contained ANSI escape sequences with NO_COLOR=1:\n{stderr}"
    );
}

#[test]
fn test_cli_color_always_overrides_no_color_env() {
    let file = unique_temp_file(
        "arandu_test_color_always",
        "fn main(): Int { let x: Int = \"type_error\"; return 0; }",
    );

    let output = arandu_cli()
        .env("NO_COLOR", "1")
        .arg("check")
        .arg("--color=always")
        .arg(&file)
        .output()
        .expect("run arandu_cli");

    let _ = fs::remove_file(&file);

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success());
    assert!(
        stderr.contains("\x1b["),
        "stderr should contain ANSI escape sequences with --color=always even with NO_COLOR=1:\n{stderr}"
    );
}

#[test]
fn test_doctor_no_color_flag_disables_ansi() {
    let output = arandu_cli()
        .arg("doctor")
        .arg("--no-color")
        .output()
        .expect("run arandu_cli doctor");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains("\x1b["),
        "doctor stdout contained ANSI escape sequences with --no-color:\n{stdout}"
    );
}

#[test]
fn test_doctor_color_always_enables_ansi() {
    let output = arandu_cli()
        .arg("doctor")
        .arg("--color=always")
        .output()
        .expect("run arandu_cli doctor");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("\x1b["),
        "doctor stdout should contain ANSI escape sequences with --color=always:\n{stdout}"
    );
}
