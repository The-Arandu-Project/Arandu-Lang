//! Benchmark runner orchestration, worker coordination, and CPU identity discovery.

use arandu_codegen::testing::{BenchmarkConfigV1, BenchmarkEventV1, CapturedOutput};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use crate::test_runner::baseline::apply_baseline;
use crate::test_runner::ipc::{
    create_ipc_pipe_pair, drain, join_capture, read_benchmark_ipc_frame,
};
use crate::test_runner::process::kill_process_tree;
use crate::test_runner::reporters::{human, json};
use crate::test_runner::types::{BenchmarkJsonReport, BenchmarkRunOutcome, BenchmarkRunnerOptions};

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
        let result = read_benchmark_ipc_frame(&mut reader, sequence, &id_owned);
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
        json::emit_benchmark_json(&report, options.output.as_deref())?;
    } else {
        human::report_benchmarks_human(events, report.comparison.as_ref());
    }
    Ok(outcome)
}

pub fn benchmark_json_report(
    events: &[BenchmarkEventV1],
    options: &BenchmarkRunnerOptions,
) -> BenchmarkJsonReport {
    json::build_benchmark_json_report(events, options, benchmark_cpu_identity())
}

pub fn benchmark_cpu_identity() -> String {
    if let Ok(identity) = std::env::var("ARANDU_BENCH_MACHINE") {
        let identity = identity.trim();
        if !identity.is_empty() {
            return format!("explicit:{identity}");
        }
    }
    #[cfg(target_os = "linux")]
    if let Ok(cpuinfo) = std::fs::read_to_string("/proc/cpuinfo")
        && let Some(model) = cpuinfo.lines().find_map(|line| {
            let (key, value) = line.split_once(':')?;
            (key.trim() == "model name").then(|| value.trim().to_string())
        })
    {
        return model;
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
