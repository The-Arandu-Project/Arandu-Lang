#![allow(clippy::unwrap_used)]

use super::parallel::ar_rt_parallel_fold_run;
use super::pool::{PoolCore, PoolError, WorkerPool};
use crate::worker_runtime::{WORK_COMPLETED, WorkerError, WorkerTask};
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

struct Gate {
    released: Mutex<bool>,
    signal: Condvar,
}

impl Gate {
    fn new() -> Self {
        Self {
            released: Mutex::new(false),
            signal: Condvar::new(),
        }
    }

    fn release(&self) {
        *self.released.lock().unwrap() = true;
        self.signal.notify_all();
    }

    fn wait_until_released(&self) {
        let mut released = self.released.lock().unwrap();
        while !*released {
            let (guard, timeout_res) = self
                .signal
                .wait_timeout(released, Duration::from_secs(5))
                .unwrap();
            released = guard;
            if timeout_res.timed_out() {
                break;
            }
        }
    }
}

fn wait_for_flag(flag: &AtomicBool) {
    let start = std::time::Instant::now();
    while !flag.load(Ordering::SeqCst) {
        if start.elapsed() > Duration::from_secs(5) {
            panic!("timed out waiting for flag");
        }
        std::thread::yield_now();
    }
}

struct Probe {
    value: usize,
    drops: Arc<AtomicUsize>,
}

impl Drop for Probe {
    fn drop(&mut self) {
        self.drops.fetch_add(1, Ordering::SeqCst);
    }
}

unsafe extern "C" fn complete(context: *mut u8, result: *mut u8) -> i32 {
    // SAFETY: the test pairs this thunk with Probe -> Probe descriptors.
    let input = unsafe { ptr::read(context.cast::<Probe>()) };
    let output = Probe {
        value: input.value + 1,
        drops: Arc::clone(&input.drops),
    };
    drop(input);
    // SAFETY: result points to aligned, uninitialized Probe storage.
    unsafe { ptr::write(result.cast::<Probe>(), output) };
    WORK_COMPLETED
}

unsafe extern "C" fn pass(context: *mut u8, result: *mut u8) -> i32 {
    // SAFETY: the test pairs this thunk with usize -> usize descriptors.
    let value = unsafe { ptr::read(context.cast::<usize>()) };
    // SAFETY: result points to aligned, uninitialized usize storage.
    unsafe { ptr::write(result.cast::<usize>(), value) };
    WORK_COMPLETED
}

struct Parked {
    gate: Arc<Gate>,
    core: PoolCore,
    started: Option<Arc<AtomicBool>>,
}

unsafe extern "C" fn parked_root(context: *mut u8, result: *mut u8) -> i32 {
    // SAFETY: the test pairs this thunk with Parked -> usize here.
    let root = unsafe { ptr::read(context.cast::<Parked>()) };
    if let Some(started) = &root.started {
        started.store(true, Ordering::SeqCst);
    }
    root.gate.wait_until_released();
    // SAFETY: pass is paired with usize -> usize here.
    let child = unsafe { WorkerTask::try_new::<usize, usize>(7, pass) }.unwrap();
    let pending = root
        .core
        .submit(child)
        .expect("nested work must make progress");
    let child_value = pending.wait().unwrap().try_take::<usize>().unwrap();
    // SAFETY: result points to aligned, uninitialized usize storage.
    unsafe { ptr::write(result.cast::<usize>(), child_value + 1) };
    drop(root);
    WORK_COMPLETED
}

struct Blocked {
    gate: Arc<Gate>,
    started: Option<Arc<AtomicBool>>,
}

unsafe extern "C" fn hold(context: *mut u8, result: *mut u8) -> i32 {
    // SAFETY: the test pairs this thunk with Blocked -> usize.
    let blocked = unsafe { ptr::read(context.cast::<Blocked>()) };
    if let Some(started) = &blocked.started {
        started.store(true, Ordering::SeqCst);
    }
    blocked.gate.wait_until_released();
    // SAFETY: result points to aligned, uninitialized usize storage.
    unsafe { ptr::write(result.cast::<usize>(), 0) };
    drop(blocked);
    WORK_COMPLETED
}

fn hold_task_started(gate: &Arc<Gate>, started: &Arc<AtomicBool>) -> WorkerTask {
    // SAFETY: hold is paired with Blocked -> usize here.
    unsafe {
        WorkerTask::try_new::<Blocked, usize>(
            Blocked {
                gate: Arc::clone(gate),
                started: Some(Arc::clone(started)),
            },
            hold,
        )
    }
    .unwrap()
}

