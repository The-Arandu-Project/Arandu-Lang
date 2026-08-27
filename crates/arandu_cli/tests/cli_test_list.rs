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
        "module sample\n\n@Test\nfunc sourceCase(): void {}\n\nfunc main(): int { return 0 }\n",
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
        "sample::bin::main::sourceCase\nsample::test::smoke::smoke\n"
    );
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
