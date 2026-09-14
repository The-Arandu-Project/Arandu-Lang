#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;

use std::fs;

#[test]
fn parallel_check_emits_results_in_sorted_path_order() {
    let directory = common::temp_dir("arandu-parallel-order").expect("reserve fixture directory");
    let slow = directory.join("a_slow.aru");
    let fast = directory.join("z_fast.aru");

    let mut slow_source = String::from("module a_slow\n");
    for index in 0..256 {
        slow_source.push_str(&format!("func value{index}(): int {{ return {index} }}\n"));
    }
    fs::write(&slow, slow_source).expect("write slow fixture");
    fs::write(&fast, "module z_fast\nfunc value(): int { return 1 }\n")
        .expect("write fast fixture");

    let output = common::cli_command()
        .args(["check", directory.to_str().unwrap(), "--parallel"])
        .output()
        .expect("parallel check should run");
    fs::remove_dir_all(&directory).expect("remove fixture directory");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let lines = String::from_utf8(output.stdout)
        .expect("stdout must be UTF-8")
        .lines()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    assert_eq!(
        lines,
        vec![
            format!("ok {}", slow.display()),
            format!("ok {}", fast.display()),
        ]
    );
}
