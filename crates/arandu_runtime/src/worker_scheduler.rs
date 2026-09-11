//! Bounded, reusable worker pool with nested-work progress (SL_R scheduler).
//!
//! Admission is enforced by a bounded [`SyncSender`]: at most `admission_bound`
//! tasks wait in the queue while at most `worker_count` tasks run, so the pool
//! never builds unbounded backlog. Workers are long-lived OS threads that reuse
//! the ownership-aware [`crate::worker_runtime::WorkerTask`] transport; no
//! thread is created per task. Nested work submitted from inside a worker never
//! blocks: when admission is exhausted the submitting worker executes the task
//! inline (self-help), so one worker with nested work makes progress without
//! spawning recursive pools.
//!
//! Results travel over per-task one-shot channels ([`PendingResult`]). Dropping
//! a receiver while its task runs does not reclaim the running callback; the
//! task completes and its result is released by the worker. Dropping the pool
//! disconnects the queue, drains already-admitted work, then joins the workers.

use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{
    Receiver, SendError, Sender, SyncSender, TrySendError, channel, sync_channel,
};
use std::sync::{Arc, Mutex, Weak};
use std::thread::JoinHandle;

use crate::worker_runtime::{WorkerError, WorkerResult, WorkerTask};

/// Why a pooled submission was refused or delivery failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PoolError {
    /// The bounded queue is full (and the submitting thread is not a worker).
    AdmissionFull,
    /// The pool is shutting down or already gone.
    ShuttingDown,
    /// A worker vanished before delivering its outcome.
    WorkerLost,
    /// At least one worker thread could not be created.
    ThreadSpawn,
    /// Invalid configuration (zero workers or a zero admission bound).
    InvalidConfig,
}

thread_local! {
    static IN_WORKER: Cell<bool> = const { Cell::new(false) };
}

type TaskOutcome = Result<WorkerResult, WorkerError>;

enum Envelope {
    Run {
        task: WorkerTask,
        result_tx: Sender<TaskOutcome>,
    },
}

#[derive(Debug)]
struct PoolState {
    sender: SyncSender<Envelope>,
    /// Number of admitted-but-not-yet-picked tasks (advisory).
    queued: Arc<AtomicUsize>,
}

/// Send-half of the pool, shareable across threads without owning the workers.
///
/// Sharing a `PoolCore` (for example inside a worker task's context) does not
/// keep the queue alive: it holds a weak reference to the pool state, so the
/// pool still drains and joins when the owning [`WorkerPool`] is dropped.
#[derive(Clone)]
pub struct PoolCore {
    state: Weak<PoolState>,
}

impl PoolCore {
    /// Enqueue a task without blocking.
    ///
    /// On `AdmissionFull` the task is returned untouched so the caller can
    /// retry. When called from a pool worker, full admission does not discard
    /// or delay the task: the worker executes it inline to preserve progress.
    #[allow(clippy::result_large_err)]
    pub fn try_submit(&self, task: WorkerTask) -> Result<PendingResult, (WorkerTask, PoolError)> {
        if task
            .cancel_token()
            .is_some_and(|tok| tok.load(Ordering::Acquire))
        {
            return Ok(run_inline(task));
        }
        let (result_tx, result_rx) = channel();
        let Some(state) = self.state.upgrade() else {
            return Err((task, PoolError::ShuttingDown));
        };
        state.queued.fetch_add(1, Ordering::AcqRel);
        match state.sender.try_send(Envelope::Run { task, result_tx }) {
            Ok(()) => Ok(PendingResult { rx: result_rx }),
            Err(TrySendError::Full(Envelope::Run { task, result_tx })) => {
                if IN_WORKER.with(Cell::get) {
                    let _ = result_tx.send(task.execute());
                    state.queued.fetch_sub(1, Ordering::AcqRel);
                    Ok(PendingResult { rx: result_rx })
                } else {
                    state.queued.fetch_sub(1, Ordering::AcqRel);
                    drop(result_tx);
                    Err((task, PoolError::AdmissionFull))
                }
            }
            Err(TrySendError::Disconnected(Envelope::Run { task, .. })) => {
                state.queued.fetch_sub(1, Ordering::AcqRel);
                Err((task, PoolError::ShuttingDown))
            }
        }
    }

