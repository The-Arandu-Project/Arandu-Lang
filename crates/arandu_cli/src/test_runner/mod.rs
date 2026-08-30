//! SL_T.2 process-isolated test coordinator with framed IPC, process-tree termination,
//! and deterministic reporting.

pub mod baseline;
pub mod benchmark;
pub mod ipc;
pub mod process;
pub mod reporters;
pub mod statistics;
pub mod types;

#[allow(unused_imports)]
pub use benchmark::run_benchmarks;
#[allow(unused_imports)]
pub use ipc::{send_benchmark_child_event, send_child_event};
#[allow(unused_imports)]
pub use process::{atomic_write_file, deterministic_shuffle, install_ctrlc_handler, run_cases};
#[allow(unused_imports)]
pub use types::{
    BenchmarkBaselineMode, BenchmarkRunOutcome, BenchmarkRunnerOptions, RunnerOptions,
    TestOutputFormat,
};

#[cfg(test)]
mod tests {
    use super::baseline::{compare_benchmark_reports, validate_baseline_name};
    use super::benchmark::benchmark_json_report;
    use super::reporters::junit::junit_report;
    use super::statistics::{benchmark_stats, format_ns_per_op};
    use super::types::{BenchmarkClassification, BenchmarkRunnerOptions};
    use arandu_codegen::testing::{
        BenchmarkConfigV1, BenchmarkEventV1, CapturedOutput, TestEventV1, TestFailure, TestStatus,
    };
    use std::time::Duration;

    fn event(values: &[(u64, u64)]) -> BenchmarkEventV1 {
        BenchmarkEventV1 {
            sequence: 0,
            id: "pkg::bin::bench".to_string(),
            config: BenchmarkConfigV1 {
                warmup_ns: 1,
                measurement_ns: 1,
                samples: u32::try_from(values.len()).unwrap(),
            },
            samples: values
                .iter()
                .map(
                    |(iterations, elapsed_ns)| arandu_codegen::testing::BenchmarkSampleV1 {
                        iterations: *iterations,
                        elapsed_ns: *elapsed_ns,
                    },
                )
                .collect(),
            stdout: CapturedOutput::default(),
            stderr: CapturedOutput::default(),
            failure: None,
        }
    }

    #[test]
    fn benchmark_statistics_use_true_median_and_mad() {
        let event = event(&[(1, 1), (1, 2), (1, 100), (1, 200)]);
        let (median, mad, p95) = benchmark_stats(&event);
        assert_eq!(median, Some(51.0));
        assert_eq!(mad, Some(49.5));
        assert_eq!(p95, Some(200.0));
    }

    #[test]
    fn benchmark_human_units_scale_without_locale_state() {
        assert_eq!(format_ns_per_op(42.0), "42.000 ns/op");
        assert_eq!(format_ns_per_op(2_500.0), "2.500 us/op");
        assert_eq!(format_ns_per_op(3_000_000.0), "3.000 ms/op");
    }

    #[test]
    fn comparison_uses_noise_as_a_regression_floor() {
        let options = BenchmarkRunnerOptions {
            timeout: Duration::from_secs(1),
            config: BenchmarkConfigV1 {
                warmup_ns: 1,
                measurement_ns: 1,
                samples: 3,
            },
            format_json: true,
            output: None,
            target: Some("test-target".to_string()),
            backend: Some("test-backend".to_string()),
            baseline: None,
        };
        let baseline = benchmark_json_report(&[event(&[(1, 95), (1, 100), (1, 105)])], &options);
        let current = benchmark_json_report(&[event(&[(1, 100), (1, 106), (1, 112)])], &options);
        let comparison =
            compare_benchmark_reports("main", false, false, 5.0, 1.0, &baseline, &current)
                .expect("comparison");
        assert_eq!(comparison.regressions, 0);
        assert_eq!(
            comparison.cases[0].classification,
            BenchmarkClassification::Unchanged
        );
        assert!(comparison.cases[0].uncertainty_percent > 5.0);
    }

    #[test]
    fn junit_distinguishes_assertion_infrastructure_and_xml_escapes() {
        let events = vec![
            TestEventV1 {
                sequence: 0,
                id: "pkg::suite::assert<&>".to_string(),
                status: TestStatus::Failed,
                duration: Duration::from_millis(5),
                stdout: CapturedOutput {
                    bytes: b"left < right & safe".to_vec(),
                    truncated: false,
                },
                stderr: CapturedOutput::default(),
                failure: Some(TestFailure::simple("expected <actual> & value")),
                secondary_failures: Vec::new(),
                logs: Vec::new(),
                logs_truncated: false,
            },
            TestEventV1 {
                sequence: 1,
                id: "pkg::suite::timeout".to_string(),
                status: TestStatus::TimedOut,
                duration: Duration::from_millis(10),
                stdout: CapturedOutput::default(),
                stderr: CapturedOutput::default(),
                failure: Some(TestFailure::simple("deadline")),
                secondary_failures: Vec::new(),
                logs: Vec::new(),
                logs_truncated: false,
            },
        ];
        let xml = junit_report(&events, 15);
        assert!(xml.contains("failures=\"1\" errors=\"1\""));
        assert!(xml.contains("<failure type=\"assertion\""));
        assert!(xml.contains("<error type=\"timeout\""));
        assert!(xml.contains("assert&lt;&amp;&gt;"));
        assert!(xml.contains("left &lt; right &amp; safe"));
    }

    #[test]
    fn baseline_names_cannot_escape_the_project_target() {
        assert!(validate_baseline_name("main-linux-x64").is_ok());
        assert!(validate_baseline_name("../main").is_err());
        assert!(validate_baseline_name("a/b").is_err());
        assert!(validate_baseline_name("..").is_err());
    }
}
