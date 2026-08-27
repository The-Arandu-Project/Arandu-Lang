//! Host runtime support for `std.testing` (SL_T.3).
//!
//! Provides process-isolated, deterministic testing context:
//! - Structured expectations (`expect`, `expectEqual`, `fail`, `skip`) without relying on panic text.
//! - Single-evaluation capture of expected/actual values and expressions.
//! - Deterministic LIFO cleanup stack executed on success, failure, or skip.
//! - Bounded log buffer (preventing unbounded allocation).
//! - Sandboxed `temp_dir` with cryptographic/nonce isolation and containment validation.

use arandu_codegen::testing::{TestFailure, TestStatus};
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_LOG_ENTRIES: usize = 1000;
const MAX_LOG_TOTAL_BYTES: usize = 64 * 1024; // 64 KB

type CleanupFn = Box<dyn FnOnce() + Send>;

/// Active test execution context.
pub struct TestContext {
    pub id: String,
    pub seed: u64,
    pub status: TestStatus,
    pub failure: Option<TestFailure>,
    pub secondary_failures: Vec<TestFailure>,
    pub skip_reason: Option<String>,
    pub logs: Vec<String>,
    pub log_bytes: usize,
    pub logs_truncated: bool,
    pub cleanups: Vec<CleanupFn>,
    pub temp_dirs: Vec<PathBuf>,
    pub temp_root: PathBuf,
    pub current_location: Option<String>,
}

impl TestContext {
    pub fn new(id: String, seed: u64, temp_root: PathBuf) -> Self {
        Self {
            id,
            seed,
            status: TestStatus::Passed,
            failure: None,
            secondary_failures: Vec::new(),
            skip_reason: None,
            logs: Vec::new(),
            log_bytes: 0,
            logs_truncated: false,
            cleanups: Vec::new(),
            temp_dirs: Vec::new(),
            temp_root,
            current_location: None,
        }
    }

    pub fn record_failure(&mut self, failure: TestFailure) {
        if self.status == TestStatus::Skipped {
            return;
        }
        if self.failure.is_none() {
            self.status = TestStatus::Failed;
            self.failure = Some(failure);
        } else {
            self.secondary_failures.push(failure);
        }
    }

    pub fn record_skip(&mut self, reason: String, location: Option<String>) {
        if self.status == TestStatus::Passed {
            self.status = TestStatus::Skipped;
            self.skip_reason = Some(reason.clone());
            self.failure = Some(TestFailure {
                operation: Some("skip".to_string()),
                message: reason,
                location,
                expression: None,
                expected: None,
                actual: None,
                type_name: None,
                cause: None,
            });
        }
    }

    pub fn log(&mut self, msg: String) {
        if self.logs.len() >= MAX_LOG_ENTRIES || self.log_bytes + msg.len() > MAX_LOG_TOTAL_BYTES {
            self.logs_truncated = true;
            return;
        }
        self.log_bytes += msg.len();
        self.logs.push(msg);
    }

    pub fn add_cleanup(&mut self, cleanup: impl FnOnce() + Send + 'static) {
        self.cleanups.push(Box::new(cleanup));
    }
}

/// Records the source span of the next `std.testing` operation. The compiler
/// emits this call immediately before the public testing helper call.
#[unsafe(no_mangle)]
pub extern "C" fn ar_test_set_span(file_id: i64, start: i64, end: i64) {
    with_active_context(|ctx| {
        ctx.current_location = Some(format!("{file_id}:{start}:{end}"));
    });
}

fn effective_location(location: Option<String>) -> Option<String> {
    if location.is_some() {
        return location;
    }
    with_active_context(|ctx| ctx.current_location.take()).flatten()
}

thread_local! {
    static ACTIVE_CONTEXT: RefCell<Option<TestContext>> = const { RefCell::new(None) };
}

static NONCE_COUNTER: AtomicU64 = AtomicU64::new(1);

fn with_active_context<R>(f: impl FnOnce(&mut TestContext) -> R) -> Option<R> {
    ACTIVE_CONTEXT.with(|cell| cell.borrow_mut().as_mut().map(f))
}

