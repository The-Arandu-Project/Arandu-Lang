//! Baseline recording, loading, comparison, and regression detection for benchmarks.

use arandu_codegen::testing::BENCH_BASELINE_PROTOCOL_V1;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::test_runner::process::atomic_write_file;
use crate::test_runner::types::{
    BenchmarkBaselineFile, BenchmarkBaselineMode, BenchmarkClassification, BenchmarkComparisonCase,
    BenchmarkComparisonReport, BenchmarkComparisonStatus, BenchmarkJsonReport, BenchmarkRunOutcome,
    BenchmarkRunnerOptions,
};

pub fn validate_baseline_name(name: &str) -> Result<(), String> {
    if name.is_empty()
        || name.len() > 64
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        || matches!(name, "." | "..")
    {
        return Err(
            "baseline name must contain 1-64 ASCII letters, digits, '.', '-' or '_'".to_string(),
        );
    }
    Ok(())
}

pub fn baseline_path(project: &Path, name: &str) -> PathBuf {
    project
        .join("target")
        .join("arandu")
        .join("benchmarks")
        .join(format!("{name}.json"))
}

pub fn apply_baseline(
    project: &Path,
    current: &mut BenchmarkJsonReport,
    options: &BenchmarkRunnerOptions,
) -> Result<BenchmarkRunOutcome, String> {
    let Some(mode) = &options.baseline else {
        return Ok(BenchmarkRunOutcome::Passed);
    };
    match mode {
        BenchmarkBaselineMode::Save { name } => {
            validate_baseline_name(name)?;
            let path = baseline_path(project, name);
            let parent = path
                .parent()
                .ok_or_else(|| "baseline path has no parent directory".to_string())?;
            fs::create_dir_all(parent).map_err(|error| {
                format!(
                    "create benchmark baseline directory {}: {error}",
                    parent.display()
                )
            })?;
            let document = BenchmarkBaselineFile {
                schema: BENCH_BASELINE_PROTOCOL_V1.to_string(),
                name: name.clone(),
                report: current.clone(),
            };
            let bytes = serde_json::to_vec_pretty(&document).map_err(|error| error.to_string())?;
            atomic_write_file(&path, &bytes)?;
            if !options.format_json {
                eprintln!("saved benchmark baseline `{name}` at {}", path.display());
            }
            Ok(BenchmarkRunOutcome::Passed)
        }
        BenchmarkBaselineMode::Compare {
            name,
            strict,
            dry_run,
            max_regression_percent,
            noise_threshold_percent,
        } => {
            validate_baseline_name(name)?;
            let path = baseline_path(project, name);
            let bytes = match fs::read(&path) {
                Ok(bytes) => bytes,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound && !strict => {
                    current.comparison = Some(empty_comparison(
                        name,
                        BenchmarkComparisonStatus::MissingBaseline,
                        *strict,
                        *dry_run,
                        *max_regression_percent,
                        *noise_threshold_percent,
                    ));
                    if !options.format_json {
                        eprintln!("benchmark baseline `{name}` does not exist; comparison skipped");
                    }
                    return Ok(BenchmarkRunOutcome::Passed);
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    current.comparison = Some(empty_comparison(
                        name,
                        BenchmarkComparisonStatus::MissingBaseline,
                        *strict,
                        *dry_run,
                        *max_regression_percent,
                        *noise_threshold_percent,
                    ));
                    if !options.format_json {
                        eprintln!("benchmark baseline `{name}` is required but missing");
                    }
                    return Ok(BenchmarkRunOutcome::MissingBaseline);
                }
                Err(error) => {
                    return Err(format!(
                        "read benchmark baseline {}: {error}",
                        path.display()
                    ));
                }
            };
            let baseline: BenchmarkBaselineFile =
                serde_json::from_slice(&bytes).map_err(|error| {
                    format!("invalid benchmark baseline {}: {error}", path.display())
                })?;
            if baseline.schema != BENCH_BASELINE_PROTOCOL_V1 || baseline.name != *name {
                return Err(format!(
                    "benchmark baseline {} has an invalid contract",
                    path.display()
                ));
            }
            if baseline.report.environment() != current.environment() {
                current.comparison = Some(empty_comparison(
                    name,
                    BenchmarkComparisonStatus::IncompatibleEnvironment,
                    *strict,
                    *dry_run,
                    *max_regression_percent,
                    *noise_threshold_percent,
                ));
                if !options.format_json {
                    eprintln!(
                        "benchmark baseline `{name}` is incompatible: baseline={:?}, current={:?}",
                        baseline.report.environment(),
                        current.environment()
                    );
                }
                return Ok(BenchmarkRunOutcome::IncompatibleBaseline);
            }
            let comparison = compare_benchmark_reports(
                name,
                *strict,
                *dry_run,
                *max_regression_percent,
                *noise_threshold_percent,
                &baseline.report,
                current,
            )?;
            let regressions = comparison.regressions;
            current.comparison = Some(comparison);
            if regressions != 0 && !dry_run {
                Ok(BenchmarkRunOutcome::Regression)
            } else {
                Ok(BenchmarkRunOutcome::Passed)
            }
        }
    }
}

