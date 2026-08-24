//! Thin OS hosts for Minimal 0.1 optional std surface (`std.process` / `std.time` / `std.env`).
//!
//! Design notes (aligned with Go `os` / Rust `std::{process,env,time}`):
//! - `exit` terminates the **process** (like `os.Exit` / `process::exit`), not just `main`.
//! - `monotonic_ns` is a steady clock (not wall time) — comparable to `Instant` / `CLOCK_MONOTONIC`.
//! - `env` is **query-only** in Minimal: no `setenv` (avoids global-process races).
//! - Strings use the JIT fat ABI (`*const u8` + `i64` len), same as path helpers.

use std::time::{Duration, Instant};

/// Process start for monotonic offsets (lazy, process-local).
fn mono_origin() -> Instant {
    static ORIGIN: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    *ORIGIN.get_or_init(Instant::now)
}

/// Terminate the process with `code` (truncated to 8 bits on Unix by the OS).
///
/// # Safety
/// Called only from JIT-linked Arandu code; never returns.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_process_exit(code: i64) -> ! {
    // Match common shell convention: clamp to u8 range for predictability.
    let status = if code < 0 {
        255
    } else if code > 255 {
        (code & 0xff) as i32
    } else {
        code as i32
    };
    std::process::exit(status)
}

/// Nanoseconds since an arbitrary process-local epoch (monotonic, not wall clock).
///
/// # Safety
/// No pointer args; always safe to call from JIT.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_time_monotonic_ns() -> i64 {
    let elapsed: Duration = mono_origin().elapsed();
    // Saturate instead of wrapping on very long-lived processes.
    i64::try_from(elapsed.as_nanos()).unwrap_or(i64::MAX)
}

/// Number of process arguments including `argv[0]` (like Go `len(os.Args)` / Rust `env::args().len()`).
///
/// # Safety
/// No pointer args.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_env_args_len() -> i64 {
    std::env::args_os().count() as i64
}

/// `1` if the environment variable `name` is set (even to empty), else `0`.
///
/// Mirrors Rust `env::var_os(name).is_some()` and Go `LookupEnv` presence — not “non-empty”.
///
/// # Safety
/// `ptr`/`len` must form a valid UTF-8 fat string from the Arandu JIT (or null/empty → 0).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_env_var_is_set(ptr: *const u8, len: i64) -> i64 {
    if len <= 0 || ptr.is_null() {
        return 0;
    }
    let bytes = unsafe { std::slice::from_raw_parts(ptr, len as usize) };
    let Ok(name) = std::str::from_utf8(bytes) else {
        return 0;
    };
    if name.is_empty() || name.contains('\0') || name.contains('=') {
        // Invalid env names — treat as unset (never panic from host).
        return 0;
    }
    i64::from(std::env::var_os(name).is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn monotonic_non_decreasing() {
        unsafe {
            let a = ar_time_monotonic_ns();
            let b = ar_time_monotonic_ns();
            assert!(b >= a, "monotonic_ns went backwards: {a} -> {b}");
            assert!(a >= 0);
        }
    }

    #[test]
    fn args_len_at_least_one() {
        unsafe {
            assert!(ar_env_args_len() >= 1);
        }
    }

    #[test]
    fn var_is_set_path_and_missing() {
        unsafe {
            // PATH is present on essentially all Unix/macOS/Windows CI images.
            let path = b"PATH";
            assert_eq!(ar_env_var_is_set(path.as_ptr(), path.len() as i64), 1);

            let missing = b"ARANDU_P1_ENV_MISSING_XYZ_9f3a";
            assert_eq!(ar_env_var_is_set(missing.as_ptr(), missing.len() as i64), 0);

            // Empty / invalid names never set.
            assert_eq!(ar_env_var_is_set(b"".as_ptr(), 0), 0);
            assert_eq!(ar_env_var_is_set(b"A=B".as_ptr(), 3), 0);
        }
    }
}
