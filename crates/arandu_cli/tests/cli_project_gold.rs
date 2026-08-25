//! P2 gold bars: project CLI (`new` / `doctor` / package check|run|build),
//! manifest errors, rebuild status line, backend convention.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::process::Command;

fn run_cli(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_arandu_cli"))
        .args(args)
        .output()
        .expect("cli should run")
}

fn run_cli_in(dir: &std::path::Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_arandu_cli"))
        .args(args)
        .current_dir(dir)
        .output()
        .expect("cli should run")
}

#[test]
fn doctor_reports_binary_and_stdlib() {
    let out = run_cli(&["doctor"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "doctor should pass in monorepo: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stdout.contains("Doctor summary"),
        "expected Flutter-style header, got:\n{stdout}"
    );
    assert!(
        stdout.contains("Arandu toolchain"),
        "expected toolchain category:\n{stdout}"
    );
    assert!(
        stdout.contains("Stdlib"),
        "expected Stdlib category:\n{stdout}"
    );
    assert!(
        stdout.contains("Cranelift") || stdout.contains("cranelift"),
        "expected Cranelift category:\n{stdout}"
    );
    assert!(
        stdout.contains("No issues found"),
        "expected clean summary:\n{stdout}"
    );
}

#[test]
fn doctor_verbose_expands_details() {
    let out = run_cli(&["doctor", "-v"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "doctor -v failed: {stdout}");
    assert!(
        stdout.contains("binary at") || stdout.contains("stdlib at"),
        "verbose should expand detail bullets:\n{stdout}"
    );
    assert!(
        stdout.contains("cascade:") || stdout.contains("relative to binary"),
        "verbose should mention resolution cascade:\n{stdout}"
    );
}

#[test]
fn new_scaffolds_package_and_check_run() {
    let tmp = tempfile_dir("arandu_new_gold");
    let name = "hello_gold";
    let project = tmp.join(name);

    let new_out = run_cli_in(&tmp, &["new", name]);
    assert!(
        new_out.status.success(),
        "new failed: {}",
        String::from_utf8_lossy(&new_out.stderr)
    );
    assert!(project.join("arandu.toml").is_file());
    assert!(project.join("src/main.aru").is_file());
    assert!(project.join("README.md").is_file());
    assert!(project.join(".gitignore").is_file());
    assert!(project.join("tests/smoke.aru").is_file());

    let check = run_cli_in(&project, &["check"]);
    assert!(
        check.status.success(),
        "project check failed: stdout={} stderr={}",
        String::from_utf8_lossy(&check.stdout),
        String::from_utf8_lossy(&check.stderr)
    );
    assert!(String::from_utf8_lossy(&check.stdout).contains("ok"));

    let run = run_cli_in(&project, &["run"]);
    assert!(
        run.status.success(),
        "project run failed: stdout={} stderr={}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(
        stderr.contains("[rebuilt:") || stderr.contains("[cached]"),
        "run must print DX.5 status line, got stderr={stderr}"
    );

    let build = run_cli_in(&project, &["build"]);
    assert!(
        build.status.success(),
        "build failed: {}",
        String::from_utf8_lossy(&build.stderr)
    );
    assert!(String::from_utf8_lossy(&build.stdout).contains("cranelift"));

    // --release is reserved for LLVM — must not silently change meaning.
    let rel = run_cli_in(&project, &["build", "--release"]);
    assert_eq!(rel.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&rel.stderr).contains("LLVM")
            || String::from_utf8_lossy(&rel.stderr).contains("llvm")
    );

    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn new_lib_and_init_create_the_requested_targets_without_git() {
    let tmp = tempfile_dir("arandu_scaffold_kinds");
    let new_lib = run_cli_in(&tmp, &["new", "math_lib", "--lib", "--vcs=none"]);
    assert!(
        new_lib.status.success(),
        "{}",
        String::from_utf8_lossy(&new_lib.stderr)
    );
    let library = tmp.join("math_lib");
    assert!(library.join("src/lib.aru").is_file());
    assert!(!library.join("src/main.aru").exists());
    assert!(!library.join(".git").exists());
    let manifest = fs::read_to_string(library.join("arandu.toml")).unwrap();
    assert!(manifest.contains("[targets.lib]"));

    let existing = tmp.join("existing_app");
    fs::create_dir(&existing).unwrap();
    fs::write(existing.join("keep.txt"), "untouched\n").unwrap();
    let init = run_cli_in(&existing, &["init", "--bin", "--vcs=none"]);
    assert!(
        init.status.success(),
        "{}",
        String::from_utf8_lossy(&init.stderr)
    );
    assert!(existing.join("arandu.toml").is_file());
    assert!(existing.join("src/main.aru").is_file());
    assert_eq!(
        fs::read_to_string(existing.join("keep.txt")).unwrap(),
        "untouched\n"
    );
    let _ = fs::remove_dir_all(tmp);
}

#[test]
fn new_classifies_usage_and_operational_failures() {
    let tmp = tempfile_dir("arandu_new_failure_classes");

    let created = run_cli_in(&tmp, &["new", "hello"]);
    assert!(created.status.success());
    let created_stdout = String::from_utf8_lossy(&created.stdout);
    assert!(created_stdout.contains("arandu check"));
    assert!(created_stdout.contains("arandu run"));
    assert!(!created_stdout.contains("arandu_cli"));

    let invalid = run_cli_in(&tmp, &["new", "../escape"]);
    assert_eq!(invalid.status.code(), Some(2));
    let invalid_stderr = String::from_utf8_lossy(&invalid.stderr);
    assert!(invalid_stderr.contains("invalid project name"));

    fs::create_dir(tmp.join("occupied")).unwrap();
    let occupied = run_cli_in(&tmp, &["new", "occupied"]);
    assert_eq!(occupied.status.code(), Some(1));
    let occupied_stderr = String::from_utf8_lossy(&occupied.stderr);
    assert!(occupied_stderr.contains("create project"));
    assert!(occupied_stderr.contains("path already exists"));

    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn new_rejects_nonportable_names_and_case_collisions() {
    let tmp = tempfile_dir("arandu_new_portable_names");
    fs::create_dir(tmp.join("Hello_App")).unwrap();

    for name in ["std", "olá", "hello world"] {
        let output = run_cli_in(&tmp, &["new", name, "--vcs=none"]);
        assert_eq!(output.status.code(), Some(1), "name `{name}` should fail");
        assert!(!tmp.join(name).exists());
    }

    let collision = run_cli_in(&tmp, &["new", "hello_app", "--vcs=none"]);
    assert_eq!(collision.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&collision.stderr).contains("only by case"),
        "{}",
        String::from_utf8_lossy(&collision.stderr)
    );
    let _ = fs::remove_dir_all(tmp);
}

#[test]
fn init_is_transactional_when_a_generated_file_exists() {
    let tmp = tempfile_dir("arandu_init_transaction");
    fs::write(tmp.join("README.md"), "keep me\n").unwrap();

    let output = run_cli_in(&tmp, &["init", "--vcs=none"]);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        fs::read_to_string(tmp.join("README.md")).unwrap(),
        "keep me\n"
    );
    assert!(!tmp.join("arandu.toml").exists());
    assert!(!tmp.join("src").exists());
    assert!(!tmp.join("tests").exists());
    let _ = fs::remove_dir_all(tmp);
}

#[test]
fn stale_interrupted_staging_does_not_block_new() {
    let tmp = tempfile_dir("arandu_new_interrupted");
    let stale = tmp.join(format!(".recoverable.arandu-new-{}", std::process::id()));
    fs::create_dir(&stale).unwrap();
    fs::write(stale.join("partial"), "incomplete\n").unwrap();

    let output = run_cli_in(&tmp, &["new", "recoverable", "--vcs=none"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(tmp.join("recoverable/arandu.toml").is_file());
    assert_eq!(
        fs::read_to_string(stale.join("partial")).unwrap(),
        "incomplete\n"
    );
    let _ = fs::remove_dir_all(tmp);
}

#[test]
fn vcs_auto_reuses_parent_and_explicit_git_creates_repository() {
    if Command::new("git").arg("--version").output().is_err() {
        return;
    }
    let tmp = tempfile_dir("arandu_new_vcs");
    assert!(
        Command::new("git")
            .arg("init")
            .arg("--quiet")
            .arg(&tmp)
            .status()
            .unwrap()
            .success()
    );

    let nested = run_cli_in(&tmp, &["new", "nested", "--vcs=auto"]);
    assert!(
        nested.status.success(),
        "{}",
        String::from_utf8_lossy(&nested.stderr)
    );
    assert!(!tmp.join("nested/.git").exists());

    let explicit = run_cli_in(&tmp, &["new", "standalone", "--vcs=git"]);
    assert!(
        explicit.status.success(),
        "{}",
        String::from_utf8_lossy(&explicit.stderr)
    );
    assert!(tmp.join("standalone/.git").is_dir());
    let _ = fs::remove_dir_all(tmp);
}

#[test]
fn package_local_multi_file_check_and_run() {
    let tmp = tempfile_dir("arandu_l2_pkg");
    let name = "pkg_l2";
    let project = tmp.join(name);

    assert!(run_cli_in(&tmp, &["new", name]).status.success());

    // Add util module next to main (package src root).
    fs::write(
        project.join("src/util.aru"),
        r#"
public func answer(): int {
    return 42
}
"#,
    )
    .unwrap();
    fs::write(
        project.join("src/main.aru"),
        format!(
            r#"
module {name}
import {name}.util as util
func main(): int {{
    return util.answer()
}}
"#
        ),
    )
    .unwrap();

    let check = run_cli_in(&project, &["check"]);
    assert!(
        check.status.success(),
        "L2 package check failed: {}",
        String::from_utf8_lossy(&check.stderr)
    );

    let run = run_cli_in(&project, &["run"]);
    // main returns 42 — process exit code is 42 (not OS "success").
    assert_eq!(
        run.status.code(),
        Some(42),
        "L2 package run exit: stdout={} stderr={}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );

    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn reserved_package_name_std_rejected() {
    let tmp = tempfile_dir("arandu_reserved_std");
    fs::write(
        tmp.join("Arandu.toml"),
        r#"
name = "std"
version = "0.0.1"
entry = "src/main.aru"
"#,
    )
    .unwrap();
    fs::create_dir_all(tmp.join("src")).unwrap();
    fs::write(tmp.join("src/main.aru"), "func main(): int { return 0 }\n").unwrap();
    let out = run_cli_in(&tmp, &["check"]);
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("reserved") || err.contains("std"),
        "expected reserved name error, got: {err}"
    );
    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn malformed_manifest_is_hard_error() {
    let tmp = tempfile_dir("arandu_bad_manifest");
    fs::write(tmp.join("Arandu.toml"), "this is not valid toml {{{\n").unwrap();
    let out = run_cli_in(&tmp, &["check"]);
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("malformed") || err.contains("failed") || err.contains("error"),
        "expected parse error, got: {err}"
    );
    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn legacy_manifest_loads_with_migration_warning() {
    let tmp = tempfile_dir("arandu_legacy_manifest");
    fs::create_dir_all(tmp.join("src")).unwrap();
    fs::write(
        tmp.join("Arandu.toml"),
        "name = \"legacy_app\"\nversion = \"0.1.0\"\nentry = \"src/main.aru\"\n",
    )
    .unwrap();
    fs::write(tmp.join("src/main.aru"), "func main(): int { return 0 }\n").unwrap();

    let out = run_cli_in(&tmp, &["check"]);
    assert!(out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("deprecated"));
    assert!(stderr.contains("arandu.toml"));
    let _ = fs::remove_dir_all(tmp);
}

#[test]
fn missing_entry_field_is_hard_error() {
    let tmp = tempfile_dir("arandu_missing_entry");
    fs::write(
        tmp.join("Arandu.toml"),
        "name = \"x\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    let out = run_cli_in(&tmp, &["check"]);
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("entry") || err.contains("missing"),
        "expected missing entry, got: {err}"
    );
    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn single_file_run_prints_status_line() {
    let tmp = tempfile_dir("arandu_run_status");
    let file = tmp.join("main.aru");
    fs::write(
        &file,
        r#"module t
func main(): int {
    return 0
}
"#,
    )
    .unwrap();
    let out = run_cli(&["run", &file.to_string_lossy()]);
    assert!(
        out.status.success(),
        "run failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("[rebuilt:") || stderr.contains("[cached]"),
        "expected status line in stderr, got: {stderr}"
    );
    let _ = fs::remove_dir_all(&tmp);
}

fn tempfile_dir(prefix: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "{prefix}_{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}