pub fn compare_benchmark_reports(
    name: &str,
    strict: bool,
    dry_run: bool,
    max_regression_percent: f64,
    noise_threshold_percent: f64,
    baseline: &BenchmarkJsonReport,
    current: &BenchmarkJsonReport,
) -> Result<BenchmarkComparisonReport, String> {
    let baseline_cases = baseline
        .cases
        .iter()
        .map(|case| (case.id.as_str(), case))
        .collect::<BTreeMap<_, _>>();
    let mut cases = Vec::with_capacity(current.cases.len());
    for case in &current.cases {
        let Some(previous) = baseline_cases.get(case.id.as_str()) else {
            if strict {
                return Err(format!(
                    "benchmark `{}` is absent from strict baseline `{name}`",
                    case.id
                ));
            }
            continue;
        };
        if previous.config != case.config {
            return Err(format!(
                "benchmark `{}` uses different measurement configuration than baseline `{name}`",
                case.id
            ));
        }
        let baseline_median = previous
            .median_ns_per_op
            .filter(|value| value.is_finite() && *value > 0.0)
            .ok_or_else(|| format!("baseline benchmark `{}` has no valid median", case.id))?;
        let current_median = case
            .median_ns_per_op
            .filter(|value| value.is_finite() && *value > 0.0)
            .ok_or_else(|| format!("current benchmark `{}` has no valid median", case.id))?;
        let baseline_noise = previous.mad_ns_per_op.unwrap_or(0.0) / baseline_median * 100.0;
        let current_noise = case.mad_ns_per_op.unwrap_or(0.0) / current_median * 100.0;
        let uncertainty = (baseline_noise + current_noise).max(noise_threshold_percent);
        let effective_threshold = max_regression_percent.max(uncertainty);
        let delta = (current_median - baseline_median) / baseline_median * 100.0;
        let classification = if delta > effective_threshold {
            BenchmarkClassification::Regressed
        } else if delta < -uncertainty {
            BenchmarkClassification::Improved
        } else {
            BenchmarkClassification::Unchanged
        };
        cases.push(BenchmarkComparisonCase {
            id: case.id.clone(),
            baseline_median_ns_per_op: baseline_median,
            current_median_ns_per_op: current_median,
            delta_percent: delta,
            uncertainty_percent: uncertainty,
            effective_threshold_percent: effective_threshold,
            classification,
        });
    }
    if strict {
        for id in baseline_cases.keys() {
            if !current.cases.iter().any(|case| case.id == *id) {
                return Err(format!(
                    "strict baseline `{name}` contains benchmark `{id}` missing from the current run"
                ));
            }
        }
    }
    let regressions = cases
        .iter()
        .filter(|case| case.classification == BenchmarkClassification::Regressed)
        .count();
    Ok(BenchmarkComparisonReport {
        baseline: name.to_string(),
        status: BenchmarkComparisonStatus::Compared,
        strict,
        dry_run,
        max_regression_percent,
        noise_threshold_percent,
        cases,
        regressions,
    })
}

pub fn empty_comparison(
    name: &str,
    status: BenchmarkComparisonStatus,
    strict: bool,
    dry_run: bool,
    max_regression_percent: f64,
    noise_threshold_percent: f64,
) -> BenchmarkComparisonReport {
    BenchmarkComparisonReport {
        baseline: name.to_string(),
        status,
        strict,
        dry_run,
        max_regression_percent,
        noise_threshold_percent,
        cases: Vec::new(),
        regressions: 0,
    }
}
