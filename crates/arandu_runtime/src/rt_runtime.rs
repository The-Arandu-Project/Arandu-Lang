//! SL_R.0 — cooperative multi-task host for debug JIT (i64 payload MVP).
//!
//! Complements [`crate::poll_runtime`] (single-coroutine poll/block_on).
//!
//! ## Model
//! - Explicit handles, no global language-level executor in user code beyond
//!   these host symbols (stdlib wraps them as `SyncExecutor`).
//! - `spawn` parks a coroutine state blob; `join` drives it with
//!   [`crate::poll_runtime::ar_co_block_on_i64`].
//! - Cooperative only: Pending spins (no OS reactor yet — SL_R.2).

use crate::poll_runtime::{ar_co_block_on_i64, ar_co_free};
use std::sync::Mutex;

struct TaskSlot {
    state: *mut u8,
    done: bool,
    result: i64,
}

// Safety: JIT is single-threaded today; Mutex for future multi-thread SyncExecutor.
unsafe impl Send for TaskSlot {}

static TASKS: Mutex<Vec<Option<TaskSlot>>> = Mutex::new(Vec::new());

/// Spawn a coroutine state onto the SyncExecutor queue. Returns handle (>= 0).
///
/// # Safety
/// `state` must be a valid coroutine blob (same as poll_runtime).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_rt_spawn_i64(state: *mut u8) -> i64 {
    if state.is_null() {
        std::process::abort();
    }
    let mut guard = TASKS.lock().unwrap_or_else(|e| e.into_inner());
    let slot = TaskSlot {
        state,
        done: false,
        result: 0,
    };
    // Reuse free slots
    if let Some(idx) = guard.iter().position(|s| s.is_none()) {
        guard[idx] = Some(slot);
        return idx as i64;
    }
    let id = guard.len();
    guard.push(Some(slot));
    id as i64
}

/// Drive task `handle` to completion; returns i64 payload. Invalid handle aborts.
///
/// # Safety
/// `handle` must come from [`ar_rt_spawn_i64`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_rt_join_i64(handle: i64) -> i64 {
    if handle < 0 {
        std::process::abort();
    }
    let idx = handle as usize;
    let state = {
        let mut guard = TASKS.lock().unwrap_or_else(|e| e.into_inner());
        let slot = guard.get_mut(idx).and_then(|s| s.as_mut());
        let Some(slot) = slot else {
            std::process::abort();
        };
        if slot.done {
            return slot.result;
        }
        slot.state
    };
    let result = unsafe { ar_co_block_on_i64(state) };
    {
        let mut guard = TASKS.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(Some(slot)) = guard.get_mut(idx) {
            slot.done = true;
            slot.result = result;
            // Free blob after join (ownership transfer to runtime).
            unsafe {
                ar_co_free(slot.state);
            }
            slot.state = std::ptr::null_mut();
        }
    }
    result
}

/// Block on a single coroutine without spawn (alias surface for std.runtime).
///
/// # Safety
/// Same as [`ar_co_block_on_i64`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_rt_block_on_i64(state: *mut u8) -> i64 {
    unsafe { ar_co_block_on_i64(state) }
}

/// Drop a finished/unneeded handle without joining (frees if not done).
///
/// # Safety
/// Handle from spawn; not usable after.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_rt_cancel_i64(handle: i64) {
    if handle < 0 {
        return;
    }
    let idx = handle as usize;
    let mut guard = TASKS.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(slot) = guard.get_mut(idx).and_then(|s| s.take()) {
        if !slot.state.is_null() {
            unsafe {
                ar_co_free(slot.state);
            }
        }
    }
}

/// Path absolute check for SL_S / Minimal path helpers.
///
/// Uses the host [`std::path::Path::is_absolute`] semantics (Unix `/…`, Windows
/// drive/UNC). Empty and invalid UTF-8 are never absolute.
///
/// # Safety
/// `ptr`/`len` fat string from Arandu JIT.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_path_is_absolute(ptr: *const u8, len: isize) -> isize {
    if len <= 0 || ptr.is_null() {
        return 0;
    }
    let s = unsafe { std::slice::from_raw_parts(ptr, len as usize) };
    let Ok(text) = std::str::from_utf8(s) else {
        return 0;
    };
    isize::from(std::path::Path::new(text).is_absolute())
}

/// Path empty check.
///
/// # Safety
/// Fat string ABI.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_path_is_empty(_ptr: *const u8, len: isize) -> isize {
    isize::from(len <= 0)
}

