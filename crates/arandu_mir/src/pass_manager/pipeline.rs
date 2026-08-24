//! Standard pipelines per [`OptLevel`].
//!
//! New passes join these lists at named positions. Keep O0 empty (LLVM-style:
//! only semantically required work belongs at O0 — here, nothing).

use super::{FunctionPass, OptLevel, PassResources};
use crate::amir::AmirFunc;
use crate::{Diagnostic, dce::mark_sweep_dce, sccp::sccp, simplify_cfg::simplify_cfg};

/// Sparse conditional constant propagation + branch folding.
struct SccpPass;

impl FunctionPass for SccpPass {
    fn name(&self) -> &'static str {
        "sccp"
    }

    fn run(
        &self,
        func: &mut AmirFunc,
        resources: &mut PassResources<'_>,
    ) -> Result<bool, Diagnostic> {
        Ok(sccp(func, resources.literal_pool, resources.bump))
    }
}

/// Mark-sweep dead code elimination (keeps side effects and jump arguments).
struct MarkSweepDcePass;

impl FunctionPass for MarkSweepDcePass {
    fn name(&self) -> &'static str {
        "mark_sweep_dce"
    }

    fn run(
        &self,
        func: &mut AmirFunc,
        _resources: &mut PassResources<'_>,
    ) -> Result<bool, Diagnostic> {
        mark_sweep_dce(func)
    }
}

/// CFG simplification: block merging, unreachable removal, jump threading.
struct SimplifyCfgPass;

impl FunctionPass for SimplifyCfgPass {
    fn name(&self) -> &'static str {
        "simplify_cfg"
    }

    fn run(
        &self,
        func: &mut AmirFunc,
        resources: &mut PassResources<'_>,
    ) -> Result<bool, Diagnostic> {
        simplify_cfg(func, resources.bump)
    }
}

/// Builds the standard pass sequence for `level`.
///
/// O1/O2/Os share the core simplification loop today; the positions below are
/// the extension points where escape analysis, gen-promotion and pin-free
/// passes join as they land (roadmap Fase 1 / Plano Gold).
pub fn pipeline_for_level(level: OptLevel) -> Vec<Box<dyn FunctionPass>> {
    match level {
        OptLevel::O0 => Vec::new(),
        OptLevel::O1 | OptLevel::O2 | OptLevel::Os => vec![
            Box::new(SccpPass),
            Box::new(MarkSweepDcePass),
            Box::new(SimplifyCfgPass),
        ],
    }
}
