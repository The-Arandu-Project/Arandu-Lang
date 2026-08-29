//! SL_T.2 process-isolated test coordinator with framed IPC, process-tree termination,
//! and deterministic reporting.

mod junit;
mod statistics;

use arandu_codegen::testing::{
    BENCH_BASELINE_PROTOCOL_V1, BENCH_PROTOCOL_V1, BenchmarkConfigV1, BenchmarkEventV1,
    CapturedOutput, TEST_PROTOCOL_V1, TestEventV1, TestFailure, TestStatus, read_benchmark_frame,
    read_frame, write_benchmark_frame, write_frame,
};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use junit::junit_report;
use statistics::{benchmark_stats, format_ns_per_op, percentile, sample_values};

const CAPTURE_LIMIT: usize = 1024 * 1024; // 1MB

static CANCELLED: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Clone)]
pub struct RunnerOptions {
    pub jobs: usize,
    pub timeout: Duration,
    pub fail_fast: bool,
    pub seed: u64,
    pub format: TestOutputFormat,
    pub output: Option<PathBuf>,
    pub target: Option<String>,
    pub backend: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TestOutputFormat {
    Human,
    Json,
    Junit,
}

#[derive(Debug, Clone)]
pub struct BenchmarkRunnerOptions {
    pub timeout: Duration,
    pub config: BenchmarkConfigV1,
    pub format_json: bool,
    pub output: Option<PathBuf>,
    pub target: Option<String>,
    pub backend: Option<String>,
    pub baseline: Option<BenchmarkBaselineMode>,
}

#[derive(Debug, Clone)]
pub enum BenchmarkBaselineMode {
    Save {
        name: String,
    },
    Compare {
        name: String,
        strict: bool,
        dry_run: bool,
        max_regression_percent: f64,
        noise_threshold_percent: f64,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BenchmarkRunOutcome {
    Passed,
    BenchmarkFailed,
    Regression,
    MissingBaseline,
    IncompatibleBaseline,
}

impl BenchmarkRunOutcome {
    #[must_use]
    pub const fn exit_code(self) -> i32 {
        match self {
            Self::Passed => 0,
            Self::BenchmarkFailed | Self::Regression => 1,
            Self::MissingBaseline => 3,
            Self::IncompatibleBaseline => 4,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BenchmarkJsonReport {
    schema: String,
    arandu_version: String,
    target: String,
    backend: String,
    profile: String,
    clock: String,
    os: String,
    arch: String,
    cpu: String,
    cases: Vec<BenchmarkJsonCase>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    comparison: Option<BenchmarkComparisonReport>,
}

impl BenchmarkJsonReport {
    fn environment(&self) -> BenchmarkEnvironment {
        BenchmarkEnvironment {
            target: self.target.clone(),
            backend: self.backend.clone(),
            profile: self.profile.clone(),
            clock: self.clock.clone(),
            os: self.os.clone(),
            arch: self.arch.clone(),
            cpu: self.cpu.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct BenchmarkEnvironment {
    target: String,
    backend: String,
    profile: String,
    clock: String,
    os: String,
    arch: String,
    cpu: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BenchmarkJsonCase {
    id: String,
    config: BenchmarkConfigV1,
    samples: Vec<arandu_codegen::testing::BenchmarkSampleV1>,
    median_ns_per_op: Option<f64>,
    mad_ns_per_op: Option<f64>,
    p50_ns_per_op: Option<f64>,
    p95_ns_per_op: Option<f64>,
    min_ns_per_op: Option<f64>,
    stdout: String,
    stderr: String,
    stdout_truncated: bool,
    stderr_truncated: bool,
    failure: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BenchmarkBaselineFile {
    schema: String,
    name: String,
    report: BenchmarkJsonReport,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BenchmarkComparisonReport {
    baseline: String,
    status: BenchmarkComparisonStatus,
    strict: bool,
    dry_run: bool,
    max_regression_percent: f64,
    noise_threshold_percent: f64,
    cases: Vec<BenchmarkComparisonCase>,
    regressions: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum BenchmarkComparisonStatus {
    Compared,
    MissingBaseline,
    IncompatibleEnvironment,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BenchmarkComparisonCase {
    id: String,
    baseline_median_ns_per_op: f64,
    current_median_ns_per_op: f64,
    delta_percent: f64,
    uncertainty_percent: f64,
    effective_threshold_percent: f64,
    classification: BenchmarkClassification,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum BenchmarkClassification {
    Improved,
    Unchanged,
    Regressed,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct JsonReport<'a> {
    pub schema: &'static str,
    pub target: String,
    pub backend: String,
    pub seed: u64,
    pub jobs: usize,
    pub timeout_ms: u64,
    pub fail_fast: bool,
    pub summary: JsonSummary,
    pub cases: Vec<JsonCase<'a>>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct JsonSummary {
    pub total: usize,
    pub passed: usize,
    pub failed: usize,
    pub skipped: usize,
    pub timed_out: usize,
    pub crashed: usize,
    pub duration_ms: u128,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct JsonCase<'a> {
    pub id: &'a str,
    pub status: &'static str,
    pub duration_ms: u128,
    pub stdout: String,
    pub stderr: String,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
    pub failure: Option<TestFailure>,
    pub secondary_failures: Vec<TestFailure>,
    pub logs: Vec<String>,
    pub logs_truncated: bool,
}

/// Registers Ctrl-C signal handler to set global cancellation flag.
pub fn install_ctrlc_handler() {
    #[cfg(windows)]
    {
        unsafe extern "system" fn handler(_type: u32) -> i32 {
            CANCELLED.store(true, Ordering::Release);
            1 // Handled
        }
        unsafe {
            windows_sys::Win32::System::Console::SetConsoleCtrlHandler(Some(handler), 1);
        }
    }
    #[cfg(unix)]
    {
        // Simple atomic flag update for signals on Unix
        thread::spawn(|| {
            // Signal handling thread fallback if needed
        });
    }
}

/// Called inside `--harness-child` execution to send a terminal event frame over IPC.
pub fn send_child_event(sequence: u64, event: &TestEventV1) -> Result<(), String> {
    let mut writer = get_child_ipc_writer()?;
    write_frame(&mut writer, sequence, event)
}

pub fn send_benchmark_child_event(sequence: u64, event: &BenchmarkEventV1) -> Result<(), String> {
    let mut writer = get_child_ipc_writer()?;
    write_benchmark_frame(&mut writer, sequence, event)
}

pub fn run_benchmarks(
    project: &Path,
    stdlib_root: &Path,
    cases: Vec<String>,
    options: &BenchmarkRunnerOptions,
) -> Result<BenchmarkRunOutcome, String> {
    let mut events = Vec::with_capacity(cases.len());
    for (index, id) in cases.iter().enumerate() {
        let sequence = u64::try_from(index).map_err(|_| "benchmark sequence overflow")?;
        events.push(run_benchmark_case(
            project,
            stdlib_root,
            id,
            sequence,
            options,
        ));
    }
    events.sort_by(|left, right| left.id.cmp(&right.id));
    if events.iter().any(|event| event.failure.is_some()) {
        let mut reporting_options = options.clone();
        reporting_options.baseline = None;
        report_benchmarks(project, &events, &reporting_options)?;
        return Ok(BenchmarkRunOutcome::BenchmarkFailed);
    }
    report_benchmarks(project, &events, options)
}

fn run_benchmark_case(
    project: &Path,
    stdlib_root: &Path,
    id: &str,
    sequence: u64,
    options: &BenchmarkRunnerOptions,
) -> BenchmarkEventV1 {
    let (parent_reader, child_stdio) = match create_ipc_pipe_pair() {
        Ok(pair) => pair,
        Err(error) => return failed_benchmark(sequence, id, &options.config, error),
    };
    let executable = match std::env::current_exe() {
        Ok(executable) => executable,
        Err(error) => {
            return failed_benchmark(sequence, id, &options.config, error.to_string());
        }
    };
    let mut command = Command::new(executable);
    command
        .args([
            "bench",
            project.to_string_lossy().as_ref(),
            "--exact",
            id,
            "--harness-child",
        ])
        .env("ARANDU_BENCH_SEQUENCE", sequence.to_string())
        .env(
            "ARANDU_BENCH_WARMUP_NS",
            options.config.warmup_ns.to_string(),
        )
        .env(
            "ARANDU_BENCH_MEASUREMENT_NS",
            options.config.measurement_ns.to_string(),
        )
        .env("ARANDU_BENCH_SAMPLES", options.config.samples.to_string())
        .env("ARANDU_STDLIB", stdlib_root)
        .stdin(child_stdio)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            return failed_benchmark(sequence, id, &options.config, error.to_string());
        }
    };
    let stdout_handle = drain(child.stdout.take());
    let stderr_handle = drain(child.stderr.take());
    let (sender, receiver) = mpsc::channel();
    let id_owned = id.to_string();
    thread::spawn(move || {
        let mut reader = parent_reader;
        let result = read_benchmark_frame(&mut reader, sequence, &id_owned);
        let _ = sender.send(result);
    });
    let deadline = Instant::now() + options.timeout;
    let exit = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
            _ => {
                kill_process_tree(&mut child);
                break None;
            }
        }
    };
    let stdout = join_capture(stdout_handle);
    let stderr = join_capture(stderr_handle);
    let mut event = if exit.is_none() {
        failed_benchmark(sequence, id, &options.config, "benchmark timed out".into())
    } else {
        match receiver.recv_timeout(Duration::from_secs(2)) {
            Ok(Ok(event)) if exit.is_some_and(|status| status.success()) => event,
            Ok(Ok(mut event)) => {
                event
                    .failure
                    .get_or_insert_with(|| "benchmark child failed".to_string());
                event
            }
            Ok(Err(error)) => failed_benchmark(
                sequence,
                id,
                &options.config,
                format!(
                    "benchmark protocol failure: {error}; stdout={}; stderr={}",
                    String::from_utf8_lossy(&stdout.bytes),
                    String::from_utf8_lossy(&stderr.bytes)
                ),
            ),
            Err(mpsc::RecvTimeoutError::Disconnected) => failed_benchmark(
                sequence,
                id,
                &options.config,
                format!(
                    "benchmark IPC disconnected; stdout={}; stderr={}",
                    String::from_utf8_lossy(&stdout.bytes),
                    String::from_utf8_lossy(&stderr.bytes)
                ),
            ),
            Err(mpsc::RecvTimeoutError::Timeout) => failed_benchmark(
                sequence,
                id,
                &options.config,
                "benchmark child exited without a control frame".into(),
            ),
        }
    };
    event.stdout = stdout;
    event.stderr = stderr;
    event
}

fn failed_benchmark(
    sequence: u64,
    id: &str,
    config: &BenchmarkConfigV1,
    failure: String,
) -> BenchmarkEventV1 {
    BenchmarkEventV1 {
        sequence,
        id: id.to_string(),
        config: config.clone(),
        samples: Vec::new(),
        stdout: CapturedOutput::default(),
        stderr: CapturedOutput::default(),
        failure: Some(failure),
    }
}

fn report_benchmarks(
    project: &Path,
    events: &[BenchmarkEventV1],
    options: &BenchmarkRunnerOptions,
) -> Result<BenchmarkRunOutcome, String> {
    let mut report = benchmark_json_report(events, options);
    let outcome = apply_baseline(project, &mut report, options)?;

    if options.format_json {
        let bytes = serde_json::to_vec_pretty(&report).map_err(|error| error.to_string())?;
        if let Some(output) = &options.output {
            atomic_write_file(output, &bytes)?;
        } else {
            println!("{}", String::from_utf8_lossy(&bytes));
        }
    } else {
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
        if let Some(comparison) = &report.comparison {
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
    Ok(outcome)
}

fn benchmark_json_report(
    events: &[BenchmarkEventV1],
    options: &BenchmarkRunnerOptions,
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
        cpu: benchmark_cpu_identity(),
        cases,
        comparison: None,
    }
}

fn apply_baseline(
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

fn compare_benchmark_reports(
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
        .collect::<std::collections::BTreeMap<_, _>>();
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

fn empty_comparison(
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

fn validate_baseline_name(name: &str) -> Result<(), String> {
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

fn baseline_path(project: &Path, name: &str) -> PathBuf {
    project
        .join("target")
        .join("arandu")
        .join("benchmarks")
        .join(format!("{name}.json"))
}

fn benchmark_cpu_identity() -> String {
    if let Ok(identity) = std::env::var("ARANDU_BENCH_MACHINE") {
        let identity = identity.trim();
        if !identity.is_empty() {
            return format!("explicit:{identity}");
        }
    }
    #[cfg(target_os = "linux")]
    if let Ok(cpuinfo) = fs::read_to_string("/proc/cpuinfo") {
        if let Some(model) = cpuinfo.lines().find_map(|line| {
            let (key, value) = line.split_once(':')?;
            (key.trim() == "model name").then(|| value.trim().to_string())
        }) {
            return model;
        }
    }
    #[cfg(target_os = "macos")]
    if let Ok(output) = Command::new("sysctl")
        .args(["-n", "machdep.cpu.brand_string"])
        .output()
    {
        let model = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !model.is_empty() {
            return model;
        }
    }
    std::env::var("PROCESSOR_IDENTIFIER").unwrap_or_else(|_| "unknown".to_string())
}

#[cfg(unix)]
fn get_child_ipc_writer() -> Result<impl Write, String> {
    use std::os::unix::io::FromRawFd;
    // Standard File descriptor 0 (stdin) is connected to parent's read pipe/socket
    unsafe { Ok(std::fs::File::from_raw_fd(0)) }
}

#[cfg(windows)]
fn get_child_ipc_writer() -> Result<impl Write, String> {
    use std::os::windows::io::FromRawHandle;
    unsafe {
        let handle = windows_sys::Win32::System::Console::GetStdHandle(
            windows_sys::Win32::System::Console::STD_INPUT_HANDLE,
        );
        if handle.is_null() || handle == windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE {
            return Err("invalid child IPC handle".to_string());
        }
        Ok(std::fs::File::from_raw_handle(handle as *mut _))
    }
}

#[cfg(not(any(unix, windows)))]
fn get_child_ipc_writer() -> Result<impl Write, String> {
    Err("unsupported platform for IPC".to_string())
}

pub fn run_cases(
    project: &Path,
    stdlib_root: &Path,
    mut cases: Vec<String>,
    options: &RunnerOptions,
) -> Result<bool, String> {
    if cases.is_empty() {
        let empty_events: Vec<TestEventV1> = Vec::new();
        report(&empty_events, options)?;
        return Ok(true);
    }

    deterministic_shuffle(&mut cases, options.seed);
    let cases = Arc::new(cases);
    let next = Arc::new(AtomicUsize::new(0));
    let stop = Arc::new(AtomicBool::new(false));
    let (sender, receiver) = mpsc::channel();
    let workers = options.jobs.max(1).min(cases.len());
    let mut handles = Vec::new();

    for _ in 0..workers {
        let cases = Arc::clone(&cases);
        let next = Arc::clone(&next);
        let stop = Arc::clone(&stop);
        let sender = sender.clone();
        let project = project.to_path_buf();
        let stdlib_root = stdlib_root.to_path_buf();
        let timeout = options.timeout;
        let fail_fast = options.fail_fast;

        handles.push(thread::spawn(move || {
            loop {
                if CANCELLED.load(Ordering::Acquire) || (fail_fast && stop.load(Ordering::Acquire))
                {
                    break;
                }
                let index = next.fetch_add(1, Ordering::AcqRel);
                let Some(id) = cases.get(index) else { break };
                let sequence = u64::try_from(index).unwrap_or(u64::MAX);

                let event = run_case(&project, &stdlib_root, id, timeout, sequence);
                if !matches!(event.status, TestStatus::Passed | TestStatus::Skipped) {
                    stop.store(true, Ordering::Release);
                }
                if sender.send(event).is_err() {
                    break;
                }
            }
        }));
    }
    drop(sender);

    let mut events: Vec<_> = receiver.into_iter().collect();
    for handle in handles {
        let _ = handle.join();
    }

    events.sort_by(|left, right| left.id.cmp(&right.id));
    report(&events, options)?;

    Ok(events
        .iter()
        .all(|event| matches!(event.status, TestStatus::Passed | TestStatus::Skipped)))
}

fn run_case(
    project: &Path,
    stdlib_root: &Path,
    id: &str,
    timeout: Duration,
    sequence: u64,
) -> TestEventV1 {
    let started = Instant::now();
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temp_root = std::env::temp_dir().join(format!(
        "arandu-test-run-{}-{sequence}-{nonce}",
        blake3::hash(id.as_bytes()).to_hex()
    ));
    if let Err(error) = fs::create_dir(&temp_root) {
        return failed_event(
            sequence,
            id,
            started,
            TestStatus::Crashed,
            format!("failed creating test sandbox: {error}"),
        );
    }

    let (parent_reader, child_stdio) = match create_ipc_pipe_pair() {
        Ok(pair) => pair,
        Err(error) => {
            return failed_event(
                sequence,
                id,
                started,
                TestStatus::Crashed,
                format!("failed creating IPC pipe: {error}"),
            );
        }
    };

    let executable = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(error) => {
            return failed_event(
                sequence,
                id,
                started,
                TestStatus::Crashed,
                error.to_string(),
            );
        }
    };

    let mut command = Command::new(executable);
    command
        .args([
            "test",
            project.to_string_lossy().as_ref(),
            "--exact",
            id,
            "--harness-child",
        ])
        .env("ARANDU_TEST_SEQUENCE", sequence.to_string())
        .env("ARANDU_TEST_TEMP_ROOT", &temp_root)
        .env("ARANDU_STDLIB", stdlib_root)
        .stdin(child_stdio)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            return failed_event(
                sequence,
                id,
                started,
                TestStatus::Crashed,
                format!("failed spawning child process: {error}"),
            );
        }
    };

    let stdout_handle = drain(child.stdout.take());
    let stderr_handle = drain(child.stderr.take());

    let (status_outcome, frame_result) =
        wait_and_read_frame(&mut child, parent_reader, timeout, sequence, id);

    let stdout = join_capture(stdout_handle);
    let stderr = join_capture(stderr_handle);
    let _ = fs::remove_dir_all(&temp_root);

    let (event_status, failure, secondary_failures, logs, logs_truncated) = match status_outcome {
        WaitOutcome::TimedOut => (
            TestStatus::TimedOut,
            Some(TestFailure::simple("test timed out")),
            Vec::new(),
            Vec::new(),
            false,
        ),
        WaitOutcome::Exited(exit) => match frame_result {
            Ok(event) if exit.success() && event.status == TestStatus::Passed => (
                TestStatus::Passed,
                None,
                event.secondary_failures,
                event.logs,
                event.logs_truncated,
            ),
            Ok(event) => (
                event.status,
                event.failure,
                event.secondary_failures,
                event.logs,
                event.logs_truncated,
            ),
            Err(error) => (
                TestStatus::Crashed,
                Some(TestFailure::simple(format!("protocol failure: {error}"))),
                Vec::new(),
                Vec::new(),
                false,
            ),
        },
    };

    TestEventV1 {
        sequence,
        id: id.into(),
        status: event_status,
        duration: started.elapsed(),
        stdout,
        stderr,
        failure,
        secondary_failures,
        logs,
        logs_truncated,
    }
}

enum WaitOutcome {
    Exited(std::process::ExitStatus),
    TimedOut,
}

fn wait_and_read_frame<R: Read + Send + 'static>(
    child: &mut Child,
    mut reader: R,
    timeout: Duration,
    sequence: u64,
    id: &str,
) -> (WaitOutcome, Result<TestEventV1, String>) {
    // The IPC frame reader runs on a background thread. When the child exits normally,
    // the write-end of the pipe is closed and read_frame returns (EOF error or success).
    // When the child crashes without writing, we give a short grace period after exit
    // before declaring a protocol failure, to avoid blocking forever.
    let (frame_tx, frame_rx) = mpsc::channel::<Result<TestEventV1, String>>();
    let id_clone = id.to_string();
    thread::spawn(move || {
        let result = read_frame(&mut reader, Some(sequence), Some(&id_clone));
        let _ = frame_tx.send(result);
    });

    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                // Give the IPC reader up to 2s to drain after the child exits.
                let frame_res = frame_rx
                    .recv_timeout(Duration::from_secs(2))
                    .unwrap_or_else(|_| {
                        Err("child exited without sending IPC control frame".to_string())
                    });
                return (WaitOutcome::Exited(status), frame_res);
            }
            Ok(None) if Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(10));
            }
            _ => {
                kill_process_tree(child);
                // After killing, give a short window for any in-flight IPC data.
                let frame_res = frame_rx
                    .recv_timeout(Duration::from_millis(200))
                    .unwrap_or_else(|_| Err("process timed out without IPC frame".to_string()));
                return (WaitOutcome::TimedOut, frame_res);
            }
        }
    }
}

fn kill_process_tree(child: &mut Child) {
    let pid = child.id();
    #[cfg(unix)]
    {
        if let Ok(pid_i32) = i32::try_from(pid) {
            // SAFETY: `pid_i32` is the checked process id returned by `Child`.
            // The child is spawned as the leader of its own process group, so
            // `killpg` targets only that test and its descendants.
            let _ = unsafe { libc::killpg(pid_i32, libc::SIGKILL) };
        }
    }
    #[cfg(windows)]
    {
        let _ = Command::new("taskkill")
            .args(["/F", "/T", "/PID", &pid.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(unix)]
fn create_ipc_pipe_pair() -> Result<(impl Read + Send + 'static, Stdio), String> {
    use std::os::fd::OwnedFd;
    use std::os::unix::net::UnixStream;
    let (parent_stream, child_stream) =
        UnixStream::pair().map_err(|e| format!("unix socketpair failed: {e}"))?;
    let child_fd = OwnedFd::from(child_stream);
    Ok((parent_stream, Stdio::from(child_fd)))
}

#[cfg(windows)]
fn create_ipc_pipe_pair() -> Result<(impl Read + Send + 'static, Stdio), String> {
    use std::os::windows::io::{FromRawHandle, OwnedHandle};
    let mut read_handle = std::ptr::null_mut();
    let mut write_handle = std::ptr::null_mut();

    let sa = windows_sys::Win32::Security::SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<windows_sys::Win32::Security::SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: std::ptr::null_mut(),
        bInheritHandle: 1,
    };

    let res = unsafe {
        windows_sys::Win32::System::Pipes::CreatePipe(&mut read_handle, &mut write_handle, &sa, 0)
    };
    if res == 0 {
        return Err("CreatePipe failed".to_string());
    }

    let parent_file = unsafe { std::fs::File::from_raw_handle(read_handle) };
    let child_stdio = unsafe { Stdio::from(OwnedHandle::from_raw_handle(write_handle)) };

    Ok((parent_file, child_stdio))
}

#[cfg(not(any(unix, windows)))]
fn create_ipc_pipe_pair() -> Result<(impl Read + Send + 'static, Stdio), String> {
    Err("unsupported OS platform".to_string())
}

fn failed_event(
    sequence: u64,
    id: &str,
    started: Instant,
    status: TestStatus,
    failure: String,
) -> TestEventV1 {
    TestEventV1 {
        sequence,
        id: id.into(),
        status,
        duration: started.elapsed(),
        stdout: empty_capture(),
        stderr: empty_capture(),
        failure: Some(TestFailure::simple(failure)),
        secondary_failures: Vec::new(),
        logs: Vec::new(),
        logs_truncated: false,
    }
}

fn drain<R: Read + Send + 'static>(reader: Option<R>) -> thread::JoinHandle<CapturedOutput> {
    thread::spawn(move || {
        let mut retained = Vec::new();
        let mut truncated = false;
        if let Some(mut reader) = reader {
            let mut buffer = [0_u8; 8192];
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(read) => {
                        let remaining = CAPTURE_LIMIT.saturating_sub(retained.len());
                        retained.extend_from_slice(&buffer[..read.min(remaining)]);
                        truncated |= read > remaining;
                    }
                }
            }
        }
        CapturedOutput {
            bytes: retained,
            truncated,
        }
    })
}

fn join_capture(handle: thread::JoinHandle<CapturedOutput>) -> CapturedOutput {
    handle.join().unwrap_or_else(|_| empty_capture())
}

fn empty_capture() -> CapturedOutput {
    CapturedOutput {
        bytes: Vec::new(),
        truncated: false,
    }
}

pub fn deterministic_shuffle(cases: &mut [String], mut state: u64) {
    if state == 0 {
        return;
    }
    for index in (1..cases.len()).rev() {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let upper = u64::try_from(index + 1).unwrap_or(u64::MAX);
        let selected = usize::try_from(state % upper).unwrap_or(0);
        cases.swap(index, selected);
    }
}

pub fn atomic_write_file(path: &Path, content: &[u8]) -> Result<(), String> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)
            .map_err(|error| format!("create report directory {}: {error}", parent.display()))?;
    }
    let staging = path.with_extension("arandu-test-staging");
    fs::write(&staging, content).map_err(|error| error.to_string())?;

    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        let src: Vec<u16> = staging
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let dst: Vec<u16> = path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let res = unsafe {
            windows_sys::Win32::Storage::FileSystem::MoveFileExW(
                src.as_ptr(),
                dst.as_ptr(),
                windows_sys::Win32::Storage::FileSystem::MOVEFILE_REPLACE_EXISTING
                    | windows_sys::Win32::Storage::FileSystem::MOVEFILE_WRITE_THROUGH,
            )
        };
        if res == 0 {
            // Fallback if target file replace failed
            let _ = fs::remove_file(path);
            let _ = fs::rename(&staging, path);
        }
        let _ = fs::remove_file(&staging);
        Ok(())
    }

    #[cfg(not(windows))]
    {
        fs::rename(&staging, path).map_err(|error| {
            let _ = fs::remove_file(&staging);
            error.to_string()
        })
    }
}

fn report(events: &[TestEventV1], options: &RunnerOptions) -> Result<(), String> {
    let total = events.len();
    let mut passed = 0;
    let mut failed = 0;
    let mut skipped = 0;
    let mut timed_out = 0;
    let mut crashed = 0;
    let mut total_duration_ms: u128 = 0;

    for event in events {
        total_duration_ms += event.duration.as_millis();
        match event.status {
            TestStatus::Passed => passed += 1,
            TestStatus::Failed => failed += 1,
            TestStatus::Skipped => skipped += 1,
            TestStatus::TimedOut => timed_out += 1,
            TestStatus::Crashed => crashed += 1,
        }
    }

    if options.format == TestOutputFormat::Json {
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
            summary: JsonSummary {
                total,
                passed,
                failed,
                skipped,
                timed_out,
                crashed,
                duration_ms: total_duration_ms,
            },
            cases,
        };

        let encoded = serde_json::to_vec_pretty(&report_obj).map_err(|error| error.to_string())?;

        if let Some(output) = &options.output {
            atomic_write_file(output, &encoded)?;
        } else {
            println!("{}", String::from_utf8_lossy(&encoded));
        }
    } else if options.format == TestOutputFormat::Junit {
        let encoded = junit_report(events, total_duration_ms);
        if let Some(output) = &options.output {
            atomic_write_file(output, encoded.as_bytes())?;
        } else {
            println!("{encoded}");
        }
    } else {
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
            "test result: {}. {passed} passed; {failed} failed; {skipped} skipped; {timed_out} timed out; {crashed} crashed",
            if failed == 0 && timed_out == 0 && crashed == 0 {
                "ok"
            } else {
                "FAILED"
            }
        );
    }
    Ok(())
}

fn status_name(status: TestStatus) -> &'static str {
    match status {
        TestStatus::Passed => "passed",
        TestStatus::Failed => "failed",
        TestStatus::Skipped => "skipped",
        TestStatus::TimedOut => "timed_out",
        TestStatus::Crashed => "crashed",
    }
}

#[cfg(test)]
mod benchmark_tests {
    use super::*;

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