/// Fat `str` return for path hosts (matches LayoutEngine / Cranelift multi-value).
///
/// On System V x86_64, two pointer-width fields return in the same registers as
/// Cranelift multi-value `(ptr, len)`.
#[repr(C)]
pub struct ArFatStr {
    pub ptr: *mut u8,
    pub len: isize,
}

pub(crate) fn fat_str_from_string(s: String) -> ArFatStr {
    let len = s.len() as isize;
    // Process-lifetime leak (same policy as ToStr / string interp).
    let boxed = s.into_boxed_str();
    let ptr = Box::into_raw(boxed) as *mut u8;
    ArFatStr { ptr, len }
}

fn path_from_fat(ptr: *const u8, len: isize) -> Option<std::path::PathBuf> {
    if len < 0 || (len > 0 && ptr.is_null()) {
        return None;
    }
    if len == 0 {
        return Some(std::path::PathBuf::new());
    }
    let s = unsafe { std::slice::from_raw_parts(ptr, len as usize) };
    let text = std::str::from_utf8(s).ok()?;
    Some(std::path::PathBuf::from(text))
}

/// Join two path segments (`std::path::Path::join`).
///
/// # Safety
/// Fat string ABI for both inputs; returns malloc-style owned buffer.
unsafe fn ar_path_join_impl(
    a_ptr: *const u8,
    a_len: isize,
    b_ptr: *const u8,
    b_len: isize,
) -> ArFatStr {
    let a = path_from_fat(a_ptr, a_len).unwrap_or_default();
    let b = path_from_fat(b_ptr, b_len).unwrap_or_default();
    let joined = a.join(b);
    fat_str_from_string(joined.to_string_lossy().into_owned())
}

/// File name component (`Path::file_name`); empty string when none.
///
/// # Safety
/// Fat string ABI.
unsafe fn ar_path_file_name_impl(ptr: *const u8, len: isize) -> ArFatStr {
    let Some(path) = path_from_fat(ptr, len) else {
        return fat_str_from_string(String::new());
    };
    match path.file_name() {
        Some(name) => fat_str_from_string(name.to_string_lossy().into_owned()),
        None => fat_str_from_string(String::new()),
    }
}

fn slice_from_fat(ptr: *const u8, len: isize) -> &'static [u8] {
    if len <= 0 || ptr.is_null() {
        return b"";
    }
    unsafe { std::slice::from_raw_parts(ptr, len as usize) }
}

/// Fat-str length (bytes).
///
/// # Safety
/// Fat string ABI.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_str_len(_ptr: *const u8, len: isize) -> isize {
    len.max(0)
}

/// Concatenate two fat strings (malloc-style process-lifetime buffer).
///
/// # Safety
/// Fat string ABI for both inputs.
unsafe fn ar_str_concat_impl(
    a_ptr: *const u8,
    a_len: isize,
    b_ptr: *const u8,
    b_len: isize,
) -> ArFatStr {
    let a = slice_from_fat(a_ptr, a_len);
    let b = slice_from_fat(b_ptr, b_len);
    let mut out = Vec::with_capacity(a.len() + b.len());
    out.extend_from_slice(a);
    out.extend_from_slice(b);
    let len = out.len() as isize;
    let ptr = Box::into_raw(out.into_boxed_slice()) as *mut u8;
    ArFatStr { ptr, len }
}

/// Prefix check (byte-wise).
///
/// # Safety
/// Fat string ABI.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_str_starts_with(
    s_ptr: *const u8,
    s_len: isize,
    p_ptr: *const u8,
    p_len: isize,
) -> isize {
    let s = slice_from_fat(s_ptr, s_len);
    let p = slice_from_fat(p_ptr, p_len);
    isize::from(s.starts_with(p))
}

/// Suffix check (byte-wise).
///
/// # Safety
/// Fat string ABI.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_str_ends_with(
    s_ptr: *const u8,
    s_len: isize,
    p_ptr: *const u8,
    p_len: isize,
) -> isize {
    let s = slice_from_fat(s_ptr, s_len);
    let p = slice_from_fat(p_ptr, p_len);
    isize::from(s.ends_with(p))
}

