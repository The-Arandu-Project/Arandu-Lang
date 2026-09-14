//! Integration tests for Phase 1: Partitioned Codegen in Cranelift (CGUs) & Machine Code Caching.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

mod common;

fn run_cli_in(dir: &Path, args: &[&str]) -> std::process::Output {
    common::cli_command()
        .args(args)
        .current_dir(dir)
        .output()
        .expect("cli should run")
}

fn files_in_dir(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            if path.is_file()
                && matches!(
                    path.extension().and_then(|value| value.to_str()),
                    Some("o" | "obj")
                )
            {
                files.push(path);
            }
        }
    }
    files.sort();
    files
}

#[test]
fn corrupted_cgu_object_is_recompiled_instead_of_reused() {
    let tmp = common::temp_dir("arandu_cgu_corruption_test").unwrap();
    let project = tmp.join("cgu_corruption");
    let init = run_cli_in(&tmp, &["new", "cgu_corruption", "--bin", "--vcs=none"]);
    assert!(init.status.success());

    let source = project.join("src/main.aru");
    fs::write(&source, "func main(): int { return 1 }\n").unwrap();
    let cold = run_cli_in(&project, &["build", "-v"]);
    assert!(cold.status.success());
    let state = files_named(&project.join("target/dev"), "build-state.json");
    let cgu_dir = state[0].parent().unwrap().join("incremental/cgu");
    let object = files_in_dir(&cgu_dir).pop().expect("one CGU object");
    let mut corrupted = fs::read(&object).unwrap();
    corrupted[0] ^= 0xff;
    fs::write(&object, corrupted).unwrap();

    fs::write(&source, "func main(): int { return 2 }\n").unwrap();
    let rebuilt = run_cli_in(&project, &["build", "-v"]);
    assert!(
        rebuilt.status.success(),
        "rebuild failed: {}",
        String::from_utf8_lossy(&rebuilt.stderr)
    );
    assert!(
        String::from_utf8_lossy(&rebuilt.stderr).contains("[cgu] 1 units: 0 cached, 1 recompiled")
    );

    let _ = fs::remove_dir_all(tmp);
}

#[test]
fn partitioned_cgu_compiles_caches_and_links_multicgu_project() {
    let tmp = common::temp_dir("arandu_cgu_test").unwrap();
    let project = tmp.join("cgu_app");

    // 1. Create a new package
    let init = run_cli_in(&tmp, &["new", "cgu_app", "--bin", "--vcs=none"]);
    assert!(
        init.status.success(),
        "new failed: {}",
        String::from_utf8_lossy(&init.stderr)
    );

    // 2. Write multi-function source
    let src_v1 = r#"module cgu_app

func helper(): int {
    return 10;
}

func calculate(): int {
    return helper() + 32;
}

func main(): int {
    return calculate();
}
"#;
    fs::write(project.join("src/main.aru"), src_v1).unwrap();

    // 3. Cold build
    let build1 = run_cli_in(&project, &["build", "-v"]);
    assert!(
        build1.status.success(),
        "build 1 failed:\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&build1.stdout),
        String::from_utf8_lossy(&build1.stderr)
    );
    let stderr1 = String::from_utf8_lossy(&build1.stderr);
    assert!(
        stderr1.contains("[cgu] 3 units: 0 cached, 3 recompiled"),
        "expected all 3 CGUs to be compiled cold, got: {stderr1}"
    );

    // 4. Verify executable runs and returns 42
    let build_states = files_named(&project.join("target/dev"), "build-state.json");
    assert_eq!(build_states.len(), 1);
    let state: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&build_states[0]).unwrap()).unwrap();
    let bin_path = build_states[0]
        .parent()
        .unwrap()
        .join(state["artifact"].as_str().unwrap());
    assert!(bin_path.is_file());
    let run1 = Command::new(&bin_path).output().unwrap();
    assert_eq!(run1.status.code(), Some(42));

    // Verify incremental CGU cache directory exists with 3 objects
    let cgu_dir = build_states[0].parent().unwrap().join("incremental/cgu");
    assert!(cgu_dir.is_dir());
    let cgus1 = files_in_dir(&cgu_dir);
    assert_eq!(
        cgus1.len(),
        3,
        "expected exactly 3 CGU files for 3 functions"
    );
    assert!(
        cgus1
            .iter()
            .any(|p| p.file_name().unwrap().to_string_lossy().contains("main"))
    );
    assert!(
        cgus1
            .iter()
            .any(|p| p.file_name().unwrap().to_string_lossy().contains("helper"))
    );
    assert!(cgus1.iter().any(|p| {
        p.file_name()
            .unwrap()
            .to_string_lossy()
            .contains("calculate")
    }));

    // 5. Build without changes -> Phase 0 early cutoff
    let build2 = run_cli_in(&project, &["build"]);
    assert!(build2.status.success());
    assert!(String::from_utf8_lossy(&build2.stdout).contains("incremental: up-to-date"));

    // 6. Modify ONLY `calculate()` function body.
    // Notice `helper()` and `main()` are NOT changed!
    let src_v2 = r#"module cgu_app

