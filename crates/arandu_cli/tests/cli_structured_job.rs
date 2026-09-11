//! Sequential prerequisite for structured jobs: statically dispatched generic
//! work with explicit context and a result larger than the i64 executor ABI.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;

mod common;

#[test]
fn static_job_preserves_aggregate_result_with_and_without_optimization() {
    let directory = common::temp_dir("arandu-static-job").unwrap();
    let source = directory.join("main.aru");
    fs::write(
        &source,
        r#"
struct Stats { code: int, comment: int, blank: int }
interface Job {
    func run(shared self): Stats
}
struct CountJob { amount: int }
func CountJob.run(shared self): Stats {
    return Stats { code: self.amount, comment: 2, blank: 3 }
}
func execute<J: Job>(job: J): Stats {
    return job.run()
}
func main(): int {
    let stats = execute<CountJob>(CountJob { amount: 37 })
    if stats.code != 37 { return 1 }
    if stats.comment != 2 { return 2 }
    if stats.blank != 3 { return 3 }
    return 0
}
"#,
    )
    .unwrap();

    for optimized in [false, true] {
        let mut command = common::cli_command();
        command.arg("run").arg(&source);
        if optimized {
            command.arg("--opt");
        }
        let output = command.output().expect("run statically dispatched job");
        assert_eq!(
            output.status.code(),
            Some(0),
            "optimized={optimized}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn owned_job_context_and_generic_result_are_destroyed_once() {
    let directory = common::temp_dir("arandu-owned-job").unwrap();
    let source = directory.join("main.aru");
    fs::write(&source, include_str!("fixtures/owned_job_lifecycle.aru")).unwrap();
    for optimized in [false, true] {
        let mut command = common::cli_command();
        command.arg("run").arg(&source);
        if optimized {
            command.arg("--opt");
        }
        let output = command.output().unwrap();
        assert_eq!(
            output.status.code(),
            Some(0),
            "optimized={optimized}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stdout).replace("\r\n", "\n"),
            "10\n30\n20\n",
            "optimized={optimized}"
        );
    }
    fs::remove_dir_all(directory).unwrap();
}
