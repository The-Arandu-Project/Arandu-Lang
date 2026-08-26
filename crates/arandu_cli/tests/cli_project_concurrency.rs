//! P7-B: project metadata and artifacts remain valid under concurrent CLI processes.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

#[test]
fn concurrent_builds_publish_one_complete_state_after_interrupted_staging() {
    let root = temp_dir("arandu-p7-build-process");
    let project = root.join("concurrent_gold");
    let created = cli(&root, &["new", "concurrent_gold"]);
    assert_success(&created, "new");

    let target = project.join("target");
    let profile_root = target.join("dev").join(host_triple());
    fs::create_dir_all(&profile_root).unwrap();
    let interrupted_state = profile_root.join("build-state.write-tmp-interrupted");
    fs::write(&interrupted_state, b"{").unwrap();

    let mut children = Vec::new();
    for _ in 0..3 {
        children.push(
            Command::new(env!("CARGO_BIN_EXE_arandu_cli"))
                .arg("build")
                .current_dir(&project)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("spawn concurrent build"),
        );
    }
    for child in children {
        let output = child.wait_with_output().unwrap();
        assert_success(&output, "concurrent build");
    }

    let states = find_named(&target, "build-state.json");
    assert_eq!(states.len(), 1, "there must be one canonical build state");
    let state: Value = serde_json::from_slice(&fs::read(&states[0]).unwrap()).unwrap();
    assert_eq!(state["schema"], 2);
    let published_root = states[0].parent().unwrap();
    let artifact = published_root.join(state["artifact"].as_str().unwrap());
    let object = published_root.join(state["object"].as_str().unwrap());
    assert!(artifact.is_file() && fs::metadata(&artifact).unwrap().len() > 0);
    assert!(object.is_file() && fs::metadata(&object).unwrap().len() > 0);

    let run = Command::new(&artifact)
        .output()
        .expect("run published artifact");
    assert_success(&run, "published artifact");
    assert!(
        interrupted_state.is_file(),
        "an interrupted staging file must never become authoritative state"
    );

    fs::remove_dir_all(root).unwrap();
}

fn host_triple() -> String {
    let arch = match std::env::consts::ARCH {
        "x86" => "i686",
        other => other,
    };
    match (arch, std::env::consts::OS) {
        (arch, "windows") => format!("{arch}-pc-windows-msvc"),
        (arch, "macos") => format!("{arch}-apple-darwin"),
        (arch, "linux") => format!("{arch}-unknown-linux-gnu"),
        (arch, os) => format!("{arch}-unknown-{os}"),
    }
}

fn cli(dir: &std::path::Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_arandu_cli"))
        .args(args)
        .current_dir(dir)
        .output()
        .expect("run Arandu CLI")
}

fn assert_success(output: &std::process::Output, operation: &str) {
    assert!(
        output.status.success(),
        "{operation} failed:\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn find_named(root: &std::path::Path, name: &str) -> Vec<std::path::PathBuf> {
    let mut found = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else if path.file_name().is_some_and(|candidate| candidate == name) {
                found.push(path);
            }
        }
    }
    found
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