    /// Enqueue a task, blocking until admission is available.
    ///
    /// A worker thread never blocks on admission: it enqueues when there is
    /// room and executes inline when the queue is full, guaranteeing nested
    /// progress regardless of the admission bound.
    #[allow(clippy::result_large_err)]
    pub fn submit(&self, task: WorkerTask) -> Result<PendingResult, (WorkerTask, PoolError)> {
        if task
            .cancel_token()
            .is_some_and(|tok| tok.load(Ordering::Acquire))
        {
            return Ok(run_inline(task));
        }
        if IN_WORKER.with(Cell::get) {
            return match self.try_submit(task) {
                Ok(pending) => Ok(pending),
                Err((task, PoolError::AdmissionFull)) => Ok(run_inline(task)),
                Err((task, err)) => Err((task, err)),
            };
        }
        let Some(state) = self.state.upgrade() else {
            return Err((task, PoolError::ShuttingDown));
        };
        let (result_tx, result_rx) = channel();
        state.queued.fetch_add(1, Ordering::AcqRel);
        match state.sender.send(Envelope::Run { task, result_tx }) {
            Ok(()) => Ok(PendingResult { rx: result_rx }),
            Err(SendError(Envelope::Run { task, .. })) => {
                state.queued.fetch_sub(1, Ordering::AcqRel);
                Err((task, PoolError::ShuttingDown))
            }
        }
    }

    /// Number of tasks currently admitted but not yet picked (advisory).
    pub fn queued(&self) -> Option<usize> {
        self.state
            .upgrade()
            .map(|state| state.queued.load(Ordering::Acquire))
    }
}

impl std::fmt::Debug for PoolCore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PoolCore").finish_non_exhaustive()
    }
}

/// Handle for one submitted task's delivery.
#[derive(Debug)]
pub struct PendingResult {
    rx: Receiver<TaskOutcome>,
}

impl PendingResult {
    /// Block until the task finishes and take its owned result.
    ///
    /// `WorkerError::Canceled` means the pool went away (or the producing
    /// worker died) before the outcome could be delivered.
    pub fn wait(self) -> Result<WorkerResult, WorkerError> {
        match self.rx.recv() {
            Ok(outcome) => outcome,
            Err(_) => Err(WorkerError::Canceled),
        }
    }
}

/// A set of reusable workers sharing one bounded admission queue.
#[derive(Debug)]
pub struct WorkerPool {
    state: Option<Arc<PoolState>>,
    workers: Vec<JoinHandle<()>>,
}

impl WorkerPool {
    /// Build a pool with `worker_count` long-lived threads and an admission
    /// bound of `admission_bound` queued tasks.
    ///
    /// Failure at thread creation tears down the already-started workers and
    /// returns `PoolError::ThreadSpawn`.
    pub fn new(worker_count: usize, admission_bound: usize) -> Result<Self, PoolError> {
        if worker_count == 0 {
            return Err(PoolError::InvalidConfig);
        }
        if admission_bound == 0 {
            return Err(PoolError::InvalidConfig);
        }
        let (sender, rx) = sync_channel(admission_bound);
        let queued = Arc::new(AtomicUsize::new(0));
        let shared_rx = Arc::new(Mutex::new(rx));
        let mut workers = Vec::with_capacity(worker_count);
        for index in 0..worker_count {
            let worker_rx = Arc::clone(&shared_rx);
            let worker_queued = Arc::clone(&queued);
            match std::thread::Builder::new()
                .name(format!("arandu-worker-{index}"))
                .spawn(move || worker_main(worker_rx, worker_queued))
            {
                Ok(handle) => workers.push(handle),
                Err(_) => {
                    drop(sender);
                    for handle in workers {
                        let _ = handle.join();
                    }
                    return Err(PoolError::ThreadSpawn);
                }
            }
        }
        Ok(Self {
            state: Some(Arc::new(PoolState { sender, queued })),
            workers,
        })
    }

    /// Shareable submission handle for this pool.
    pub fn core(&self) -> PoolCore {
        match self.state.as_ref() {
            Some(state) => PoolCore {
                state: Arc::downgrade(state),
            },
            None => PoolCore { state: Weak::new() },
        }
    }

    /// Non-blocking submission; see [`PoolCore::try_submit`].
    #[allow(clippy::result_large_err)]
    pub fn try_submit(&self, task: WorkerTask) -> Result<PendingResult, (WorkerTask, PoolError)> {
        self.core().try_submit(task)
    }

    /// Blocking submission; see [`PoolCore::submit`].
    #[allow(clippy::result_large_err)]
    pub fn submit(&self, task: WorkerTask) -> Result<PendingResult, (WorkerTask, PoolError)> {
        self.core().submit(task)
    }

    /// Number of tasks currently admitted but not yet picked (advisory).
    pub fn queued(&self) -> Option<usize> {
        self.core().queued()
    }
}

impl Drop for WorkerPool {
    fn drop(&mut self) {
        // Dropping the strong state disconnects the queue: workers drain
        // already-admitted work, deliver outcomes, then exit on recv failure.
        self.state.take();
        let workers = std::mem::take(&mut self.workers);
        for handle in workers {
            let _ = handle.join();
        }
    }
}

#[derive(Copy, Clone)]
struct SendPtr(*mut u8);
unsafe impl Send for SendPtr {}
unsafe impl Sync for SendPtr {}