/// Contains check (byte-wise).
///
/// # Safety
/// Fat string ABI.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_str_contains(
    s_ptr: *const u8,
    s_len: isize,
    needle_ptr: *const u8,
    needle_len: isize,
) -> isize {
    let s = slice_from_fat(s_ptr, s_len);
    let needle = slice_from_fat(needle_ptr, needle_len);
    if needle.is_empty() {
        return 1;
    }
    isize::from(s.windows(needle.len()).any(|w| w == needle))
}

/// Find index of needle (byte-wise); returns -1 if not found.
///
/// # Safety
/// Fat string ABI.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_str_find(
    s_ptr: *const u8,
    s_len: isize,
    needle_ptr: *const u8,
    needle_len: isize,
) -> isize {
    let s = slice_from_fat(s_ptr, s_len);
    let needle = slice_from_fat(needle_ptr, needle_len);
    if needle.is_empty() {
        return 0;
    }
    if let Some(pos) = s.windows(needle.len()).position(|w| w == needle) {
        pos as isize
    } else {
        -1
    }
}

/// Bytes after the last occurrence of `sep` (byte-wise). Empty sep → full `s`.
///
/// # Safety
/// Fat string ABI.
unsafe fn ar_str_split_last_impl(
    s_ptr: *const u8,
    s_len: isize,
    sep_ptr: *const u8,
    sep_len: isize,
) -> ArFatStr {
    let s = slice_from_fat(s_ptr, s_len);
    let sep = slice_from_fat(sep_ptr, sep_len);
    if sep.is_empty() {
        return fat_str_from_string(String::from_utf8_lossy(s).into_owned());
    }
    if let Some(pos) = s.windows(sep.len()).rposition(|w| w == sep) {
        let after = &s[pos + sep.len()..];
        return fat_str_from_string(String::from_utf8_lossy(after).into_owned());
    }
    fat_str_from_string(String::from_utf8_lossy(s).into_owned())
}

// Cranelift represents `str` as two return registers. Windows x64's C ABI
// returns this 16-byte struct indirectly, whereas SysV returns it in RAX/RDX.
// Keep the exported JIT boundary on SysV there; native Rust callers use the
// platform ABI only through the private implementations above.
/// Join two valid fat-string paths.
///
/// # Safety
/// Both pointer/length pairs must satisfy the fat-string ABI.
#[cfg(not(windows))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_path_join(
    a_ptr: *const u8,
    a_len: isize,
    b_ptr: *const u8,
    b_len: isize,
) -> ArFatStr {
    unsafe { ar_path_join_impl(a_ptr, a_len, b_ptr, b_len) }
}

#[cfg(windows)]
/// Join two valid fat-string paths using the System V ABI expected by JIT code.
///
/// # Safety
/// Both pointer/length pairs must satisfy the fat-string ABI.
#[unsafe(no_mangle)]
pub unsafe extern "sysv64" fn ar_path_join(
    a_ptr: *const u8,
    a_len: isize,
    b_ptr: *const u8,
    b_len: isize,
) -> ArFatStr {
    unsafe { ar_path_join_impl(a_ptr, a_len, b_ptr, b_len) }
}

#[cfg(not(windows))]
/// Return the final component of a valid fat-string path.
///
/// # Safety
/// `ptr` and `len` must satisfy the fat-string ABI.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_path_file_name(ptr: *const u8, len: isize) -> ArFatStr {
    unsafe { ar_path_file_name_impl(ptr, len) }
}

#[cfg(windows)]
/// Return the final path component using the System V ABI expected by JIT code.
///
/// # Safety
/// `ptr` and `len` must satisfy the fat-string ABI.
#[unsafe(no_mangle)]
pub unsafe extern "sysv64" fn ar_path_file_name(ptr: *const u8, len: isize) -> ArFatStr {
    unsafe { ar_path_file_name_impl(ptr, len) }
}

#[cfg(not(windows))]
/// Concatenate two valid fat strings.
///
/// # Safety
/// Both pointer/length pairs must satisfy the fat-string ABI.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_str_concat(
    a_ptr: *const u8,
    a_len: isize,
    b_ptr: *const u8,
    b_len: isize,
) -> ArFatStr {
    unsafe { ar_str_concat_impl(a_ptr, a_len, b_ptr, b_len) }
}

#[cfg(windows)]
/// Concatenate two fat strings using the System V ABI expected by JIT code.
///
/// # Safety
/// Both pointer/length pairs must satisfy the fat-string ABI.
#[unsafe(no_mangle)]
pub unsafe extern "sysv64" fn ar_str_concat(
    a_ptr: *const u8,
    a_len: isize,
    b_ptr: *const u8,
    b_len: isize,
) -> ArFatStr {
    unsafe { ar_str_concat_impl(a_ptr, a_len, b_ptr, b_len) }
}