/// Initializes the test context for a given test case before execution.
pub fn init_test_context(id: &str, seed: u64, temp_root: Option<PathBuf>) {
    let root = temp_root.unwrap_or_else(|| {
        let default_dir =
            std::env::temp_dir().join(format!("arandu-test-ctx-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&default_dir);
        default_dir
    });
    let ctx = TestContext::new(id.to_string(), seed, root);
    ACTIVE_CONTEXT.with(|cell| {
        *cell.borrow_mut() = Some(ctx);
    });
}

/// Result collected after executing a test and running all cleanups.
#[derive(Debug, Clone)]
pub struct TestContextResult {
    pub id: String,
    pub status: TestStatus,
    pub failure: Option<TestFailure>,
    pub secondary_failures: Vec<TestFailure>,
    pub logs: Vec<String>,
    pub logs_truncated: bool,
}

/// Finishes the active test context: runs LIFO cleanups, cleans up contained temp dirs,
/// and returns the final outcome.
pub fn finish_test_context() -> TestContextResult {
    let ctx = ACTIVE_CONTEXT.with(|cell| cell.borrow_mut().take());

    let Some(mut ctx) = ctx else {
        return TestContextResult {
            id: String::new(),
            status: TestStatus::Passed,
            failure: None,
            secondary_failures: Vec::new(),
            logs: Vec::new(),
            logs_truncated: false,
        };
    };

    // 1. Run all cleanups in LIFO order (reverse of registration).
    while let Some(cleanup) = ctx.cleanups.pop() {
        let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(cleanup));
        if res.is_err() {
            ctx.record_failure(TestFailure::simple("test cleanup handler panicked"));
        }
    }

    // 2. Safely remove registered temporary directories with containment validation.
    for dir in &ctx.temp_dirs {
        safe_cleanup_temp_dir(&ctx.temp_root, dir);
    }

    TestContextResult {
        id: ctx.id,
        status: ctx.status,
        failure: ctx.failure,
        secondary_failures: ctx.secondary_failures,
        logs: ctx.logs,
        logs_truncated: ctx.logs_truncated,
    }
}

/// Registers a cleanup closure for the active test case.
pub fn register_cleanup(f: impl FnOnce() + Send + 'static) {
    with_active_context(|ctx| {
        ctx.add_cleanup(f);
    });
}

/// Validates containment and safely removes a temporary directory.
fn safe_cleanup_temp_dir(temp_root: &Path, dir: &Path) {
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

// ─── C-ABI Host Exports for Arandu Programs ─────────────────────────────────

unsafe fn parse_str_arg(ptr: *const u8, len: i64) -> Option<String> {
    if len <= 0 || ptr.is_null() {
        return None;
    }
    let len = usize::try_from(len).ok()?;
    let slice = unsafe { std::slice::from_raw_parts(ptr, len) };
    std::str::from_utf8(slice).ok().map(|s| s.to_string())
}

/// Evaluates a generic boolean expectation.
/// Returns 1 on success, 0 on failure.
///
/// # Safety
/// All pointer args must be valid UTF-8 fat-string slices (ptr + len) from JIT-compiled Arandu
/// code. Caller must not pass dangling or misaligned pointers. Zero-length strings (`len == 0`)
/// are accepted and treated as absent.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_test_expect(
    condition: i64,
    expr_ptr: *const u8,
    expr_len: i64,
    msg_ptr: *const u8,
    msg_len: i64,
    loc_ptr: *const u8,
    loc_len: i64,
) -> i64 {
    if condition != 0 {
        return 1;
    }
    let expr = unsafe { parse_str_arg(expr_ptr, expr_len) };
    let msg = unsafe { parse_str_arg(msg_ptr, msg_len) };
    let loc = effective_location(unsafe { parse_str_arg(loc_ptr, loc_len) });

    with_active_context(|ctx| {
        let failure = TestFailure::expectation(
            "expect",
            expr,
            Some("true".to_string()),
            Some("false".to_string()),
            Some("bool".to_string()),
            msg,
            loc,
        );
        ctx.record_failure(failure);
    });
    0
}

/// Compares two integer values for equality.
/// Returns 1 if equal, 0 if not equal.
///
/// # Safety
/// Pointer args must be valid UTF-8 fat-string slices from JIT-compiled Arandu code.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_test_expect_equal_i64(
    expected: i64,
    actual: i64,
    expr_ptr: *const u8,
    expr_len: i64,
    msg_ptr: *const u8,
    msg_len: i64,
    loc_ptr: *const u8,
    loc_len: i64,
) -> i64 {
    if expected == actual {
        return 1;
    }
    let expr = unsafe { parse_str_arg(expr_ptr, expr_len) };
    let msg = unsafe { parse_str_arg(msg_ptr, msg_len) };
    let loc = effective_location(unsafe { parse_str_arg(loc_ptr, loc_len) });

    with_active_context(|ctx| {
        let failure = TestFailure::expectation(
            "expect_equal",
            expr,
            Some(expected.to_string()),
            Some(actual.to_string()),
            Some("int".to_string()),
            msg,
            loc,
        );
        ctx.record_failure(failure);
    });
    0
}