impl SendPtr {
    #[inline]
    fn get(self) -> *mut u8 {
        self.0
    }
}

/// C ABI entry point for executing `num_chunks` with `thunk` across worker threads.
///
/// Returns:
/// - 0 on success (all chunks completed, result buffers populated).
/// - 1 on failure (at least one chunk returned error).
/// - 2 on cancellation (canceled via stop_flag or cooperative token).
///
/// # Safety
/// - `contexts` must point to an array of at least `num_chunks` valid context pointers.
/// - `results` must point to an array of at least `num_chunks` valid destination pointers.
/// - `thunk` must be safe to call concurrently with disjoint context/result pairs.
/// - `stop_flag`, if non-null, must point to an aligned, readable/writable volatile `i64`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ar_rt_parallel_fold_run(
    num_chunks: u64,
    contexts: *const *mut u8,
    thunk: Option<crate::worker_runtime::WorkThunk>,
    results: *const *mut u8,
    workers: u64,
    stop_flag: *mut i64,
) -> i32 {
    let Some(thunk) = thunk else {
        return 1;
    };
    if num_chunks == 0 {
        return 0;
    }
    if contexts.is_null() || results.is_null() {
        return 1;
    }

    let n = num_chunks as usize;
    let w = (workers as usize).clamp(1, 64).min(n);

    // Single worker fast-path: run inline without thread spawn overhead
    if w <= 1 || n == 1 {
        for i in 0..n {
            if !stop_flag.is_null() && unsafe { std::ptr::read_volatile(stop_flag) } != 0 {
                return 2;
            }
            let ctx = unsafe { *contexts.add(i) };
            let res = unsafe { *results.add(i) };
            let code = unsafe { (thunk)(ctx, res) };
            if code != 0 {
                if !stop_flag.is_null() {
                    unsafe { std::ptr::write_volatile(stop_flag, 1) };
                }
                return code;
            }
        }
        return 0;
    }

    // Parallel multi-worker execution with structured scope
    let contexts_vec: Vec<SendPtr> = (0..n)
        .map(|i| unsafe { SendPtr(*contexts.add(i)) })
        .collect();
    let results_vec: Vec<SendPtr> = (0..n)
        .map(|i| unsafe { SendPtr(*results.add(i)) })
        .collect();
    let stop_flag_addr = SendPtr(stop_flag.cast::<u8>());

    // Shadow raw pointers so closures only access SendPtr wrappers
    let _ = (contexts, results, stop_flag);

    let next_chunk = std::sync::atomic::AtomicUsize::new(0);
    let first_error = std::sync::atomic::AtomicI32::new(0);
    let stopped = std::sync::atomic::AtomicBool::new(false);

    std::thread::scope(|s| {
        for _ in 0..w {
            s.spawn(|| {
                let stop_ptr = stop_flag_addr.get().cast::<i64>();
                while !stopped.load(std::sync::atomic::Ordering::Acquire) {
                    let idx = next_chunk.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    if idx >= n {
                        break;
                    }
                    // JDK-8311867 pre-admission check: check before starting chunk
                    if stopped.load(std::sync::atomic::Ordering::Acquire)
                        || (!stop_ptr.is_null()
                            && unsafe { std::ptr::read_volatile(stop_ptr) } != 0)
                    {
                        break;
                    }

                    let ctx = contexts_vec[idx].get();
                    let res = results_vec[idx].get();

                    let code = unsafe { (thunk)(ctx, res) };
                    if code != 0 {
                        let _ = first_error.compare_exchange(
                            0,
                            code,
                            std::sync::atomic::Ordering::SeqCst,
                            std::sync::atomic::Ordering::SeqCst,
                        );
                        stopped.store(true, std::sync::atomic::Ordering::Release);
                        if !stop_ptr.is_null() {
                            unsafe { std::ptr::write_volatile(stop_ptr, 1) };
                        }
                        break;
                    }
                }
            });
        }
    });

    first_error.load(std::sync::atomic::Ordering::SeqCst)
}

fn run_inline(task: WorkerTask) -> PendingResult {
    let (result_tx, result_rx) = channel();
    let _ = result_tx.send(task.execute());
    PendingResult { rx: result_rx }
}

fn worker_main(rx: Arc<Mutex<Receiver<Envelope>>>, queued: Arc<AtomicUsize>) {
    IN_WORKER.with(|flag| flag.set(true));
    loop {
        let envelope = match rx.lock() {
            Ok(guard) => match guard.recv() {
                Ok(envelope) => envelope,
                Err(_) => break,
            },
            Err(_) => break,
        };
        queued.fetch_sub(1, Ordering::AcqRel);
        let Envelope::Run { task, result_tx } = envelope;
        let _ = result_tx.send(task.execute());
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::worker_runtime::WORK_COMPLETED;
    use std::ptr;
    use std::sync::atomic::{AtomicBool, Ordering};
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
}