fn hold_task(gate: &Arc<Gate>) -> WorkerTask {
    // SAFETY: hold is paired with Blocked -> usize here.
    unsafe {
        WorkerTask::try_new::<Blocked, usize>(
            Blocked {
                gate: Arc::clone(gate),
                started: None,
            },
            hold,
        )
    }
    .unwrap()
}

#[test]
fn pool_runs_batch_and_reuses_workers() {
    let drops = Arc::new(AtomicUsize::new(0));
    let pool = WorkerPool::new(2, 4).unwrap();
    assert_eq!(pool.queued(), Some(0));
    let mut pending = Vec::with_capacity(16);
    for value in 0..16 {
        // SAFETY: complete is paired with Probe -> Probe here.
        let task = unsafe {
            WorkerTask::try_new::<Probe, Probe>(
                Probe {
                    value,
                    drops: Arc::clone(&drops),
                },
                complete,
            )
        }
        .unwrap();
        pending.push(pool.submit(task).unwrap());
    }
    let mut values = Vec::with_capacity(16);
    for result in pending {
        let out = result.wait().unwrap().try_take::<Probe>().unwrap();
        values.push(out.value);
        drop(out);
    }
    assert_eq!(values, (0..16).map(|v| v + 1).collect::<Vec<_>>());
    assert_eq!(drops.load(Ordering::SeqCst), 32);
    assert_eq!(pool.queued(), Some(0));
}

#[test]
fn queued_is_bounded_when_workers_are_blocked() {
    let gate = Arc::new(Gate::new());
    let running_started = Arc::new(AtomicBool::new(false));
    let pool = WorkerPool::new(1, 2).unwrap();

    let running = pool
        .submit(hold_task_started(&gate, &running_started))
        .unwrap();
    wait_for_flag(&running_started);

    let queued = pool.submit(hold_task(&gate)).unwrap();
    let queued_again = pool.submit(hold_task(&gate)).unwrap();
    assert_eq!(pool.queued(), Some(2));

    let (rejected, err) = pool.try_submit(hold_task(&gate)).unwrap_err();
    assert_eq!(err, PoolError::AdmissionFull);
    drop(rejected);

    gate.release();
    assert_eq!(running.wait().unwrap().try_take::<usize>().unwrap(), 0);
    assert_eq!(queued.wait().unwrap().try_take::<usize>().unwrap(), 0);
    assert_eq!(queued_again.wait().unwrap().try_take::<usize>().unwrap(), 0);
    assert_eq!(pool.queued(), Some(0));
}

#[test]
fn worker_self_help_runs_nested_task_when_admission_is_full() {
    let gate = Arc::new(Gate::new());
    let root_started = Arc::new(AtomicBool::new(false));
    let pool = WorkerPool::new(1, 1).unwrap();
    let core = pool.core();

    // SAFETY: parked_root is paired with Parked -> usize here.
    let root = unsafe {
        WorkerTask::try_new::<Parked, usize>(
            Parked {
                gate: Arc::clone(&gate),
                core,
                started: Some(Arc::clone(&root_started)),
            },
            parked_root,
        )
    }
    .unwrap();
    let root_result = pool.submit(root).unwrap();
    wait_for_flag(&root_started);

    // Fill the single admission slot while the worker is parked in root.
    let blocker = pool.submit(hold_task(&gate)).unwrap();
    assert_eq!(pool.queued(), Some(1));

    gate.release();
    let root_value = root_result.wait().unwrap().try_take::<usize>().unwrap();
    assert_eq!(root_value, 8);
    assert_eq!(blocker.wait().unwrap().try_take::<usize>().unwrap(), 0);
    assert_eq!(pool.queued(), Some(0));
}

#[test]
fn dropping_pending_receiver_releases_result_when_worker_completes() {
    let drops = Arc::new(AtomicUsize::new(0));
    let pool = WorkerPool::new(1, 2).unwrap();

    // SAFETY: complete is paired with Probe -> Probe here.
    let task = unsafe {
        WorkerTask::try_new::<Probe, Probe>(
            Probe {
                value: 1,
                drops: Arc::clone(&drops),
            },
            complete,
        )
    }
    .unwrap();
    let pending = pool.submit(task).unwrap();
    drop(pending);

    // SAFETY: pass is paired with usize -> usize here.
    let after = unsafe { WorkerTask::try_new::<usize, usize>(0, pass) }.unwrap();
    pool.submit(after)
        .unwrap()
        .wait()
        .unwrap()
        .try_take::<usize>()
        .unwrap();
    assert_eq!(drops.load(Ordering::SeqCst), 2);
}

