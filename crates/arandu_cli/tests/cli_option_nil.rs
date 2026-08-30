//! SYN.3: `nil` as Option.None + match Some/None end-to-end.
//! SL_S thin: `import std.path` typechecks via stdlib/std rewrite.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::process::Command;

fn run_cli(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_arandu_cli"))
        .args(args)
        .output()
        .expect("cli should run")
}

fn run_cli_in(cwd: &std::path::Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_arandu_cli"))
        .current_dir(cwd)
        .args(args)
        .output()
        .expect("cli should run")
}

#[test]
fn run_option_nil_and_some_exits_42() {
    let dir = std::env::temp_dir();
    let file = dir.join("arandu_cli_option_nil.aru");
    fs::write(
        &file,
        r#"
module tests.cli.option_nil

func none_int(): Option<int> {
    return nil
}

func some_int(): Option<int> {
    return Option.Some(42)
}

func main(): int {
    let a = none_int()
    let n = match a {
        Some(v) => v
        None => 0
    }
    if n != 0 {
        return n
    }
    let b = some_int()
    return match b {
        Some(v) => v
        None => 0
    }
}
"#,
    )
    .expect("write");

    let path = file.to_string_lossy();
    let run = run_cli(&["run", &path]);
    assert_eq!(
        run.status.code(),
        Some(42),
        "option nil/some run exit, stderr={}",
        String::from_utf8_lossy(&run.stderr)
    );
}

#[test]
fn check_std_path_import_sls() {
    let dir = std::env::temp_dir();
    let file = dir.join("arandu_cli_sls_path.aru");
    fs::write(
        &file,
        r#"
module tests.cli.sls_path

import std.path as path

func main(): int {
    let _ = path.isAbsolute("/tmp")
    return 0
}
"#,
    )
    .expect("write");

    // Walk-up from workspace root finds stdlib/std/path.aru.
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root");
    let path = file.to_string_lossy();
    let check = run_cli_in(&root, &["check", &path]);
    assert!(
        check.status.success(),
        "SL_S path import check failed: {}",
        String::from_utf8_lossy(&check.stderr)
    );
}

/// Multi-file HIR linking: imported `std.path` bodies are codegen'd (not just signatures).
#[test]
fn run_std_path_is_empty_exits_0() {
    let dir = std::env::temp_dir();
    let file = dir.join("arandu_cli_sls_path_run.aru");
    fs::write(
        &file,
        r#"
module tests.cli.sls_path_run

import std.path as path

func main(): int {
    let empty = path.isEmpty("")
    let nonempty = path.isEmpty("/tmp")
    if !empty {
        return 1
    }
    if nonempty {
        return 2
    }
    let _ = path.isAbsolute("/tmp")
    return 0
}
"#,
    )
    .expect("write");

    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root");
    let path = file.to_string_lossy();
    let run = run_cli_in(&root, &["run", &path]);
    assert_eq!(
        run.status.code(),
        Some(0),
        "std.path run failed: stderr={}\nstdout={}",
        String::from_utf8_lossy(&run.stderr),
        String::from_utf8_lossy(&run.stdout)
    );
}