/// Compares two floating point values for equality.
/// Returns 1 if equal (or bitwise equal for NaN), 0 if not equal.
///
/// # Safety
/// Pointer args must be valid UTF-8 fat-string slices from JIT-compiled Arandu code.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_test_expect_equal_f64(
    expected: f64,
    actual: f64,
    expr_ptr: *const u8,
    expr_len: i64,
    msg_ptr: *const u8,
    msg_len: i64,
    loc_ptr: *const u8,
    loc_len: i64,
) -> i64 {
    if (expected == actual) || (expected.is_nan() && actual.is_nan()) {
        return 1;
    }
    let expr = unsafe { parse_str_arg(expr_ptr, expr_len) };
    let msg = unsafe { parse_str_arg(msg_ptr, msg_len) };
    let loc = effective_location(unsafe { parse_str_arg(loc_ptr, loc_len) });

    with_active_context(|ctx| {
        let failure = TestFailure::expectation(
            "expect_equal",
            expr,
            Some(expected.to_string()),
            Some(actual.to_string()),
            Some("float".to_string()),
            msg,
            loc,
        );
        ctx.record_failure(failure);
    });
    0
}

/// Compares two boolean values for equality.
/// Returns 1 if equal, 0 if not equal.
///
/// # Safety
/// Pointer args must be valid UTF-8 fat-string slices from JIT-compiled Arandu code.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_test_expect_equal_bool(
    expected: i64,
    actual: i64,
    expr_ptr: *const u8,
    expr_len: i64,
    msg_ptr: *const u8,
    msg_len: i64,
    loc_ptr: *const u8,
    loc_len: i64,
) -> i64 {
    let exp_bool = expected != 0;
    let act_bool = actual != 0;
    if exp_bool == act_bool {
        return 1;
    }
    let expr = unsafe { parse_str_arg(expr_ptr, expr_len) };
    let msg = unsafe { parse_str_arg(msg_ptr, msg_len) };
    let loc = effective_location(unsafe { parse_str_arg(loc_ptr, loc_len) });

    with_active_context(|ctx| {
        let failure = TestFailure::expectation(
            "expect_equal",
            expr,
            Some(exp_bool.to_string()),
            Some(act_bool.to_string()),
            Some("bool".to_string()),
            msg,
            loc,
        );
        ctx.record_failure(failure);
    });
    0
}

/// Compares two fat strings for equality.
/// Returns 1 if equal, 0 if not equal.
///
/// # Safety
/// All pointer args must be valid UTF-8 fat-string slices from JIT-compiled Arandu code.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_test_expect_equal_str(
    exp_ptr: *const u8,
    exp_len: i64,
    act_ptr: *const u8,
    act_len: i64,
    expr_ptr: *const u8,
    expr_len: i64,
    msg_ptr: *const u8,
    msg_len: i64,
    loc_ptr: *const u8,
    loc_len: i64,
) -> i64 {
    let exp_str = unsafe { parse_str_arg(exp_ptr, exp_len) }.unwrap_or_default();
    let act_str = unsafe { parse_str_arg(act_ptr, act_len) }.unwrap_or_default();

    if exp_str == act_str {
        return 1;
    }
    let expr = unsafe { parse_str_arg(expr_ptr, expr_len) };
    let msg = unsafe { parse_str_arg(msg_ptr, msg_len) };
    let loc = effective_location(unsafe { parse_str_arg(loc_ptr, loc_len) });

    with_active_context(|ctx| {
        let failure = TestFailure::expectation(
            "expect_equal",
            expr,
            Some(exp_str),
            Some(act_str),
            Some("str".to_string()),
            msg,
            loc,
        );
        ctx.record_failure(failure);
    });
    0
}