#[cfg(not(windows))]
/// Return the suffix after the final occurrence of `sep`.
///
/// # Safety
/// Both pointer/length pairs must satisfy the fat-string ABI.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_str_split_last(
    s_ptr: *const u8,
    s_len: isize,
    sep_ptr: *const u8,
    sep_len: isize,
) -> ArFatStr {
    unsafe { ar_str_split_last_impl(s_ptr, s_len, sep_ptr, sep_len) }
}

#[cfg(windows)]
/// Split a fat string using the System V ABI expected by JIT code.
///
/// # Safety
/// Both pointer/length pairs must satisfy the fat-string ABI.
#[unsafe(no_mangle)]
pub unsafe extern "sysv64" fn ar_str_split_last(
    s_ptr: *const u8,
    s_len: isize,
    sep_ptr: *const u8,
    sep_len: isize,
) -> ArFatStr {
    unsafe { ar_str_split_last_impl(s_ptr, s_len, sep_ptr, sep_len) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::poll_runtime::ar_co_make_ready_i64;

    #[test]
    fn spawn_join_ready() {
        unsafe {
            let s = ar_co_make_ready_i64(42);
            let h = ar_rt_spawn_i64(s);
            assert_eq!(ar_rt_join_i64(h), 42);
        }
    }

    #[test]
    fn path_absolute() {
        unsafe {
            let current_dir = std::env::current_dir().unwrap();
            let current_dir = current_dir.to_string_lossy();
            assert_eq!(
                ar_path_is_absolute(current_dir.as_ptr(), current_dir.len() as isize),
                1
            );
            assert_eq!(ar_path_is_absolute(b"rel".as_ptr(), 3), 0);
            assert_eq!(ar_path_is_absolute(b"".as_ptr(), 0), 0);
            assert_eq!(ar_path_is_absolute(b"./x".as_ptr(), 3), 0);
            assert_eq!(ar_path_is_empty(b"".as_ptr(), 0), 1);
        }
    }

    #[test]
    fn path_join_and_file_name() {
        unsafe {
            let j = ar_path_join(b"/tmp".as_ptr(), 4, b"x".as_ptr(), 1);
            let s = std::slice::from_raw_parts(j.ptr, j.len as usize);
            assert_eq!(
                s,
                std::path::Path::new("/tmp")
                    .join("x")
                    .to_string_lossy()
                    .as_bytes()
            );

            let j2 = ar_path_join(b"a".as_ptr(), 1, b"b".as_ptr(), 1);
            let s2 = std::slice::from_raw_parts(j2.ptr, j2.len as usize);
            assert_eq!(
                s2,
                std::path::Path::new("a")
                    .join("b")
                    .to_string_lossy()
                    .as_bytes()
            );

            let fnm = ar_path_file_name(b"/tmp/leaf".as_ptr(), 9);
            let sn = std::slice::from_raw_parts(fnm.ptr, fnm.len as usize);
            assert_eq!(sn, b"leaf");

            let leaf = ar_path_file_name(b"leaf".as_ptr(), 4);
            let sl = std::slice::from_raw_parts(leaf.ptr, leaf.len as usize);
            assert_eq!(sl, b"leaf");
        }
    }

    #[test]
    fn str_concat_prefix_suffix_split() {
        unsafe {
            assert_eq!(ar_str_len(b"hi".as_ptr(), 2), 2);
            let c = ar_str_concat(b"ab".as_ptr(), 2, b"cd".as_ptr(), 2);
            let cs = std::slice::from_raw_parts(c.ptr, c.len as usize);
            assert_eq!(cs, b"abcd");
            assert_eq!(
                ar_str_starts_with(b"hello".as_ptr(), 5, b"he".as_ptr(), 2),
                1
            );
            assert_eq!(
                ar_str_starts_with(b"hello".as_ptr(), 5, b"x".as_ptr(), 1),
                0
            );
            assert_eq!(ar_str_ends_with(b"hello".as_ptr(), 5, b"lo".as_ptr(), 2), 1);
            let tail = ar_str_split_last(b"a/b/c".as_ptr(), 5, b"/".as_ptr(), 1);
            let ts = std::slice::from_raw_parts(tail.ptr, tail.len as usize);
            assert_eq!(ts, b"c");
        }
    }
}