#[test]
fn dropping_pool_drains_admitted_work_before_joining() {
    let pool = WorkerPool::new(1, 3).unwrap();
    let mut pending = Vec::with_capacity(8);
    for value in 0..8 {
        // SAFETY: pass is paired with usize -> usize here.
        let task = unsafe { WorkerTask::try_new::<usize, usize>(value, pass) }.unwrap();
        pending.push(pool.submit(task).unwrap());
    }
    drop(pool);
    let mut values = Vec::with_capacity(8);
    for result in pending {
        values.push(result.wait().unwrap().try_take::<usize>().unwrap());
    }
    assert_eq!(values, (0..8).collect::<Vec<_>>());
}

#[test]
fn external_blocking_submit_wakes_after_pool_progresses() {
    let gate = Arc::new(Gate::new());
    let root_started = Arc::new(AtomicBool::new(false));
    let pool = WorkerPool::new(1, 1).unwrap();

    let root = pool
        .submit(hold_task_started(&gate, &root_started))
        .unwrap();
    wait_for_flag(&root_started);

    // Fill the single admission slot so any subsequent external submit blocks.
    let queued = pool.submit(hold_task(&gate)).unwrap();
    assert_eq!(pool.queued(), Some(1));

    let core = pool.core();
    let entered = Arc::new(AtomicBool::new(false));
    let submit_started = Arc::new(AtomicBool::new(false));
    // SAFETY: pass is paired with usize -> usize here.
    let block_task = unsafe { WorkerTask::try_new::<usize, usize>(5, pass) }.unwrap();
    let block_handle = {
        let entered = Arc::clone(&entered);
        let submit_started = Arc::clone(&submit_started);
        let core = core.clone();
        std::thread::spawn(move || {
            submit_started.store(true, Ordering::SeqCst);
            let pending = core.submit(block_task).unwrap();
            entered.store(true, Ordering::SeqCst);
            pending.wait().unwrap().try_take::<usize>().unwrap()
        })
    };

    // Wait until the spawned thread has started its submit call.
    while !submit_started.load(Ordering::SeqCst) {
        std::thread::yield_now();
    }
    // Brief sleep ensuring it reached and blocked on sync_channel admission.
    std::thread::sleep(Duration::from_millis(20));
    assert!(!entered.load(Ordering::SeqCst));

    gate.release();
    assert_eq!(root.wait().unwrap().try_take::<usize>().unwrap(), 0);
    assert_eq!(queued.wait().unwrap().try_take::<usize>().unwrap(), 0);
    assert_eq!(block_handle.join().unwrap(), 5);
}

#[test]
fn invalid_configurations_are_rejected() {
    assert_eq!(WorkerPool::new(0, 1).unwrap_err(), PoolError::InvalidConfig);
    assert_eq!(WorkerPool::new(1, 0).unwrap_err(), PoolError::InvalidConfig);
}

#[test]
fn pre_canceled_task_does_not_block_on_full_queue_and_returns_canceled() {
    let gate = Arc::new(Gate::new());
    let root_started = Arc::new(AtomicBool::new(false));
    let pool = WorkerPool::new(1, 1).unwrap();

    // Fill worker and queue so any normal submit would block
    let root = pool
        .submit(hold_task_started(&gate, &root_started))
        .unwrap();
    wait_for_flag(&root_started);

    let queued = pool.submit(hold_task(&gate)).unwrap();
    assert_eq!(pool.queued(), Some(1));

    let cancel_flag = Arc::new(AtomicBool::new(true));
    let canceled_task = unsafe { WorkerTask::try_new::<usize, usize>(99, pass) }
        .unwrap()
        .with_cancel_token(Arc::clone(&cancel_flag));

    // Must complete inline immediately without blocking
    let pending = pool.submit(canceled_task).unwrap();
    assert_eq!(pending.wait().unwrap_err(), WorkerError::Canceled);

    gate.release();
    assert_eq!(root.wait().unwrap().try_take::<usize>().unwrap(), 0);
    assert_eq!(queued.wait().unwrap().try_take::<usize>().unwrap(), 0);
}

