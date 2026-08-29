//! Human-readable console reporting for tests and benchmarks.

use arandu_codegen::testing::{BenchmarkEventV1, TestEventV1, TestStatus};

use crate::test_runner::statistics::{benchmark_stats, format_ns_per_op};
use crate::test_runner::types::{
    BenchmarkComparisonReport, BenchmarkComparisonStatus, JsonSummary,
};

pub fn report_tests_human(events: &[TestEventV1], summary: JsonSummary) {
    for event in events {
        eprintln!(
            "{} {} ({:?})",
            status_name(event.status),
            event.id,
            event.duration
        );
        if let Some(failure) = &event.failure {
            if let Some(loc) = &failure.location {
                eprintln!("    location: {loc}");
            }
            if let (Some(exp), Some(act)) = (&failure.expected, &failure.actual) {
                eprintln!("    expected: `{exp}`");
                eprintln!("    actual:   `{act}`");
            }
            if !failure.message.is_empty() {
                eprintln!("    message:  {}", failure.message);
            }
        }
        for failure in &event.secondary_failures {
            eprintln!("    secondary: {}", failure.message);
        }
        for log in &event.logs {
            eprintln!("    log: {log}");
        }
        if event.logs_truncated {
            eprintln!("    log: <truncated>");
        }
    }
    eprintln!(
        "test result: {}. {} passed; {} failed; {} skipped; {} timed out; {} crashed",
        if summary.failed == 0 && summary.timed_out == 0 && summary.crashed == 0 {
            "ok"
        } else {
            "FAILED"
        },
        summary.passed,
        summary.failed,
        summary.skipped,
        summary.timed_out,
        summary.crashed,
    );
}

pub fn report_benchmarks_human(
    events: &[BenchmarkEventV1],
    comparison: Option<&BenchmarkComparisonReport>,
) {
    for event in events {
        if let Some(failure) = &event.failure {
            eprintln!("FAILED {}: {failure}", event.id);
            continue;
        }
        let (median, mad, p95) = benchmark_stats(event);
        let median = median.unwrap_or(0.0);
        let mad = mad.unwrap_or(0.0);
        eprintln!(
            "bench {}: median {}; MAD {}; p95 {}; {} samples",
            event.id,
            format_ns_per_op(median),
            format_ns_per_op(mad),
            format_ns_per_op(p95.unwrap_or(0.0)),
            event.samples.len()
        );
        if median > 0.0 && mad / median > 0.10 {
            eprintln!(
                "warning: benchmark {} is noisy (MAD exceeds 10% of median)",
                event.id
            );
        }
    }
    if let Some(comparison) = comparison {
        if comparison.status == BenchmarkComparisonStatus::Compared {
            for case in &comparison.cases {
                eprintln!(
                    "compare {}: {:+.2}% ({:?}, threshold {:.2}%, uncertainty {:.2}%)",
                    case.id,
                    case.delta_percent,
                    case.classification,
                    case.effective_threshold_percent,
                    case.uncertainty_percent
                );
            }
            eprintln!(
                "benchmark comparison: {} regression(s) against baseline `{}`{}",
                comparison.regressions,
                comparison.baseline,
                if comparison.dry_run { " (dry-run)" } else { "" }
            );
        } else {
            eprintln!(
                "benchmark comparison: {:?} for baseline `{}`",
                comparison.status, comparison.baseline
            );
        }
    }
}

pub fn status_name(status: TestStatus) -> &'static str {
    match status {
        TestStatus::Passed => "passed",
        TestStatus::Failed => "failed",
        TestStatus::Skipped => "skipped",
        TestStatus::TimedOut => "timed_out",
        TestStatus::Crashed => "crashed",
    }
}
