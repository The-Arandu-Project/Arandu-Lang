//! Sandboxed temporary directory creation and safe containment cleanup.

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use super::context::with_active_context;

pub(crate) static NONCE_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Creates a safe sandboxed temporary directory for the active test case.
/// Returns `1` on success, `0` on failure.
///
/// # Safety
/// No pointer args; `nonce_val` is a plain integer. Always safe to call from JIT.
fn ar_test_temp_dir_impl(nonce_val: i64) -> crate::rt_runtime::ArFatStr {
    let counter = NONCE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let clock = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let nonce =
        blake3::hash(format!("{}:{counter}:{clock}:{nonce_val}", std::process::id()).as_bytes());

    let mut created = String::new();
    with_active_context(|ctx| {
        let dir_name = format!("t_{}", nonce.to_hex());
        let target = ctx.temp_root.join(dir_name);
        if std::fs::create_dir(&target).is_ok() {
            created = target.to_string_lossy().into_owned();
            ctx.temp_dirs.push(target);
        }
    });
    crate::rt_runtime::fat_str_from_string(created)
}

#[cfg(not(windows))]
#[unsafe(no_mangle)]
/// Creates a contained temporary directory and returns its fat-string path.
///
/// # Safety
/// Uses the Arandu fat-string return ABI expected by generated code.
pub unsafe extern "C" fn ar_test_temp_dir(nonce_val: i64) -> crate::rt_runtime::ArFatStr {
    ar_test_temp_dir_impl(nonce_val)
}

#[cfg(windows)]
#[unsafe(no_mangle)]
/// Creates a contained temporary directory and returns its fat-string path.
///
/// # Safety
/// Uses the System V ABI selected by the Cranelift multi-value string contract.
pub unsafe extern "sysv64" fn ar_test_temp_dir(nonce_val: i64) -> crate::rt_runtime::ArFatStr {
    ar_test_temp_dir_impl(nonce_val)
}

/// Validates containment and safely removes a temporary directory.
pub(crate) fn safe_cleanup_temp_dir(temp_root: &Path, dir: &Path) {
    let Ok(canonical_root) = temp_root.canonicalize() else {
        return;
    };
    if let Ok(canonical_dir) = dir.canonicalize() {
        // Ensure the directory is strictly contained within temp_root (no symlink escape)
        if canonical_dir.starts_with(&canonical_root) && canonical_dir != canonical_root {
            let _ = std::fs::remove_dir_all(&canonical_dir);
        }
    }
}
