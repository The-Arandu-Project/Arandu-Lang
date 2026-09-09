//! Ownership-aware, type-erased task transport for structured workers.
//!
//! This module defines transport and lifecycle only. Queueing, admission,
//! cancellation and worker selection remain scheduler responsibilities.

use crate::genref::GenError;
use crate::genref_payload::{OwnedPayload, PayloadDescriptor, UninitPayload};

/// Erased compiler-generated entry point for one statically selected job.
///
/// `context` is initialized on entry and must be consumed exactly once before
/// returning. [`WORK_COMPLETED`] means `result` was initialized exactly once;
/// every other value means it remains uninitialized. The thunk must not unwind.
pub type WorkThunk = unsafe extern "C" fn(context: *mut u8, result: *mut u8) -> i32;

pub const WORK_COMPLETED: i32 = 0;
pub const WORK_FAILED: i32 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkerError {
    Payload(GenError),
    TaskFailed,
    InvalidStatus(i32),
}

impl From<GenError> for WorkerError {
    fn from(value: GenError) -> Self {
        Self::Payload(value)
    }
}

/// One owned context plus the metadata needed to produce an owned result.
pub struct WorkerTask {
    context: Option<OwnedPayload>,
    result: PayloadDescriptor,
    thunk: WorkThunk,
}

// SAFETY: the typed constructor requires `Send` for context and result. A
// WorkerTask exposes no shared access to its erased payload and execution
// consumes the task.
unsafe impl Send for WorkerTask {}

/// Initialized result whose constructor proved the concrete type is `Send`.
#[derive(Debug)]
pub struct WorkerResult(OwnedPayload);

// SAFETY: WorkerResult is created only from a WorkerTask result descriptor.
// The safe constructor requires the corresponding concrete result to be Send.
unsafe impl Send for WorkerResult {}

impl WorkerResult {
    pub fn try_take<T: Send + 'static>(self) -> Result<T, Self> {
        match self.0.try_take::<T>() {
            Ok(value) => Ok(value),
            Err(payload) => Err(Self(payload)),
        }
    }
}

impl WorkerTask {
    /// Construct a task after pairing the erased thunk with its concrete types.
    ///
    /// # Safety
    /// The thunk must obey [`WorkThunk`]'s lifecycle contract and interpret the
    /// buffers as exactly `C` and `R`.
    pub unsafe fn try_new<C: Send + 'static, R: Send + 'static>(
        context: C,
        thunk: WorkThunk,
    ) -> Result<Self, WorkerError> {
        Ok(Self {
            context: Some(OwnedPayload::try_new(context)?),
            result: PayloadDescriptor::for_type::<R>(),
            thunk,
        })
    }

