//! Bounded, priority-aware worker scheduler for IDE jobs.

use arandu_query::DocumentId;
use lsp_server::RequestId;
use rustc_hash::FxHashMap;
use std::collections::VecDeque;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;

const DEFAULT_QUEUE_CAPACITY: usize = 64;
type JobFn = Box<dyn FnOnce(CancellationToken) + Send + 'static>;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum JobKey {
    Request(RequestId),
    Diagnostics(DocumentId),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Priority {
    Interactive,
    Background,
}

#[derive(Clone, Debug)]
pub struct CancellationToken(Arc<AtomicBool>);

impl CancellationToken {
    fn new() -> Self {
        Self(Arc::new(AtomicBool::new(false)))
    }

    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }

    fn same_as(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

struct Job {
    key: Option<JobKey>,
    cancellation: CancellationToken,
    run: JobFn,
}

#[derive(Default)]
struct QueueState {
    interactive: VecDeque<Job>,
    background: VecDeque<Job>,
    active: FxHashMap<JobKey, CancellationToken>,
    shutdown: bool,
}

struct Shared {
    state: Mutex<QueueState>,
    ready: Condvar,
    capacity: usize,
}

pub struct WorkerPool {
    shared: Arc<Shared>,
}

impl Drop for WorkerPool {
    fn drop(&mut self) {
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        state.shutdown = true;
        for token in state.active.values() {
            token.cancel();
        }
        drop(state);
        self.shared.ready.notify_all();
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct QueueFull;

impl WorkerPool {
    pub fn new(workers: usize) -> std::io::Result<Self> {
        Self::with_capacity(workers, DEFAULT_QUEUE_CAPACITY)
    }

    fn with_capacity(workers: usize, capacity: usize) -> std::io::Result<Self> {
        let shared = Arc::new(Shared {
            state: Mutex::new(QueueState::default()),
            ready: Condvar::new(),
            capacity: capacity.max(1),
        });
        for i in 0..workers.clamp(1, 16) {
            let shared = Arc::clone(&shared);
            thread::Builder::new()
                .name(format!("arandu-lsp-worker-{i}"))
                .spawn(move || worker_loop(&shared))?;
        }
        Ok(Self { shared })
    }

    pub fn spawn<F>(
        &self,
        priority: Priority,
        key: Option<JobKey>,
        f: F,
    ) -> Result<CancellationToken, QueueFull>
    where
        F: FnOnce(CancellationToken) + Send + 'static,
    {
        let cancellation = CancellationToken::new();
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(key) = key.as_ref() {
            remove_queued_key(&mut state, key);
            if let Some(previous) = state.active.insert(key.clone(), cancellation.clone()) {
                previous.cancel();
            }
        }
        if state.interactive.len() + state.background.len() >= self.shared.capacity {
            if priority == Priority::Interactive {
                if let Some(evicted) = state.background.pop_front() {
                    retire_job(&mut state, &evicted);
                    evicted.cancellation.cancel();
                } else {
                    remove_active_token(&mut state, key.as_ref(), &cancellation);
                    return Err(QueueFull);
                }
            } else {
                remove_active_token(&mut state, key.as_ref(), &cancellation);
                return Err(QueueFull);
            }
        }
        let job = Job {
            key,
            cancellation: cancellation.clone(),
            run: Box::new(f),
        };
        match priority {
            Priority::Interactive => state.interactive.push_back(job),
            Priority::Background => state.background.push_back(job),
        }
        drop(state);
        self.shared.ready.notify_one();
        Ok(cancellation)
    }

    pub fn cancel(&self, key: &JobKey) -> bool {
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        let Some(token) = state.active.get(key).cloned() else {
            return false;
        };
        token.cancel();
        let queued = take_queued_key(&mut state, key);
        if let Some(job) = queued.as_ref() {
            retire_job(&mut state, job);
        }
        drop(state);
        if let Some(job) = queued {
            let _ = catch_unwind(AssertUnwindSafe(|| (job.run)(token)));
        }
        true
    }

    pub fn cancel_requests(&self) {
        let keys: Vec<JobKey> = {
            let state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
            state
                .active
                .keys()
                .filter(|key| matches!(key, JobKey::Request(_)))
                .cloned()
                .collect()
        };
        for key in keys {
            let _ = self.cancel(&key);
        }
    }
}

fn worker_loop(shared: &Shared) {
    loop {
        let job = {
            let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
            while state.interactive.is_empty() && state.background.is_empty() && !state.shutdown {
                state = shared.ready.wait(state).unwrap_or_else(|e| e.into_inner());
            }
            if state.shutdown && state.interactive.is_empty() && state.background.is_empty() {
                return;
            }
            state
                .interactive
                .pop_front()
                .or_else(|| state.background.pop_front())
                .expect("worker woke with a queued job")
        };
        let token = job.cancellation.clone();
        let key = job.key.clone();
        let _ = catch_unwind(AssertUnwindSafe(|| (job.run)(token.clone())));
        let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
        remove_active_token(&mut state, key.as_ref(), &token);
    }
}

fn remove_queued_key(state: &mut QueueState, key: &JobKey) {
    if let Some(job) = take_queued_key(state, key) {
        job.cancellation.cancel();
    }
}

fn take_queued_key(state: &mut QueueState, key: &JobKey) -> Option<Job> {
    for queue in [&mut state.interactive, &mut state.background] {
        if let Some(index) = queue.iter().position(|job| job.key.as_ref() == Some(key)) {
            return queue.remove(index);
        }
    }
    None
}

fn retire_job(state: &mut QueueState, job: &Job) {
    remove_active_token(state, job.key.as_ref(), &job.cancellation);
}

fn remove_active_token(
    state: &mut QueueState,
    key: Option<&JobKey>,
    cancellation: &CancellationToken,
) {
    let Some(key) = key else { return };
    if state
        .active
        .get(key)
        .is_some_and(|active| active.same_as(cancellation))
    {
        state.active.remove(key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn panic_in_one_job_does_not_kill_the_worker() {
        let pool = WorkerPool::new(1).expect("test worker must start");
        let (tx, rx) = mpsc::channel();
        pool.spawn(Priority::Interactive, None, |_| {
            panic!("synthetic worker failure")
        })
        .expect("panic job must queue");
        pool.spawn(Priority::Interactive, None, move |_| {
            tx.send(42).expect("test receiver must remain alive")
        })
        .expect("follow-up job must queue");
        assert_eq!(rx.recv_timeout(Duration::from_secs(2)), Ok(42));
    }

    #[test]
    fn interactive_job_overtakes_background_backlog() {
        let pool = WorkerPool::with_capacity(1, 4).expect("test worker must start");
        let (release_tx, release_rx) = mpsc::channel();
        let (order_tx, order_rx) = mpsc::channel();
        pool.spawn(Priority::Background, None, move |_| {
            release_rx.recv().expect("release first job");
        })
        .expect("blocking job must queue");
        for value in 1..=2 {
            let tx = order_tx.clone();
            pool.spawn(Priority::Background, None, move |_| {
                tx.send(value).expect("record background order")
            })
            .expect("background job must queue");
        }
        let tx = order_tx.clone();
        pool.spawn(Priority::Interactive, None, move |_| {
            tx.send(0).expect("record interactive order")
        })
        .expect("interactive job must queue");
        release_tx.send(()).expect("release worker");
        assert_eq!(order_rx.recv_timeout(Duration::from_secs(2)), Ok(0));
    }

    #[test]
    fn keyed_background_jobs_coalesce_and_cancel_previous_work() {
        let pool = WorkerPool::with_capacity(1, 3).expect("test worker must start");
        let (release_tx, release_rx) = mpsc::channel();
        let (tx, rx) = mpsc::channel();
        pool.spawn(Priority::Interactive, None, move |_| {
            release_rx.recv().expect("release first job");
        })
        .expect("blocking job must queue");
        let key = JobKey::Request(7.into());
        let first = pool
            .spawn(Priority::Background, Some(key.clone()), {
                let tx = tx.clone();
                move |_| tx.send(1).expect("record stale job")
            })
            .expect("first keyed job must queue");
        pool.spawn(Priority::Background, Some(key), move |_| {
            tx.send(2).expect("record replacement job")
        })
        .expect("replacement keyed job must queue");
        assert!(first.is_cancelled());
        release_tx.send(()).expect("release worker");
        assert_eq!(rx.recv_timeout(Duration::from_secs(2)), Ok(2));
        assert!(rx.recv_timeout(Duration::from_millis(50)).is_err());
    }

    #[test]
    fn bounded_queue_drops_background_and_admits_interactive_work() {
        let pool = WorkerPool::with_capacity(1, 1).expect("test worker must start");
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        pool.spawn(Priority::Background, None, move |_| {
            started_tx.send(()).expect("signal running job");
            release_rx.recv().expect("release running job");
        })
        .expect("running job must queue");
        started_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("worker must start first job");

        let background = pool
            .spawn(Priority::Background, None, |_| {})
            .expect("one background job fits");
        assert!(matches!(
            pool.spawn(Priority::Background, None, |_| {}),
            Err(QueueFull)
        ));
        pool.spawn(Priority::Interactive, None, |_| {})
            .expect("interactive work evicts queued background work");
        assert!(background.is_cancelled());
        release_tx.send(()).expect("release running job");
    }

    #[test]
    fn cancelling_a_queued_request_runs_its_short_cancel_path_immediately() {
        let pool = WorkerPool::with_capacity(1, 2).expect("test worker must start");
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        pool.spawn(Priority::Background, None, move |_| {
            started_tx.send(()).expect("signal running job");
            release_rx.recv().expect("release running job");
        })
        .expect("running job must queue");
        started_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("worker must start first job");

        let key = JobKey::Request(9.into());
        let (tx, rx) = mpsc::channel();
        pool.spawn(Priority::Interactive, Some(key.clone()), move |token| {
            tx.send(token.is_cancelled()).expect("record cancellation")
        })
        .expect("request must queue");
        assert!(pool.cancel(&key));
        assert_eq!(rx.recv_timeout(Duration::from_millis(100)), Ok(true));
        release_tx.send(()).expect("release running job");
    }
}