/// Explicitly fails the active test with a message.
///
/// # Safety
/// Pointer args must be valid UTF-8 fat-string slices from JIT-compiled Arandu code.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_test_fail(
    msg_ptr: *const u8,
    msg_len: i64,
    loc_ptr: *const u8,
    loc_len: i64,
) {
    let msg = unsafe { parse_str_arg(msg_ptr, msg_len) }
        .unwrap_or_else(|| "test failed explicitly".to_string());
    let loc = effective_location(unsafe { parse_str_arg(loc_ptr, loc_len) });

    with_active_context(|ctx| {
        let failure = TestFailure {
            operation: Some("fail".to_string()),
            message: msg,
            location: loc,
            expression: None,
            expected: None,
            actual: None,
            type_name: None,
            cause: None,
        };
        ctx.record_failure(failure);
    });
}

/// Explicitly skips the active test with a reason.
///
/// # Safety
/// Pointer args must be valid UTF-8 fat-string slices from JIT-compiled Arandu code.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_test_skip(
    reason_ptr: *const u8,
    reason_len: i64,
    loc_ptr: *const u8,
    loc_len: i64,
) {
    let reason = unsafe { parse_str_arg(reason_ptr, reason_len) }
        .unwrap_or_else(|| "test skipped explicitly".to_string());
    let loc = effective_location(unsafe { parse_str_arg(loc_ptr, loc_len) });

    with_active_context(|ctx| {
        ctx.record_skip(reason, loc);
    });
}

