//! JSON serialization and atomic file emission for tests and benchmarks.

use arandu_codegen::testing::{BENCH_PROTOCOL_V1, BenchmarkEventV1, TEST_PROTOCOL_V1, TestEventV1};
use std::path::Path;

use super::human::status_name;
use crate::test_runner::process::atomic_write_file;
use crate::test_runner::statistics::{benchmark_stats, percentile, sample_values};
use crate::test_runner::types::{
    BenchmarkJsonCase, BenchmarkJsonReport, BenchmarkRunnerOptions, JsonCase, JsonReport,
    JsonSummary, RunnerOptions,
};

pub fn report_tests_json(
    events: &[TestEventV1],
    options: &RunnerOptions,
    summary: JsonSummary,
) -> Result<(), String> {
    let cases = events
        .iter()
        .map(|event| JsonCase {
            id: &event.id,
            status: status_name(event.status),
            duration_ms: event.duration.as_millis(),
            stdout: String::from_utf8_lossy(&event.stdout.bytes).into_owned(),
            stderr: String::from_utf8_lossy(&event.stderr.bytes).into_owned(),
            stdout_truncated: event.stdout.truncated,
            stderr_truncated: event.stderr.truncated,
            failure: event.failure.clone(),
            secondary_failures: event.secondary_failures.clone(),
            logs: event.logs.clone(),
            logs_truncated: event.logs_truncated,
        })
        .collect();

    let report_obj = JsonReport {
        schema: TEST_PROTOCOL_V1,
        target: options
            .target
            .clone()
            .unwrap_or_else(|| std::env::consts::ARCH.to_string()),
        backend: options
            .backend
            .clone()
            .unwrap_or_else(|| "cranelift".to_string()),
        seed: options.seed,
        jobs: options.jobs,
        timeout_ms: options.timeout.as_millis() as u64,
        fail_fast: options.fail_fast,
        summary,
        cases,
    };

    let encoded = serde_json::to_vec_pretty(&report_obj).map_err(|error| error.to_string())?;

    if let Some(output) = &options.output {
        atomic_write_file(output, &encoded)?;
    } else {
        println!("{}", String::from_utf8_lossy(&encoded));
    }
    Ok(())
}

pub fn build_benchmark_json_report(
    events: &[BenchmarkEventV1],
    options: &BenchmarkRunnerOptions,
    cpu_identity: String,
) -> BenchmarkJsonReport {
    let cases = events
        .iter()
        .map(|event| {
            let values = sample_values(event);
            let (median, mad, p95) = benchmark_stats(event);
            BenchmarkJsonCase {
                id: event.id.clone(),
                config: event.config.clone(),
                samples: event.samples.clone(),
                median_ns_per_op: median,
                mad_ns_per_op: mad,
                p50_ns_per_op: percentile(&values, 50),
                p95_ns_per_op: p95,
                min_ns_per_op: values.first().copied(),
                stdout: String::from_utf8_lossy(&event.stdout.bytes).into_owned(),
                stderr: String::from_utf8_lossy(&event.stderr.bytes).into_owned(),
                stdout_truncated: event.stdout.truncated,
                stderr_truncated: event.stderr.truncated,
                failure: event.failure.clone(),
            }
        })
        .collect();

    BenchmarkJsonReport {
        schema: BENCH_PROTOCOL_V1.to_string(),
        arandu_version: env!("CARGO_PKG_VERSION").to_string(),
        target: options
            .target
            .clone()
            .unwrap_or_else(|| std::env::consts::ARCH.to_string()),
        backend: options
            .backend
            .clone()
            .unwrap_or_else(|| "cranelift".to_string()),
        profile: if cfg!(debug_assertions) {
            "debug".to_string()
        } else {
            "release".to_string()
        },
        clock: "monotonic_instant".to_string(),
        os: std::env::consts::OS.to_string(),
        arch: std::env::consts::ARCH.to_string(),
        cpu: cpu_identity,
        cases,
        comparison: None,
    }
}

pub fn emit_benchmark_json(
    report: &BenchmarkJsonReport,
    output: Option<&Path>,
) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(report).map_err(|error| error.to_string())?;
    if let Some(output) = output {
        atomic_write_file(output, &bytes)?;
    } else {
        println!("{}", String::from_utf8_lossy(&bytes));
    }
    Ok(())
}
