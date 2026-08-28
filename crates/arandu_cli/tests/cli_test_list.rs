//! SL_T.0 end-to-end discovery through the public project CLI.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::fs;

mod common;

fn temporary_directory() -> std::path::PathBuf {
    let directory = std::env::temp_dir().join(format!(
        "arandu-test-list-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&directory).unwrap();
    directory
}

#[test]
fn test_list_uses_package_qualified_deterministic_ids() {
    let temporary = temporary_directory();
    let project = temporary.join("sample");
    let created = common::cli_command()
        .args(["new", "sample", "--vcs=none"])
        .current_dir(&temporary)
        .output()
        .expect("create project");
    assert!(
        created.status.success(),
        "new failed: {}",
        String::from_utf8_lossy(&created.stderr)
    );
    fs::write(
        project.join("src/main.aru"),
        "module sample\n\n@Test\nfunc sourceCase(): void {}\n\n@Test\nfunc resultCase(): Result<void, Err> { return nil }\n\nfunc main(): int { return 0 }\n",
    )
    .unwrap();
    fs::write(
        project.join("tests/smoke.aru"),
        "module sample_tests\n\n@Test\nfunc smoke(): void {}\n",
    )
    .unwrap();

    let listed = common::cli_command()
        .args(["test", project.to_str().unwrap(), "--list"])
        .output()
        .expect("list tests");
    assert!(
        listed.status.success(),
        "test --list failed: {}",
        String::from_utf8_lossy(&listed.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&listed.stdout),
        "sample::bin::main::resultCase\nsample::bin::main::sourceCase\nsample::test::smoke::smoke\n"
    );
    let selected = common::cli_command()
        .args([
            "test",
            project.to_str().unwrap(),
            "--list",
            "--exact",
            "sample::test::smoke::smoke",
        ])
        .output()
        .expect("select test");
    assert!(selected.status.success());
    assert_eq!(
        String::from_utf8_lossy(&selected.stdout),
        "sample::test::smoke::smoke\n"
    );
    let executed = common::cli_command()
        .args(["test", project.to_str().unwrap()])
        .output()
        .expect("run tests");
    assert!(
        executed.status.success(),
        "test run failed: {}",
        String::from_utf8_lossy(&executed.stderr)
    );
    assert!(String::from_utf8_lossy(&executed.stderr).contains("passed sample::"));
    let profile_root = project.join("target/dev");
    let harness_pointers = fs::read_dir(&profile_root)
        .expect("read target profile")
        .filter_map(Result::ok)
        .map(|entry| entry.path().join("test-harness.json"))
        .filter(|path| path.is_file())
        .collect::<Vec<_>>();
    assert!(
        harness_pointers.len() == 1,
        "expected one host-specific test harness manifest below {}, found {:?}",
        profile_root.display(),
        harness_pointers
    );
    for policy in ["--locked", "--offline", "--frozen"] {
        let checked = common::cli_command()
            .args([policy, "test", project.to_str().unwrap(), "--list"])
            .output()
            .expect("run policy test");
        assert!(
            checked.status.success(),
            "{policy} test failed: {}",
            String::from_utf8_lossy(&checked.stderr)
        );
    }
    let json = common::cli_command()
        .args([
            "test",
            project.to_str().unwrap(),
            "--format",
            "json",
            "--jobs",
            "2",
            "--seed",
            "42",
            "--timeout",
            "30",
        ])
        .output()
        .expect("run JSON test reporter");
    assert!(
        json.status.success(),
        "JSON run failed: {}",
        String::from_utf8_lossy(&json.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    assert_eq!(report["schema"], "arandu.test/v1");
    assert_eq!(report["seed"], 42);
    assert_eq!(report["cases"].as_array().map(Vec::len), Some(3));
    let _ = fs::remove_dir_all(temporary);
}

#[test]
fn test_list_rejects_an_invalid_test_contract() {
    let temporary = temporary_directory();
    let project = temporary.join("invalid_case");
    let created = common::cli_command()
        .args(["new", "invalid_case", "--vcs=none"])
        .current_dir(&temporary)
        .output()
        .expect("create project");
    assert!(created.status.success());
    fs::write(
        project.join("tests/smoke.aru"),
        "module invalid_case_tests\n\n@Test\nfunc smoke(value: int): void {}\n",
    )
    .unwrap();

    let listed = common::cli_command()
        .args(["test", project.to_str().unwrap(), "--list"])
        .output()
        .expect("list tests");
    assert_eq!(listed.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&listed.stderr).contains("T036"));
    let _ = fs::remove_dir_all(temporary);
}

#[test]
fn runner_classifies_result_error_and_timeout() {
    let temporary = temporary_directory();
    let project = temporary.join("runner_failures");
    let created = common::cli_command()
        .args(["new", "runner_failures", "--vcs=none"])
        .current_dir(&temporary)
        .output()
        .expect("create project");
    assert!(created.status.success());
    fs::write(
        project.join("tests/smoke.aru"),
        "module runner_failures_tests\n\nimport err\n\n@Test\nfunc fails(): Result<void, Err> { return Result.Err(err.new(\"boom\")) }\n",
    )
    .unwrap();
    let failed = common::cli_command()
        .args([
            "test",
            project.to_str().unwrap(),
            "--format",
            "json",
            "--exact",
            "runner_failures::test::smoke::fails",
        ])
        .output()
        .expect("run failing test");
    assert_eq!(failed.status.code(), Some(1));
    let report: serde_json::Value = serde_json::from_slice(&failed.stdout).unwrap();
    assert_eq!(report["cases"][0]["status"], "failed");

    fs::write(
        project.join("tests/smoke.aru"),
        "module runner_failures_tests\n\n@Test\nfunc hangs(): void { while true {} }\n",
    )
    .unwrap();
    let timed_out = common::cli_command()
        .args([
            "test",
            project.to_str().unwrap(),
            "--format",
            "json",
            "--timeout",
            "1",
            "--exact",
            "runner_failures::test::smoke::hangs",
        ])
        .output()
        .expect("run timed out test");
    assert_eq!(timed_out.status.code(), Some(1));
    let report: serde_json::Value = serde_json::from_slice(&timed_out.stdout).unwrap();
    assert_eq!(report["cases"][0]["status"], "timed_out");
    let _ = fs::remove_dir_all(temporary);
}
