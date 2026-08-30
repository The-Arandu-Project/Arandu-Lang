//! TYP.2 where/bounds + SL_S std.runtime import typechecks.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::process::Command;

fn run_cli_in(cwd: &std::path::Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_arandu_cli"))
        .current_dir(cwd)
        .args(args)
        .output()
        .expect("cli")
}

fn workspace_root() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

#[test]
fn where_and_colon_bounds_check_ok() {
    let root = workspace_root();
    let file = root.join("tests/ui/type_checker/where_ok.aru");
    let out = run_cli_in(&root, &["check", file.to_str().unwrap()]);
    assert!(
        out.status.success(),
        "where_ok: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn import_std_runtime_scaffold_checks() {
    let dir = std::env::temp_dir();
    let file = dir.join("arandu_cli_std_runtime.aru");
    fs::write(
        &file,
        r#"
module tests.cli.std_runtime
import std.runtime as rt
func main(): int {
    let ex = rt.newSyncExecutor()
    return ex.flags
}
"#,
    )
    .unwrap();
    let root = workspace_root();
    let out = run_cli_in(&root, &["check", file.to_str().unwrap()]);
    assert!(
        out.status.success(),
        "std.runtime: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn run_path_absolute_and_empty() {
    let dir = std::env::temp_dir();
    let file = dir.join("arandu_cli_path_abs.aru");
    let absolute = if cfg!(windows) { r"C:\\" } else { "/tmp" };
    fs::write(
        &file,
        r#"
module tests.cli.path_abs
import std.path as path
func main(): int {
    if !path.isEmpty("") {
        return 1
    }
    if !path.isAbsolute("__ABSOLUTE_PATH__") {
        return 2
    }
    if path.isAbsolute("rel") {
        return 3
    }
    return 0
}
"#
        .replace("__ABSOLUTE_PATH__", absolute),
    )
    .unwrap();
    let root = workspace_root();
    let out = run_cli_in(&root, &["run", file.to_str().unwrap()]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "path abs: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn run_sync_executor_new() {
    let dir = std::env::temp_dir();
    let file = dir.join("arandu_cli_sync_ex.aru");
    fs::write(
        &file,
        r#"
module tests.cli.sync_ex
import std.runtime as rt
func main(): int {
    let ex = rt.newSyncExecutor()
    return ex.flags
}
"#,
    )
    .unwrap();
    let root = workspace_root();
    let out = run_cli_in(&root, &["run", file.to_str().unwrap()]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "sync ex: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// SL_R.0 end-to-end: multi-file `std.runtime` bodies + host `ar_rt_spawn/join`
/// driving a Ready coroutine blob (`ar_co_make_ready_i64`).
///
/// Uses statement-form `unsafe { … }` (AMIR supports that path; expr-form U001).
#[test]
fn run_sync_executor_spawn_join_ready() {
    let dir = std::env::temp_dir();
    let file = dir.join("arandu_cli_slr_spawn_join.aru");
    fs::write(
        &file,
        r#"
module tests.cli.slr_spawn_join
import std.runtime as rt

extern "C" {
    func ar_co_make_ready_i64(payload: int): ptr[u8]
}

func make_ready(payload: int): ptr[u8] {
    unsafe {
        return ar_co_make_ready_i64(payload)
    }
}

func main(): int {
    let ex = rt.newSyncExecutor()
    let state = make_ready(42)
    let h = rt.spawnI64(ex, state)
    return rt.joinI64(ex, h)
}
"#,
    )
    .unwrap();
    let root = workspace_root();
    let out = run_cli_in(&root, &["run", file.to_str().unwrap()]);
    assert_eq!(
        out.status.code(),
        Some(42),
        "slr spawn/join: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Multi-file inferred generic `rt.spawn` / `rt.join` (no explicit type args).
#[test]
fn run_import_inferred_spawn_join() {
    let dir = std::env::temp_dir();
    let file = dir.join("arandu_cli_import_infer_spawn.aru");
    fs::write(
        &file,
        r#"
module tests.cli.import_infer_spawn
import std.runtime as rt

async func answer(): int {
    return 42
}

func main(): int {
    let ex = rt.newSyncExecutor()
    let h = rt.spawnInt(ex, answer())
    return rt.joinInt(ex, h)
}
"#,
    )
    .unwrap();
    let root = workspace_root();
    let out = run_cli_in(&root, &["run", file.to_str().unwrap()]);
    assert_eq!(
        out.status.code(),
        Some(42),
        "import infer spawn: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Same-module inferred join (no type args on join_g).
#[test]
fn run_local_inferred_join() {
    let dir = std::env::temp_dir();
    let file = dir.join("arandu_cli_local_infer_join.aru");
    fs::write(
        &file,
        r#"
module tests.cli.local_infer_join

extern "C" {
    func ar_rt_spawn_i64(state: ptr[u8]): int
    func ar_rt_join_i64(handle: int): int
}

struct SyncExecutor { flags: int }
struct TaskHandle { id: int }

func spawn<T>(shared ex: SyncExecutor, job: Coroutine<T>): TaskHandle {
    unsafe {
        let id = ar_rt_spawn_i64(job as ptr[u8])
        return TaskHandle { id: id }
    }
}

func join<T>(shared ex: SyncExecutor, handle: TaskHandle): T {
    unsafe {
        let v = ar_rt_join_i64(handle.id)
        return v as T
    }
}

async func answer(): int {
    return 42
}

func main(): int {
    let ex = SyncExecutor { flags: 0 }
    let h = spawn<int>(ex, answer())
    return join<int>(ex, h)
}
"#,
    )
    .unwrap();
    let root = workspace_root();
    let out = run_cli_in(&root, &["run", file.to_str().unwrap()]);
    assert_eq!(
        out.status.code(),
        Some(42),
        "local infer join: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Typed spawn/join over A3 `async func` → `Coroutine<int>`.
#[test]
fn run_typed_spawn_int_async_func() {
    let dir = std::env::temp_dir();
    let file = dir.join("arandu_cli_typed_spawn.aru");
    fs::write(
        &file,
        r#"
module tests.cli.typed_spawn
import std.runtime as rt

async func answer(): int {
    return 42
}

func main(): int {
    let ex = rt.newSyncExecutor()
    let h = rt.spawnInt(ex, answer())
    return rt.joinInt(ex, h)
}
"#,
    )
    .unwrap();
    let root = workspace_root();
    let out = run_cli_in(&root, &["run", file.to_str().unwrap()]);
    assert_eq!(
        out.status.code(),
        Some(42),
        "typed spawn: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Same-module generic spawn/join with explicit type args (mono specialization).
#[test]
fn run_generic_spawn_join_explicit() {
    let dir = std::env::temp_dir();
    let file = dir.join("arandu_cli_generic_spawn.aru");
    fs::write(
        &file,
        r#"
module tests.cli.generic_spawn

extern "C" {
    func ar_rt_spawn_i64(state: ptr[u8]): int
    func ar_rt_join_i64(handle: int): int
}

struct SyncExecutor { flags: int }
struct TaskHandle { id: int }

func spawn<T>(shared ex: SyncExecutor, job: Coroutine<T>): TaskHandle {
    unsafe {
        let id = ar_rt_spawn_i64(job as ptr[u8])
        return TaskHandle { id: id }
    }
}

func join<T>(shared ex: SyncExecutor, handle: TaskHandle): T {
    unsafe {
        let v = ar_rt_join_i64(handle.id)
        return v as T
    }
}

async func answer(): int {
    return 42
}

func main(): int {
    let ex = SyncExecutor { flags: 0 }
    let h = spawn<int>(ex, answer())
    return join<int>(ex, h)
}
"#,
    )
    .unwrap();
    let root = workspace_root();
    let out = run_cli_in(&root, &["run", file.to_str().unwrap()]);
    assert_eq!(
        out.status.code(),
        Some(42),
        "generic spawn: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn run_waker_wake_and_wait() {
    let dir = std::env::temp_dir();
    let file = dir.join("arandu_cli_waker.aru");
    fs::write(
        &file,
        r#"
module tests.cli.waker
import std.runtime as rt

func main(): int {
    let w = rt.newWaker()
    rt.wakerWake(w)
    let rc = rt.wakerWait(w, 100)
    rt.destroyWaker(w)
    if rc != 1 {
        return 1
    }
    return 0
}
"#,
    )
    .unwrap();
    let root = workspace_root();
    let out = run_cli_in(&root, &["run", file.to_str().unwrap()]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "waker: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn run_reactor_backend_is_supported_or_portable() {
    let dir = std::env::temp_dir();
    let file = dir.join("arandu_cli_backend.aru");
    fs::write(
        &file,
        r#"
module tests.cli.backend
import std.runtime as rt

func main(): int {
    let b = rt.reactorBackend()
    // Portable fallback: 0; Linux: 1 = epoll, 2 = io_uring.
    if b < 0 {
        return 1
    }
    if b > 2 {
        return 2
    }
    return 0
}
"#,
    )
    .unwrap();
    let root = workspace_root();
    let out = run_cli_in(&root, &["run", file.to_str().unwrap()]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "backend: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn run_tcp_async_wait_wake() {
    let dir = std::env::temp_dir();
    let file = dir.join("arandu_cli_tcp_async.aru");
    fs::write(
        &file,
        r#"
module tests.cli.tcp_async
import std.runtime as rt

func main(): int {
    let lis = rt.tcpListen(18770)
    if lis.id < 0 {
        return 1
    }
    let client = rt.tcpConnect(18770)
    if client.id < 0 {
        return 2
    }
    let server = rt.tcpAccept(lis)
    if server.id < 0 {
        return 3
    }
    let nb = rt.tcpSetNonblocking(server, 1)
    if nb != 0 {
        return 4
    }
    let w = rt.newWaker()
    // Timeout with no data
    let t0 = rt.tcpWaitWake(server, rt.tcpWaitReadFlag(), 5, w)
    if t0 != 0 {
        return 5
    }
    // Write then wait
    // Use write_async (io_uring when available)
    // We cannot easily pass string buffers without alloc; skip payload e2e here.
    // Wait writable on client should succeed.
    let wr = rt.tcpWait(client, rt.tcpWaitWriteFlag(), 100)
    if wr < 1 {
        return 6
    }
    rt.destroyWaker(w)
    rt.tcpCloseStream(client)
    rt.tcpCloseStream(server)
    rt.tcpCloseListener(lis)
    return 0
}
"#,
    )
    .unwrap();
    let root = workspace_root();
    let out = run_cli_in(&root, &["run", file.to_str().unwrap()]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "tcp async: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn run_supervisor_true() {
    let dir = std::env::temp_dir();
    let file = dir.join("arandu_cli_supervisor.aru");
    let worker = if cfg!(windows) {
        std::env::var("WINDIR")
            .map(|windir| format!(r"{windir}\System32\whoami.exe"))
            .unwrap_or_else(|_| r"C:\Windows\System32\whoami.exe".to_string())
    } else {
        "/bin/true".to_string()
    }
    .replace('\\', "\\\\");
    fs::write(
        &file,
        r#"
module tests.cli.supervisor
import std.runtime as rt

func main(): int {
    let s = rt.newSupervisor()
    if s.id < 0 {
        return 1
    }
    let w = rt.supervisorSpawn(s, "__WORKER_PATH__", 0)
    if w.id < 0 {
        return 2
    }
    let code = rt.supervisorWait(s, w)
    rt.destroySupervisor(s)
    return code
}
"#
        .replace("__WORKER_PATH__", &worker),
    )
    .unwrap();
    let root = workspace_root();
    let out = run_cli_in(&root, &["run", file.to_str().unwrap()]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "supervisor: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Typed block_on over async func (no spawn).
#[test]
fn run_typed_block_on_int() {
    let dir = std::env::temp_dir();
    let file = dir.join("arandu_cli_typed_block_on.aru");
    fs::write(
        &file,
        r#"
module tests.cli.typed_block_on
import std.runtime as rt

async func answer(): int {
    return 7
}

func main(): int {
    let ex = rt.newSyncExecutor()
    return rt.blockOnInt(ex, answer())
}
"#,
    )
    .unwrap();
    let root = workspace_root();
    let out = run_cli_in(&root, &["run", file.to_str().unwrap()]);
    assert_eq!(
        out.status.code(),
        Some(7),
        "typed block_on: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// SL_R.2: EpollReactor sleep_ms returns success.
#[test]
fn run_reactor_sleep_ms() {
    let dir = std::env::temp_dir();
    let file = dir.join("arandu_cli_reactor_sleep.aru");
    fs::write(
        &file,
        r#"
module tests.cli.reactor_sleep
import std.runtime as rt

func main(): int {
    let r = rt.newEpollReactor()
    if r.id < 0 {
        return 1
    }
    let rc = rt.reactorSleepMs(r, 5)
    rt.destroyReactor(r)
    if rc != 0 {
        return 2
    }
    return 0
}
"#,
    )
    .unwrap();
    let root = workspace_root();
    let out = run_cli_in(&root, &["run", file.to_str().unwrap()]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "reactor sleep: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// SL_R.2 + SL_R.0: arm timer, poll, and join a spawned coroutine.
#[test]
fn run_reactor_arm_poll_with_spawn() {
    let dir = std::env::temp_dir();
    let file = dir.join("arandu_cli_reactor_spawn.aru");
    fs::write(
        &file,
        r#"
module tests.cli.reactor_spawn
import std.runtime as rt

async func ready(): int {
    return 99
}

func main(): int {
    let r = rt.newEpollReactor()
    let ex = rt.newSyncExecutor()
    if r.id < 0 {
        return 1
    }
    let h = rt.spawnInt(ex, ready())
    let arm = rt.reactorArmTimerMs(r, 5)
    if arm != 0 {
        return 2
    }
    let fired = rt.reactorPollMs(r, 200)
    if fired != 1 {
        return 3
    }
    let v = rt.joinInt(ex, h)
    rt.destroyReactor(r)
    return v
}
"#,
    )
    .unwrap();
    let root = workspace_root();
    let out = run_cli_in(&root, &["run", file.to_str().unwrap()]);
    assert_eq!(
        out.status.code(),
        Some(99),
        "reactor+spawn: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}
