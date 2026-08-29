//! IPC framing, pipe creation, and child event transmission.

use arandu_codegen::testing::{
    BenchmarkEventV1, CapturedOutput, TestEventV1, read_benchmark_frame, read_frame,
    write_benchmark_frame, write_frame,
};
use std::io::{Read, Write};
use std::process::Stdio;
use std::thread;

pub const CAPTURE_LIMIT: usize = 1024 * 1024; // 1MB

/// Called inside `--harness-child` execution to send a terminal event frame over IPC.
pub fn send_child_event(sequence: u64, event: &TestEventV1) -> Result<(), String> {
    let mut writer = get_child_ipc_writer()?;
    write_frame(&mut writer, sequence, event)
}

pub fn send_benchmark_child_event(sequence: u64, event: &BenchmarkEventV1) -> Result<(), String> {
    let mut writer = get_child_ipc_writer()?;
    write_benchmark_frame(&mut writer, sequence, event)
}

pub(crate) fn read_ipc_frame<R: Read>(
    reader: &mut R,
    sequence: u64,
    id: &str,
) -> Result<TestEventV1, String> {
    read_frame(reader, Some(sequence), Some(id))
}

pub(crate) fn read_benchmark_ipc_frame<R: Read>(
    reader: &mut R,
    sequence: u64,
    id: &str,
) -> Result<BenchmarkEventV1, String> {
    read_benchmark_frame(reader, sequence, id)
}

#[cfg(unix)]
pub(crate) fn get_child_ipc_writer() -> Result<impl Write, String> {
    use std::os::unix::io::FromRawFd;
    // Standard File descriptor 0 (stdin) is connected to parent's read pipe/socket
    unsafe { Ok(std::fs::File::from_raw_fd(0)) }
}

#[cfg(windows)]
pub(crate) fn get_child_ipc_writer() -> Result<impl Write, String> {
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
pub(crate) fn get_child_ipc_writer() -> Result<impl Write, String> {
    Err("unsupported platform for IPC".to_string())
}

#[cfg(unix)]
pub(crate) fn create_ipc_pipe_pair() -> Result<(impl Read + Send + 'static, Stdio), String> {
    use std::os::fd::OwnedFd;
    use std::os::unix::net::UnixStream;
    let (parent_stream, child_stream) =
        UnixStream::pair().map_err(|e| format!("unix socketpair failed: {e}"))?;
    let child_fd = OwnedFd::from(child_stream);
    Ok((parent_stream, Stdio::from(child_fd)))
}

#[cfg(windows)]
pub(crate) fn create_ipc_pipe_pair() -> Result<(impl Read + Send + 'static, Stdio), String> {
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
pub(crate) fn create_ipc_pipe_pair() -> Result<(impl Read + Send + 'static, Stdio), String> {
    Err("unsupported OS platform".to_string())
}

pub(crate) fn drain<R: Read + Send + 'static>(
    reader: Option<R>,
) -> thread::JoinHandle<CapturedOutput> {
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

pub(crate) fn join_capture(handle: thread::JoinHandle<CapturedOutput>) -> CapturedOutput {
    handle.join().unwrap_or_else(|_| empty_capture())
}

pub(crate) fn empty_capture() -> CapturedOutput {
    CapturedOutput {
        bytes: Vec::new(),
        truncated: false,
    }
}
