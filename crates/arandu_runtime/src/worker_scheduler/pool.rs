//! Bounded, reusable worker pool with nested-work progress (SL_R scheduler).
//!
//! Admission is enforced by a bounded [`SyncSender`]: at most `admission_bound`
//! tasks wait in the queue while at most `worker_count` tasks run, so the pool
//! never builds unbounded backlog. Workers are long-lived OS threads that reuse
//! the ownership-aware [`crate::worker_runtime::WorkerTask`] transport; no
//! thread is created per task. Nested work submitted from inside a worker always
//! executes inline (self-help), so a parent can wait for its child without an
//! all-workers-waiting cycle, even while the admission queue has spare capacity.
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
    /// A worker thread never blocks on admission: nested work executes inline,
    /// guaranteeing progress regardless of the admission bound.
    #[allow(clippy::result_large_err)]
    pub fn submit(&self, task: WorkerTask) -> Result<PendingResult, (WorkerTask, PoolError)> {
        if task
            .cancel_token()
            .is_some_and(|tok| tok.load(Ordering::Acquire))
        {
            return Ok(run_inline(task));
        }
        if IN_WORKER.with(Cell::get) {
            // A nested structured operation may immediately wait for its
            // children. Always helping inline avoids the all-workers-waiting
            // cycle even when the admission queue still has spare capacity.
            return Ok(run_inline(task));
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
