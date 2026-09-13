use super::pool::{PoolError, WorkerPool};
use crate::worker_runtime::WorkerTask;
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};

/// Maximum worker thread count allowed for parallel fold execution.
const MAX_PARALLEL_WORKERS: usize = 64;

#[derive(Copy, Clone, Debug)]
struct SendPtr(*mut u8);
unsafe impl Send for SendPtr {}
unsafe impl Sync for SendPtr {}

impl SendPtr {
    #[inline]
    fn get(self) -> *mut u8 {
        self.0
    }
}

struct ParallelWorkerShared {
    next_chunk: AtomicUsize,
    num_chunks: usize,
    contexts: SendPtr,
    results: SendPtr,
    thunk: crate::worker_runtime::WorkThunk,
    stop_flag: SendPtr,
}

// SAFETY: construction is confined to `ar_rt_parallel_fold_run`, whose ABI
// contract requires all pointed-to arrays and chunk buffers to remain valid
// until the call returns.
unsafe impl Send for ParallelWorkerShared {}
unsafe impl Sync for ParallelWorkerShared {}

struct DynamicBatchWorker {
    shared: Arc<ParallelWorkerShared>,
}

#[derive(Debug, Default)]
struct DynamicBatchOutcome {
    first_error: Option<(usize, i32)>,
    externally_canceled: bool,
}

unsafe extern "C" fn run_parallel_dynamic_worker(context: *mut u8, result: *mut u8) -> i32 {
    // SAFETY: WorkerTask pairs this thunk with DynamicBatchWorker exactly once.
    let worker = unsafe { std::ptr::read(context.cast::<DynamicBatchWorker>()) };
    let outcome = execute_chunks(&worker.shared);
    // SAFETY: WorkerTask allocated result storage for DynamicBatchOutcome.
    unsafe { std::ptr::write(result.cast::<DynamicBatchOutcome>(), outcome) };
    crate::worker_runtime::WORK_COMPLETED
}

fn execute_chunks(shared: &ParallelWorkerShared) -> DynamicBatchOutcome {
    let mut outcome = DynamicBatchOutcome {
        first_error: None,
        externally_canceled: false,
    };
    loop {
        let stop = shared.stop_flag.get().cast::<AtomicI64>();
        if !stop.is_null() {
            // SAFETY: the ABI requires an aligned AtomicI64-compatible flag
            // that remains alive for the duration of this structured call.
            if unsafe { (*stop).load(Ordering::Acquire) } != 0 {
                outcome.externally_canceled = true;
                break;
            }
        }
        let index = shared.next_chunk.fetch_add(1, Ordering::Relaxed);
        if index >= shared.num_chunks {
            break;
        }
        // SAFETY: the ABI provides arrays of valid pointers; each worker receives
        // a unique `index` via atomic fetch_add, so disjoint context/result pairs are used.
        let ctx = unsafe { *shared.contexts.get().cast::<*mut u8>().add(index) };
        let res = unsafe { *shared.results.get().cast::<*mut u8>().add(index) };
        let code = unsafe { (shared.thunk)(ctx, res) };
        if code != crate::worker_runtime::WORK_COMPLETED {
            if outcome
                .first_error
                .is_none_or(|(prev_index, _)| index < prev_index)
            {
                outcome.first_error = Some((index, code));
            }
            if !stop.is_null() {
                // SAFETY: ABI contract.
                unsafe { (*stop).store(1, Ordering::Release) };
            }
            break;
        }
    }
    outcome
}

