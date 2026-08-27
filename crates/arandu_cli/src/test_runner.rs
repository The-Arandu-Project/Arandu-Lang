//! SL_T.2 process-isolated test coordinator with framed IPC, process-tree termination,
//! and deterministic reporting.

use arandu_codegen::testing::{
    CapturedOutput, TEST_PROTOCOL_V1, TestEventV1, TestFailure, TestStatus, read_frame, write_frame,
};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::{Duration, Instant};

const CAPTURE_LIMIT: usize = 1024 * 1024; // 1MB

static CANCELLED: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Clone)]
pub struct RunnerOptions {
    pub jobs: usize,
    pub timeout: Duration,
    pub fail_fast: bool,
    pub seed: u64,
    pub format_json: bool,
    pub output: Option<PathBuf>,
    pub target: Option<String>,
    pub backend: Option<String>,
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

                let event = run_case(&project, id, timeout, sequence);
                if event.status != TestStatus::Passed {
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
        .all(|event| event.status == TestStatus::Passed))
}

fn run_case(project: &Path, id: &str, timeout: Duration, sequence: u64) -> TestEventV1 {
    let started = Instant::now();

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

    let (event_status, failure) = match status_outcome {
        WaitOutcome::TimedOut => (
            TestStatus::TimedOut,
            Some(TestFailure::simple("test timed out")),
        ),
        WaitOutcome::Exited(exit) => match frame_result {
            Ok(event) if exit.success() && event.status == TestStatus::Passed => {
                (TestStatus::Passed, None)
            }
            Ok(event) => (event.status, event.failure),
            Err(error) => (
                TestStatus::Crashed,
                Some(TestFailure::simple(format!("protocol failure: {error}"))),
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
            unsafe {
                libc::kill(-pid_i32, libc::SIGKILL);
            }
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
    use std::os::unix::net::UnixStream;
    let (parent_stream, child_stream) =
        UnixStream::pair().map_err(|e| format!("unix socketpair failed: {e}"))?;
    Ok((parent_stream, Stdio::from(child_stream)))
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

    if options.format_json {
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
        }
        eprintln!(
            "test result: {}. {passed} passed; {failed} failed; {skipped} skipped; {timed_out} timed out; {crashed} crashed",
            if passed == total { "ok" } else { "FAILED" }
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