func helper(): int {
    return 10;
}

func calculate(): int {
    return helper() + 90;
}

func main(): int {
    return calculate();
}
"#;
    fs::write(project.join("src/main.aru"), src_v2).unwrap();

    // 7. Incremental rebuild
    let build3 = run_cli_in(&project, &["build", "-v"]);
    assert!(
        build3.status.success(),
        "build 3 failed:\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&build3.stdout),
        String::from_utf8_lossy(&build3.stderr)
    );
    let stderr3 = String::from_utf8_lossy(&build3.stderr);
    assert!(
        stderr3.contains("[cgu] 3 units: 2 cached, 1 recompiled"),
        "expected 2 CGUs cached and 1 recompiled, got: {stderr3}"
    );

    // 8. Run updated binary -> should now return 10 + 90 = 100
    let state3: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&build_states[0]).unwrap()).unwrap();
    let bin_path3 = build_states[0]
        .parent()
        .unwrap()
        .join(state3["artifact"].as_str().unwrap());
    let run3 = Command::new(&bin_path3).output().unwrap();
    assert_eq!(run3.status.code(), Some(100));

    // CGU directory should now have 4 CGU files:
    // (main, helper, calculate_v1, calculate_v2)
    let cgus3 = files_in_dir(&cgu_dir);
    assert_eq!(cgus3.len(), 4, "expected 4 CGU files in cache");

    // 9. A newly discovered but unreachable module changes the input closure,
    // yet leaves every emitted CGU unchanged. The verified executable should
    // be reused without invoking the linker again.
    fs::write(
        project.join("src/unused.aru"),
        "public func unused(): int { return 7; }\n",
    )
    .unwrap();
    let previous_artifact = state3["artifact"].as_str().unwrap().to_owned();
    let previous_digest = state3["artifact_digest"].as_str().unwrap().to_owned();
    let build4 = run_cli_in(&project, &["build", "-v"]);
    assert!(
        build4.status.success(),
        "build 4 failed:\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&build4.stdout),
        String::from_utf8_lossy(&build4.stderr)
    );
    let stderr4 = String::from_utf8_lossy(&build4.stderr);
    assert!(stderr4.contains("[cgu] 3 units: 3 cached, 0 recompiled"));
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        assert!(stderr4.contains("[incremental-artifact] all CGUs are verified cache hits"));
        assert!(String::from_utf8_lossy(&build4.stdout).contains("backend=incremental-reuse"));
    }
    #[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
    assert!(String::from_utf8_lossy(&build4.stdout).contains("backend=cranelift-dev"));

    let state4: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&build_states[0]).unwrap()).unwrap();
    assert_eq!(
        state4["artifact"].as_str(),
        Some(previous_artifact.as_str())
    );
    assert_eq!(
        state4["artifact_digest"].as_str(),
        Some(previous_digest.as_str())
    );
    assert_eq!(Command::new(&bin_path3).status().unwrap().code(), Some(100));

    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn removing_a_cached_cgu_forces_relink_instead_of_reusing_stale_executable() {
    let tmp = common::temp_dir("arandu_cgu_removal_test").unwrap();
    let project = tmp.join("cgu_removal");
    assert!(
        run_cli_in(&tmp, &["new", "cgu_removal", "--bin", "--vcs=none"])
            .status
            .success()
    );
    let source = project.join("src/main.aru");
    fs::write(
        &source,
        "func main(): int { return 1; }\nfunc unused(): int { return 9; }\n",
    )
    .unwrap();
    assert!(run_cli_in(&project, &["build", "-v"]).status.success());

    fs::write(&source, "func main(): int { return 1; }\n").unwrap();
    let rebuilt = run_cli_in(&project, &["build", "-v"]);
    assert!(
        rebuilt.status.success(),
        "relink after CGU removal failed: {}",
        String::from_utf8_lossy(&rebuilt.stderr)
    );
    assert!(
        String::from_utf8_lossy(&rebuilt.stderr).contains("[cgu] 1 units: 0 cached, 1 recompiled")
    );
    assert!(String::from_utf8_lossy(&rebuilt.stdout).contains("backend=cranelift-dev"));

    let state_path = files_named(&project.join("target/dev"), "build-state.json")[0].clone();
    let state: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&state_path).unwrap()).unwrap();
    let executable = state_path
        .parent()
        .unwrap()
        .join(state["artifact"].as_str().unwrap());
    assert_eq!(Command::new(executable).status().unwrap().code(), Some(1));

    let _ = fs::remove_dir_all(tmp);
}

fn files_named(root: &Path, name: &str) -> Vec<PathBuf> {
    let mut files = Vec::new();
    if let Ok(entries) = fs::read_dir(root) {
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            if path.is_dir() {
                files.extend(files_named(&path, name));
            } else if path.file_name().and_then(|n| n.to_str()) == Some(name) {
                files.push(path);
            }
        }
    }
    files
}
