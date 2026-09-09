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
    pub fn try_submit(&self, task: WorkerTask) -> Result<PendingResult, (WorkerTask, PoolError)> {
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
    pub fn submit(&self, task: WorkerTask) -> Result<PendingResult, (WorkerTask, PoolError)> {
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
    pub fn try_submit(&self, task: WorkerTask) -> Result<PendingResult, (WorkerTask, PoolError)> {
        self.core().try_submit(task)
    }

    /// Blocking submission; see [`PoolCore::submit`].
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
                released = self.signal.wait(released).unwrap();
            }
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
    }

    unsafe extern "C" fn parked_root(context: *mut u8, result: *mut u8) -> i32 {
        // SAFETY: the test pairs this thunk with Parked -> usize here.
        let root = unsafe { ptr::read(context.cast::<Parked>()) };
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
    }

    unsafe extern "C" fn hold(context: *mut u8, result: *mut u8) -> i32 {
        // SAFETY: the test pairs this thunk with Blocked -> usize.
        let blocked = unsafe { ptr::read(context.cast::<Blocked>()) };
        blocked.gate.wait_until_released();
        // SAFETY: result points to aligned, uninitialized usize storage.
        unsafe { ptr::write(result.cast::<usize>(), 0) };
        drop(blocked);
        WORK_COMPLETED
    }

    fn hold_task(gate: &Arc<Gate>) -> WorkerTask {
        // SAFETY: hold is paired with Blocked -> usize here.
        unsafe {
            WorkerTask::try_new::<Blocked, usize>(
                Blocked {
                    gate: Arc::clone(gate),
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
        let pool = WorkerPool::new(1, 2).unwrap();

        let running = pool.submit(hold_task(&gate)).unwrap();
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
        let pool = WorkerPool::new(1, 1).unwrap();
        let core = pool.core();

        // SAFETY: parked_root is paired with Parked -> usize here.
        let root = unsafe {
            WorkerTask::try_new::<Parked, usize>(
                Parked {
                    gate: Arc::clone(&gate),
                    core,
                },
                parked_root,
            )
        }
        .unwrap();
        let root_result = pool.submit(root).unwrap();

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
        let pool = WorkerPool::new(1, 1).unwrap();

        let root = pool.submit(hold_task(&gate)).unwrap();
        // Fill the single admission slot so the external submit below blocks.
        let queued = pool.submit(hold_task(&gate)).unwrap();
        assert_eq!(pool.queued(), Some(1));

        let core = pool.core();
        let entered = Arc::new(AtomicBool::new(false));
        // SAFETY: pass is paired with usize -> usize here.
        let block_task = unsafe { WorkerTask::try_new::<usize, usize>(5, pass) }.unwrap();
        let block_handle = {
            let entered = Arc::clone(&entered);
            let core = core.clone();
            std::thread::spawn(move || {
                let pending = core.submit(block_task).unwrap();
                entered.store(true, Ordering::SeqCst);
                pending.wait().unwrap().try_take::<usize>().unwrap()
            })
        };

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
}