#[test]
fn jdk_8311867_queued_task_canceled_before_start_does_not_execute_thunk() {
    let gate = Arc::new(Gate::new());
    let root_started = Arc::new(AtomicBool::new(false));
    let pool = WorkerPool::new(1, 2).unwrap();

    // Worker is held by root task
    let root = pool
        .submit(hold_task_started(&gate, &root_started))
        .unwrap();
    wait_for_flag(&root_started);

    // Enqueue task while worker is occupied
    let executed = Arc::new(AtomicBool::new(false));
    let cancel_flag = Arc::new(AtomicBool::new(false));

    struct ExecProbe(Arc<AtomicBool>);
    unsafe extern "C" fn probe_thunk(context: *mut u8, _result: *mut u8) -> i32 {
        let probe = unsafe { std::ptr::read(context.cast::<ExecProbe>()) };
        probe.0.store(true, Ordering::SeqCst);
        WORK_COMPLETED
    }

    let task = unsafe {
        WorkerTask::try_new::<ExecProbe, usize>(ExecProbe(Arc::clone(&executed)), probe_thunk)
    }
    .unwrap()
    .with_cancel_token(Arc::clone(&cancel_flag));

    let queued = pool.submit(task).unwrap();

    // JDK-8311867 race window: task was admitted into queue, now cancel before worker picks it up
    cancel_flag.store(true, Ordering::Release);

    // Release the worker to pick up the queued task
    gate.release();

    assert_eq!(root.wait().unwrap().try_take::<usize>().unwrap(), 0);
    assert_eq!(queued.wait().unwrap_err(), WorkerError::Canceled);
    // Assert the thunk was NEVER executed!
    assert!(!executed.load(Ordering::SeqCst));
}

unsafe extern "C" fn copy_i64(context: *mut u8, result: *mut u8) -> i32 {
    // SAFETY: the ABI tests pass aligned i64 context/result pointers.
    let value = unsafe { *context.cast::<i64>() };
    unsafe { result.cast::<i64>().write(value) };
    WORK_COMPLETED
}

unsafe extern "C" fn return_context_code(context: *mut u8, _result: *mut u8) -> i32 {
    // SAFETY: the ABI test passes aligned i32 context pointers.
    unsafe { *context.cast::<i32>() }
}

#[test]
fn parallel_fold_abi_pre_cancellation_never_reports_success() {
    let mut contexts_storage = [10i64, 20];
    let mut results_storage = [-1i64, -1];
    let contexts = contexts_storage
        .iter_mut()
        .map(|value| std::ptr::from_mut(value).cast::<u8>())
        .collect::<Vec<_>>();
    let results = results_storage
        .iter_mut()
        .map(|value| std::ptr::from_mut(value).cast::<u8>())
        .collect::<Vec<_>>();
    let stop = AtomicI64::new(1);
    // SAFETY: arrays and atomic flag remain alive until the structured call returns.
    let status = unsafe {
        ar_rt_parallel_fold_run(
            2,
            contexts.as_ptr(),
            Some(copy_i64),
            results.as_ptr(),
            2,
            stop.as_ptr(),
        )
    };
    assert_eq!(status, crate::worker_runtime::WORK_CANCELED);
    assert_eq!(results_storage, [-1, -1]);
}

#[test]
fn parallel_fold_abi_initializes_every_result() {
    let mut contexts_storage = [10i64, 20, 30, 40];
    let mut results_storage = [-1i64; 4];
    let contexts = contexts_storage
        .iter_mut()
        .map(|value| std::ptr::from_mut(value).cast::<u8>())
        .collect::<Vec<_>>();
    let results = results_storage
        .iter_mut()
        .map(|value| std::ptr::from_mut(value).cast::<u8>())
        .collect::<Vec<_>>();
    // SAFETY: arrays remain alive and each worker receives a disjoint pair.
    let status = unsafe {
        ar_rt_parallel_fold_run(
            4,
            contexts.as_ptr(),
            Some(copy_i64),
            results.as_ptr(),
            4,
            std::ptr::null_mut(),
        )
    };
    assert_eq!(status, WORK_COMPLETED);
    assert_eq!(results_storage, contexts_storage);
}

#[test]
fn parallel_fold_abi_selects_the_lowest_failing_ordinal() {
    let mut codes = [0i32, 7, 3, 0];
    let mut results_storage = [0i32; 4];
    let contexts = codes
        .iter_mut()
        .map(|value| std::ptr::from_mut(value).cast::<u8>())
        .collect::<Vec<_>>();
    let results = results_storage
        .iter_mut()
        .map(|value| std::ptr::from_mut(value).cast::<u8>())
        .collect::<Vec<_>>();
    // SAFETY: arrays remain alive and each worker receives a disjoint pair.
    let status = unsafe {
        ar_rt_parallel_fold_run(
            4,
            contexts.as_ptr(),
            Some(return_context_code),
            results.as_ptr(),
            4,
            std::ptr::null_mut(),
        )
    };
    assert_eq!(status, 7);
}