/// Appends a message to the test context log buffer.
///
/// # Safety
/// `msg_ptr` must be a valid UTF-8 fat-string slice from JIT-compiled Arandu code.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_test_log(msg_ptr: *const u8, msg_len: i64) {
    if let Some(msg) = unsafe { parse_str_arg(msg_ptr, msg_len) } {
        with_active_context(|ctx| {
            ctx.log(msg);
        });
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expect_passes_and_fails_structurally() {
        let temp = std::env::temp_dir().join("ar_test_ctx_unit1");
        let _ = std::fs::create_dir_all(&temp);
        init_test_context("pkg::mod::test1", 42, Some(temp.clone()));

        unsafe {
            let res = ar_test_expect(1, b"1 == 1".as_ptr(), 6, b"".as_ptr(), 0, b"".as_ptr(), 0);
            assert_eq!(res, 1);
        }

        let out = finish_test_context();
        assert_eq!(out.status, TestStatus::Passed);
        assert!(out.failure.is_none());

        init_test_context("pkg::mod::test2", 42, Some(temp));
        unsafe {
            let res = ar_test_expect(
                0,
                b"x > 10".as_ptr(),
                6,
                b"x must be > 10".as_ptr(),
                14,
                b"main.aru:10".as_ptr(),
                11,
            );
            assert_eq!(res, 0);
        }
        let out2 = finish_test_context();
        assert_eq!(out2.status, TestStatus::Failed);
        let failure = out2.failure.unwrap();
        assert_eq!(failure.operation.as_deref(), Some("expect"));
        assert_eq!(failure.expression.as_deref(), Some("x > 10"));
        assert_eq!(failure.message, "x must be > 10");
        assert_eq!(failure.location.as_deref(), Some("main.aru:10"));
    }

    #[test]
    fn expect_equal_i64_and_str() {
        let temp = std::env::temp_dir().join("ar_test_ctx_unit2");
        let _ = std::fs::create_dir_all(&temp);
        init_test_context("pkg::mod::test_eq", 42, Some(temp));

        unsafe {
            assert_eq!(
                ar_test_expect_equal_i64(10, 10, b"".as_ptr(), 0, b"".as_ptr(), 0, b"".as_ptr(), 0),
                1
            );
            assert_eq!(
                ar_test_expect_equal_i64(
                    10,
                    20,
                    b"a == b".as_ptr(),
                    6,
                    b"".as_ptr(),
                    0,
                    b"".as_ptr(),
                    0
                ),
                0
            );
        }

        let out = finish_test_context();
        assert_eq!(out.status, TestStatus::Failed);
        let f = out.failure.unwrap();
        assert_eq!(f.expected.as_deref(), Some("10"));
        assert_eq!(f.actual.as_deref(), Some("20"));
    }

    #[test]
    fn lifo_cleanup_executes_on_failure() {
        let temp = std::env::temp_dir().join("ar_test_ctx_unit3");
        let _ = std::fs::create_dir_all(&temp);
        init_test_context("pkg::mod::test_cleanup", 42, Some(temp));

        use std::sync::Arc;
        use std::sync::Mutex;
        let trace = Arc::new(Mutex::new(Vec::new()));

        let t1 = trace.clone();
        register_cleanup(move || {
            t1.lock().unwrap().push(1);
        });

        let t2 = trace.clone();
        register_cleanup(move || {
            t2.lock().unwrap().push(2);
        });

        unsafe {
            ar_test_fail(b"boom".as_ptr(), 4, b"".as_ptr(), 0);
        }

        let out = finish_test_context();
        assert_eq!(out.status, TestStatus::Failed);
        // LIFO: cleanup 2 was registered second, so it must execute first: [2, 1]
        assert_eq!(*trace.lock().unwrap(), vec![2, 1]);
    }

    #[test]
    fn skip_marks_status_skipped() {
        let temp = std::env::temp_dir().join("ar_test_ctx_unit4");
        let _ = std::fs::create_dir_all(&temp);
        init_test_context("pkg::mod::test_skip", 42, Some(temp));

        unsafe {
            ar_test_skip(b"not supported on windows".as_ptr(), 24, b"".as_ptr(), 0);
        }

        let out = finish_test_context();
        assert_eq!(out.status, TestStatus::Skipped);
        assert_eq!(out.failure.unwrap().message, "not supported on windows");
    }

    #[test]
    fn preserves_primary_and_secondary_failures_and_truncates_logs() {
        let temp = std::env::temp_dir().join("ar_test_ctx_unit5");
        let _ = std::fs::create_dir_all(&temp);
        init_test_context("pkg::mod::test_multiple", 42, Some(temp));
        with_active_context(|ctx| {
            ctx.record_failure(TestFailure::simple("primary"));
            ctx.record_failure(TestFailure::simple("secondary"));
            ctx.log("x".repeat(MAX_LOG_TOTAL_BYTES));
            ctx.log("overflow".to_string());
        });
        let out = finish_test_context();
        assert_eq!(
            out.failure.as_ref().map(|f| f.message.as_str()),
            Some("primary")
        );
        assert_eq!(out.secondary_failures.len(), 1);
        assert_eq!(out.secondary_failures[0].message, "secondary");
        assert!(out.logs_truncated);
    }

    #[test]
    fn temp_dir_returns_a_usable_path_and_cleanup_removes_it() {
        let root = std::env::temp_dir().join(format!(
            "ar_test_ctx_temp_{}",
            NONCE_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&root).unwrap();
        init_test_context("pkg::mod::test_temp", 42, Some(root.clone()));
        let returned = unsafe { ar_test_temp_dir(0) };
        let bytes = unsafe {
            std::slice::from_raw_parts(returned.ptr, usize::try_from(returned.len).unwrap())
        };
        let path = PathBuf::from(std::str::from_utf8(bytes).unwrap());
        assert!(path.is_dir());
        let _ = finish_test_context();
        assert!(!path.exists());
        let _ = std::fs::remove_dir(&root);
    }

    #[cfg(unix)]
    #[test]
    fn cleanup_rejects_unix_symlink_escape() {
        use std::os::unix::fs::symlink;
        let root = std::env::temp_dir().join("ar_test_ctx_symlink_root");
        let outside = std::env::temp_dir().join("ar_test_ctx_symlink_outside");
        let link = root.join("escape");
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&outside);
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("sentinel"), b"safe").unwrap();
        symlink(&outside, &link).unwrap();
        safe_cleanup_temp_dir(&root, &link);
        assert!(outside.join("sentinel").is_file());
        let _ = std::fs::remove_file(&link);
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&outside);
    }

    #[cfg(windows)]
    #[test]
    fn cleanup_rejects_windows_symlink_escape_when_supported() {
        use std::os::windows::fs::symlink_dir;
        let root = std::env::temp_dir().join("ar_test_ctx_symlink_root");
        let outside = std::env::temp_dir().join("ar_test_ctx_symlink_outside");
        let link = root.join("escape");
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&outside);
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("sentinel"), b"safe").unwrap();
        if symlink_dir(&outside, &link).is_ok() {
            safe_cleanup_temp_dir(&root, &link);
            assert!(outside.join("sentinel").is_file());
            let _ = std::fs::remove_dir(&link);
        }
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&outside);
    }
}