    pub fn execute(mut self) -> Result<WorkerResult, WorkerError> {
        let mut context = self.context.take().ok_or(WorkerError::TaskFailed)?;
        let mut result = UninitPayload::try_new(self.result)?;
        // SAFETY: both buffers match their descriptors. The thunk contract
        // consumes context and initializes result only for WORK_COMPLETED.
        let status = unsafe { (self.thunk)(context.as_mut_ptr(), result.as_mut_ptr()) };
        // SAFETY: return from the thunk proves context was consumed for every
        // status, so only its allocation remains owned by the runtime.
        unsafe { context.release_consumed() };
        match status {
            WORK_COMPLETED => {
                // SAFETY: WORK_COMPLETED is the thunk's initialization proof.
                Ok(WorkerResult(unsafe { result.assume_init() }))
            }
            WORK_FAILED => Err(WorkerError::TaskFailed),
            other => Err(WorkerError::InvalidStatus(other)),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use std::ptr;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

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

    unsafe extern "C" fn fail(context: *mut u8, _result: *mut u8) -> i32 {
        // SAFETY: the test pairs this thunk with a Probe context descriptor.
        drop(unsafe { ptr::read(context.cast::<Probe>()) });
        WORK_FAILED
    }

    unsafe extern "C" fn invalid(context: *mut u8, _result: *mut u8) -> i32 {
        // SAFETY: the test pairs this thunk with a Probe context descriptor.
        drop(unsafe { ptr::read(context.cast::<Probe>()) });
        99
    }

    #[repr(align(64))]
    struct AlignedResult(usize);

    #[repr(align(128))]
    struct AlignedZst;

    unsafe extern "C" fn aligned(context: *mut u8, result: *mut u8) -> i32 {
        // SAFETY: the test pairs this thunk with usize -> AlignedResult.
        let value = unsafe { ptr::read(context.cast::<usize>()) };
        assert_eq!(result.addr() % 64, 0);
        // SAFETY: result is aligned, uninitialized AlignedResult storage.
        unsafe { ptr::write(result.cast::<AlignedResult>(), AlignedResult(value)) };
        WORK_COMPLETED
    }

    unsafe extern "C" fn aligned_zst(context: *mut u8, result: *mut u8) -> i32 {
        // SAFETY: the test pairs this thunk with usize -> AlignedZst.
        let _ = unsafe { ptr::read(context.cast::<usize>()) };
        assert_eq!(result.addr() % 128, 0);
        // SAFETY: the aligned sentinel is a valid destination for a ZST write.
        unsafe { ptr::write(result.cast::<AlignedZst>(), AlignedZst) };
        WORK_COMPLETED
    }

    #[test]
    fn task_crosses_thread_and_transfers_result_ownership_once() {
        let drops = Arc::new(AtomicUsize::new(0));
        // SAFETY: complete obeys the Probe -> Probe WorkThunk contract.
        let task = unsafe {
            WorkerTask::try_new::<Probe, Probe>(
                Probe {
                    value: 41,
                    drops: Arc::clone(&drops),
                },
                complete,
            )
        }
        .unwrap();
        let result = std::thread::spawn(move || task.execute())
            .join()
            .unwrap()
            .unwrap();
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        let output = result.try_take::<Probe>().unwrap();
        assert_eq!(output.value, 42);
        drop(output);
        assert_eq!(drops.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn dropping_pending_task_drops_context_without_running() {
        let drops = Arc::new(AtomicUsize::new(0));
        // SAFETY: complete obeys the Probe -> Probe WorkThunk contract.
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
        drop(task);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn failed_task_consumes_context_without_dropping_uninitialized_result() {
        let drops = Arc::new(AtomicUsize::new(0));
        // SAFETY: fail consumes Probe and leaves the Probe result uninitialized.
        let task = unsafe {
            WorkerTask::try_new::<Probe, Probe>(
                Probe {
                    value: 1,
                    drops: Arc::clone(&drops),
                },
                fail,
            )
        }
        .unwrap();
        assert_eq!(task.execute().unwrap_err(), WorkerError::TaskFailed);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn invalid_status_fails_closed_after_consuming_context() {
        let drops = Arc::new(AtomicUsize::new(0));
        // SAFETY: invalid consumes Probe and deliberately returns an unknown
        // status while leaving result storage uninitialized.
        let task = unsafe {
            WorkerTask::try_new::<Probe, Probe>(
                Probe {
                    value: 1,
                    drops: Arc::clone(&drops),
                },
                invalid,
            )
        }
        .unwrap();
        assert_eq!(task.execute().unwrap_err(), WorkerError::InvalidStatus(99));
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn result_storage_honors_alignment_larger_than_pointer_width() {
        // SAFETY: aligned obeys the usize -> AlignedResult WorkThunk contract.
        let task = unsafe { WorkerTask::try_new::<usize, AlignedResult>(42, aligned) }.unwrap();
        let result = std::thread::spawn(move || task.execute())
            .join()
            .unwrap()
            .unwrap();
        assert_eq!(result.try_take::<AlignedResult>().unwrap().0, 42);
    }

    #[test]
    fn zero_sized_result_still_receives_an_aligned_sentinel() {
        // SAFETY: aligned_zst obeys the usize -> AlignedZst WorkThunk contract.
        let task = unsafe { WorkerTask::try_new::<usize, AlignedZst>(0, aligned_zst) }.unwrap();
        let result = task.execute().unwrap();
        assert!(result.try_take::<AlignedZst>().is_ok());
    }
}