fn parallel_pool() -> Result<&'static WorkerPool, PoolError> {
    static POOL: OnceLock<Result<WorkerPool, PoolError>> = OnceLock::new();
    match POOL.get_or_init(|| {
        let workers = std::thread::available_parallelism()
            .map(usize::from)
            .unwrap_or(1)
            .clamp(1, 64);
        WorkerPool::new(workers, workers.saturating_mul(2).max(1))
    }) {
        Ok(pool) => Ok(pool),
        Err(error) => Err(*error),
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
/// - `stop_flag`, if non-null, must point to aligned storage compatible with
///   [`AtomicI64`] and remain alive until this function returns.
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

    let Ok(n) = usize::try_from(num_chunks) else {
        return crate::worker_runtime::WORK_FAILED;
    };
    let requested_workers = usize::try_from(workers).unwrap_or(usize::MAX);
    let w = requested_workers.clamp(1, MAX_PARALLEL_WORKERS).min(n);

    // Single worker fast-path: run inline without thread spawn overhead
    if w <= 1 || n == 1 {
        for i in 0..n {
            if !stop_flag.is_null()
                // SAFETY: required by the ABI contract above.
                && unsafe { (*stop_flag.cast::<AtomicI64>()).load(Ordering::Acquire) } != 0
            {
                return crate::worker_runtime::WORK_CANCELED;
            }
            let ctx = unsafe { *contexts.add(i) };
            let res = unsafe { *results.add(i) };
            let code = unsafe { (thunk)(ctx, res) };
            if code != 0 {
                if !stop_flag.is_null() {
                    // SAFETY: required by the ABI contract above.
                    unsafe { (*stop_flag.cast::<AtomicI64>()).store(1, Ordering::Release) };
                }
                return code;
            }
        }
        return 0;
    }

    let Ok(pool) = parallel_pool() else {
        return crate::worker_runtime::WORK_FAILED;
    };
    let contexts = SendPtr(contexts.cast_mut().cast::<u8>());
    let results = SendPtr(results.cast_mut().cast::<u8>());
    let stop_flag = SendPtr(stop_flag.cast::<u8>());

    let shared = Arc::new(ParallelWorkerShared {
        next_chunk: AtomicUsize::new(0),
        num_chunks: n,
        contexts,
        results,
        thunk,
        stop_flag,
    });

    let pool_workers = w.saturating_sub(1);
    let mut pending = Vec::with_capacity(pool_workers);
    let mut submission_failed = false;
    for _ in 0..pool_workers {
        let worker_task_data = DynamicBatchWorker {
            shared: Arc::clone(&shared),
        };
        let task = match unsafe {
            WorkerTask::try_new::<DynamicBatchWorker, DynamicBatchOutcome>(
                worker_task_data,
                run_parallel_dynamic_worker,
            )
        } {
            Ok(task) => task,
            Err(_) => {
                submission_failed = true;
                break;
            }
        };
        match pool.submit(task) {
            Ok(result) => pending.push(result),
            Err((_task, _error)) => {
                submission_failed = true;
                break;
            }
        }
    }

    // Work sharing: the calling thread actively runs chunks alongside pool workers.
    let main_outcome = execute_chunks(&shared);

    let mut first_error = main_outcome.first_error;
    let mut externally_canceled = main_outcome.externally_canceled;
    for result in pending {
        match result.wait().and_then(|value| {
            value
                .try_take::<DynamicBatchOutcome>()
                .map_err(|_| crate::worker_runtime::WorkerError::TaskFailed)
        }) {
            Ok(outcome) => {
                externally_canceled |= outcome.externally_canceled;
                if let Some(candidate) = outcome.first_error
                    && first_error.is_none_or(|current| candidate.0 < current.0)
                {
                    first_error = Some(candidate);
                }
            }
            Err(_) => submission_failed = true,
        }
    }
    if submission_failed {
        return crate::worker_runtime::WORK_FAILED;
    }
    if let Some((_index, code)) = first_error {
        if !stop_flag.get().is_null() {
            // SAFETY: required by the ABI contract above.
            unsafe {
                (*stop_flag.get().cast::<AtomicI64>()).store(1, Ordering::Release);
            }
        }
        return code;
    }
    if externally_canceled {
        crate::worker_runtime::WORK_CANCELED
    } else {
        crate::worker_runtime::WORK_COMPLETED
    }
}
