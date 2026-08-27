//! P7-D: project roots never follow a source directory link outside the package.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

#[test]
fn check_rejects_source_directory_symlink_escape() {
    let root = temp_dir("arandu-p7-path-escape");
    let project = root.join("escape_gold");
    let outside = root.join("outside");
    run(&root, &["new", "escape_gold", "--vcs=none"]);
    fs::create_dir_all(outside.join("src")).unwrap();
    fs::write(
        outside.join("src/main.aru"),
        "module escape_gold\nfunc main(): int { return 0 }\n",
    )
    .unwrap();
    fs::remove_dir_all(project.join("src")).unwrap();
    link_dir(&outside.join("src"), &project.join("src"));

    let output = Command::new(env!("CARGO_BIN_EXE_arandu_cli"))
        .args(["check"])
        .current_dir(&project)
        .output()
        .unwrap();
    assert!(!output.status.success(), "source escape must fail closed");
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(
        error.contains("escapes the project root") || error.contains("outside"),
        "unexpected escape diagnostic: {error}"
    );
    remove_link_dir(&project.join("src"));
    fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
fn remove_link_dir(path: &std::path::Path) {
    fs::remove_file(path).unwrap();
}

#[cfg(windows)]
fn remove_link_dir(path: &std::path::Path) {
    fs::remove_dir(path).unwrap();
}

#[cfg(unix)]
fn link_dir(source: &std::path::Path, destination: &std::path::Path) {
    std::os::unix::fs::symlink(source, destination).unwrap();
}

#[cfg(windows)]
fn link_dir(source: &std::path::Path, destination: &std::path::Path) {
    let output = Command::new("cmd")
        .args(["/c", "mklink", "/J"])
        .arg(destination)
        .arg(source)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "could not create test junction: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn run(dir: &std::path::Path, args: &[&str]) {
    let output = Command::new(env!("CARGO_BIN_EXE_arandu_cli"))
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn temp_dir(prefix: &str) -> std::path::PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!("{prefix}-{}-{nonce}", std::process::id()));
    fs::create_dir_all(&path).unwrap();
    path
}
