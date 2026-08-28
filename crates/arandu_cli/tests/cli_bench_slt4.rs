//! SL_T.4 benchmark discovery, calibrated execution and JSON contract.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::fs;

mod common;

fn temp_dir() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "arandu-slt4-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn bench_lists_and_emits_auditable_json_samples() {
    let tmp = temp_dir();
    let created = common::cli_command()
        .args(["new", "bench_gold", "--vcs=none"])
        .current_dir(&tmp)
        .output()
        .expect("create project");
    assert!(created.status.success());
    let project = tmp.join("bench_gold");
    fs::write(
        project.join("src/main.aru"),
        r#"module bench_gold

import std.testing as testing

@Benchmark
func integerBarrier(mut bench: testing.Benchmark): void {
    while bench.loop() {
        let integer = testing.blackBox<int>(40 + 2)
        let boolean = testing.blackBox<bool>(true)
        let text = testing.blackBox<str>("arandu")
    }
}

func main(): int { return 0 }
"#,
    )
    .unwrap();

    let listed = common::cli_command()
        .args(["bench", project.to_str().unwrap(), "--list"])
        .output()
        .expect("list benchmarks");
    assert!(
        listed.status.success(),
        "{}",
        String::from_utf8_lossy(&listed.stderr)
    );
    let id = "bench_gold::bin::main::integerBarrier";
    assert!(String::from_utf8_lossy(&listed.stdout).contains(id));

    let output = common::cli_command()
        .args([
            "bench",
            project.to_str().unwrap(),
            "--exact",
            id,
            "--warmup",
            "0.001",
            "--measurement-time",
            "0.005",
            "--samples",
            "10",
            "--format",
            "json",
        ])
        .output()
        .expect("run benchmark");
    assert!(
        output.status.success(),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).expect("valid JSON");
    assert_eq!(report["schema"], "arandu.bench/v1");
    assert!(report["arandu_version"].is_string());
    assert_eq!(report["clock"], "monotonic_instant");
    assert!(report["profile"].is_string());
    let case = &report["cases"][0];
    assert_eq!(case["id"], id);
    assert_eq!(case["samples"].as_array().unwrap().len(), 10);
    assert!(case["samples"].as_array().unwrap().iter().all(|sample| {
        sample["iterations"].as_u64().is_some_and(|value| value > 0)
            && sample["elapsed_ns"].as_u64().is_some()
    }));
    assert!(case["median_ns_per_op"].as_f64().is_some());
    assert!(case["stdout"].is_string());
    assert!(case["stderr"].is_string());
    assert_eq!(case["stdout_truncated"], false);
    assert_eq!(case["stderr_truncated"], false);

    let _ = fs::remove_dir_all(tmp);
}
