//! Bounded, reusable worker pool with nested-work progress (SL_R scheduler).

pub mod parallel;
pub mod pool;

pub use parallel::ar_rt_parallel_fold_run;
pub use pool::{PendingResult, PoolCore, PoolError, WorkerPool};

#[cfg(test)]
mod tests;
