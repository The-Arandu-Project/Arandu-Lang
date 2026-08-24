//! SL_R — host Waker / Context for cooperative wake (no OS thread park yet).
//!
//! Wakers are explicit handles (like SyncExecutor). `wake` sets a flag;
//! `wait` blocks on a local Condvar until set or timeout.

use std::sync::{Arc, Condvar, Mutex as StdMutex};
use std::time::{Duration, Instant};

struct WakerSlot {
    state: Arc<(StdMutex<bool>, Condvar)>,
}

static WAKERS: StdMutex<Vec<Option<WakerSlot>>> = StdMutex::new(Vec::new());

fn lock() -> std::sync::MutexGuard<'static, Vec<Option<WakerSlot>>> {
    WAKERS.lock().unwrap_or_else(|e| e.into_inner())
}

/// Create a waker. Returns id >= 0.
///
/// # Safety
/// C ABI for JIT.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_rt_waker_create() -> i64 {
    let mut g = lock();
    let slot = WakerSlot {
        state: Arc::new((StdMutex::new(false), Condvar::new())),
    };
    if let Some(idx) = g.iter().position(|s| s.is_none()) {
        g[idx] = Some(slot);
        return idx as i64;
    }
    let id = g.len() as i64;
    g.push(Some(slot));
    id
}

/// Mark waker ready (idempotent).
///
/// # Safety
/// `id` from create.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_rt_waker_wake(id: i64) {
    if id < 0 {
        return;
    }
    let state = {
        let g = lock();
        g.get(id as usize)
            .and_then(|s| s.as_ref())
            .map(|s| Arc::clone(&s.state))
    };
    if let Some(state) = state {
        let (lock, cvar) = &*state;
        let mut woken = lock.lock().unwrap_or_else(|e| e.into_inner());
        *woken = true;
        cvar.notify_one();
    }
}

/// Wait until woken or `timeout_ms` elapses (-1 = forever).
/// Returns 1 if woken, 0 on timeout, -1 on error.
///
/// # Safety
/// `id` from create.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_rt_waker_wait(id: i64, timeout_ms: i64) -> i64 {
    if id < 0 {
        return -1;
    }
    let state = {
        let g = lock();
        g.get(id as usize)
            .and_then(|s| s.as_ref())
            .map(|s| Arc::clone(&s.state))
    };
    let Some(state) = state else {
        return -1;
    };

    let (lock, cvar) = &*state;
    let mut woken = lock.lock().unwrap_or_else(|e| e.into_inner());

    if *woken {
        *woken = false;
        return 1;
    }

    if timeout_ms == 0 {
        return 0;
    }

    if timeout_ms < 0 {
        while !*woken {
            woken = cvar.wait(woken).unwrap_or_else(|e| e.into_inner());
        }
        *woken = false;
        return 1;
    }

    let timeout = Duration::from_millis(timeout_ms as u64);
    let start = Instant::now();
    while !*woken {
        let elapsed = start.elapsed();
        if elapsed >= timeout {
            return 0;
        }
        let (new_woken, wait_result) = cvar
            .wait_timeout(woken, timeout - elapsed)
            .unwrap_or_else(|e| e.into_inner());
        woken = new_woken;
        if wait_result.timed_out() && !*woken {
            return 0;
        }
    }
    *woken = false;
    1
}

/// Destroy a waker handle.
///
/// # Safety
/// `id` from create.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_rt_waker_destroy(id: i64) {
    if id < 0 {
        return;
    }
    let mut g = lock();
    if let Some(slot) = g.get_mut(id as usize) {
        *slot = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wake_wait() {
        unsafe {
            let w = ar_rt_waker_create();
            ar_rt_waker_wake(w);
            assert_eq!(ar_rt_waker_wait(w, 100), 1);
            ar_rt_waker_destroy(w);
        }
    }
}
