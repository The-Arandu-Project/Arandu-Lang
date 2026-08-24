//! Host symbols for `std.alloc.vec` (two layers — do not conflate).
//!
//! ## Canonical path (L6.1 pure-buffer)
//! Language code in `stdlib/alloc/vec.aru` uses only:
//! - `ar_vec_malloc` / `ar_vec_realloc` / `ar_vec_buf_free` (raw bytes)
//! - mem intrinsics (`sizeOf` / `ptrOffset` / `ptrRead` / `ptrWrite`) for typed access
//!
//! ## Legacy handle API (GenArena-style table)
//! `ar_vec_new` / `ar_vec_push` / `ar_vec_get` / … remain registered for JIT
//! unit tests and any residual host-table experiments. **They are not the
//! stdlib surface.** Prefer pure-buffer when changing product behaviour.
//!
//! Elements on the handle API are **i64 bit patterns**. Typed Drop / non-int
//! payloads remain post-Minimal.
//!
//! # Safety
//! All `pub unsafe extern "C"` entry points are ABI host functions invoked only
//! from JIT-compiled Arandu code (or unit tests). Handles are opaque indices
//! into the process-local table; invalid ids are treated as no-ops or abort
//! on write paths that would corrupt storage.

#![allow(clippy::missing_safety_doc)]

use std::sync::Mutex;

struct Slot {
    data: Vec<i64>,
}

static VECS: Mutex<Vec<Option<Slot>>> = Mutex::new(Vec::new());

fn lock() -> std::sync::MutexGuard<'static, Vec<Option<Slot>>> {
    VECS.lock().unwrap_or_else(|e| e.into_inner())
}

/// Create an empty vector; returns handle `>= 0`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_vec_new() -> i64 {
    let mut g = lock();
    let slot = Slot { data: Vec::new() };
    if let Some(idx) = g.iter().position(|s| s.is_none()) {
        g[idx] = Some(slot);
        return idx as i64;
    }
    let id = g.len();
    g.push(Some(slot));
    id as i64
}

/// Push `value` onto vector `id`. Invalid id aborts.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_vec_push(id: i64, value: i64) {
    let mut g = lock();
    let Some(Some(slot)) = g.get_mut(id as usize) else {
        std::process::abort();
    };
    slot.data.push(value);
}

/// Length of vector `id`, or `-1` if invalid.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_vec_len(id: i64) -> i64 {
    let g = lock();
    match g.get(id as usize).and_then(|s| s.as_ref()) {
        Some(slot) => slot.data.len() as i64,
        None => -1,
    }
}

/// `1` if `index` is in range, else `0`. Invalid id → `0`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_vec_has(id: i64, index: i64) -> i64 {
    if index < 0 {
        return 0;
    }
    let g = lock();
    match g.get(id as usize).and_then(|s| s.as_ref()) {
        Some(slot) if (index as usize) < slot.data.len() => 1,
        _ => 0,
    }
}

/// Get element at `index`. Invalid / OOB → `0` (check [`ar_vec_has`] first).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_vec_get(id: i64, index: i64) -> i64 {
    if index < 0 {
        return 0;
    }
    let g = lock();
    match g.get(id as usize).and_then(|s| s.as_ref()) {
        Some(slot) => slot.data.get(index as usize).copied().unwrap_or(0),
        None => 0,
    }
}

/// Overwrite index; returns `1` on success, `0` on OOB/invalid.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_vec_put(id: i64, index: i64, value: i64) -> i64 {
    if index < 0 {
        return 0;
    }
    let mut g = lock();
    let Some(Some(slot)) = g.get_mut(id as usize) else {
        return 0;
    };
    match slot.data.get_mut(index as usize) {
        Some(cell) => {
            *cell = value;
            1
        }
        None => 0,
    }
}

/// Pop last element and return it. Caller must ensure non-empty (`ar_vec_len > 0`).
/// Empty / invalid → `0` (ambiguous with a stored zero — check length first).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_vec_pop(id: i64) -> i64 {
    let mut g = lock();
    let Some(Some(slot)) = g.get_mut(id as usize) else {
        return 0;
    };
    slot.data.pop().unwrap_or(0)
}

/// Set length to 0; capacity retained.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_vec_clear(id: i64) {
    let mut g = lock();
    if let Some(Some(slot)) = g.get_mut(id as usize) {
        slot.data.clear();
    }
}

/// Destroy handle and free storage.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_vec_destroy(id: i64) {
    let mut g = lock();
    if (id as usize) < g.len() {
        g[id as usize] = None;
    }
}

// ── Raw buffer helpers for pure-Arandu Vec growth (L6.1) ─────────────────

/// Allocate `size` bytes (8-aligned). Null on OOM / invalid size.
///
/// # Safety
/// JIT host only; free with [`ar_vec_buf_free`] using the same size.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_vec_malloc(size: i64) -> *mut u8 {
    if size <= 0 {
        return std::ptr::null_mut();
    }
    let layout = match std::alloc::Layout::from_size_align(size as usize, 8) {
        Ok(l) => l,
        Err(_) => return std::ptr::null_mut(),
    };
    unsafe { std::alloc::alloc(layout) }
}

/// Free buffer from [`ar_vec_malloc`].
///
/// # Safety
/// `p`/`size` must match a prior `ar_vec_malloc` pair (or `p` null).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_vec_buf_free(p: *mut u8, size: i64) {
    if p.is_null() || size <= 0 {
        return;
    }
    let layout = match std::alloc::Layout::from_size_align(size as usize, 8) {
        Ok(l) => l,
        Err(_) => return,
    };
    unsafe { std::alloc::dealloc(p, layout) }
}

/// Grow/shrink raw buffer; copies `min(old,new)` bytes.
///
/// # Safety
/// Same as malloc/free pair.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_vec_realloc(p: *mut u8, old_size: i64, new_size: i64) -> *mut u8 {
    if new_size <= 0 {
        unsafe { ar_vec_buf_free(p, old_size) };
        return std::ptr::null_mut();
    }
    let new_ptr = unsafe { ar_vec_malloc(new_size) };
    if new_ptr.is_null() {
        return std::ptr::null_mut();
    }
    if !p.is_null() && old_size > 0 {
        let n = std::cmp::min(old_size, new_size) as usize;
        unsafe {
            std::ptr::copy_nonoverlapping(p, new_ptr, n);
            ar_vec_buf_free(p, old_size);
        }
    } else if p.is_null() && old_size > 0 {
        // Caller capacity out of sync with data (mut writeback partial) — treat
        // as fresh alloc without free/copy of a null base.
    }
    new_ptr
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_get_len_free() {
        unsafe {
            let id = ar_vec_new();
            ar_vec_push(id, 10);
            ar_vec_push(id, 20);
            assert_eq!(ar_vec_len(id), 2);
            assert_eq!(ar_vec_has(id, 0), 1);
            assert_eq!(ar_vec_get(id, 0), 10);
            assert_eq!(ar_vec_get(id, 1), 20);
            assert_eq!(ar_vec_put(id, 1, 5), 1);
            assert_eq!(ar_vec_get(id, 1), 5);
            assert_eq!(ar_vec_pop(id), 5);
            assert_eq!(ar_vec_len(id), 1);
            ar_vec_destroy(id);
            assert_eq!(ar_vec_len(id), -1);
        }
    }
}
