//! Standard pipelines per [`OptLevel`].
//!
//! New passes join these lists at named positions. Keep O0 empty (LLVM-style:
//! only semantically required work belongs at O0 — here, nothing).

use super::{FunctionPass, OptLevel, PassResources};
use crate::amir::AmirFunc;
use crate::{
    Diagnostic, dce::mark_sweep_dce, gvn::gvn, sccp::sccp, simplify_cfg::simplify_cfg, sroa::sroa,
};

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

/// Scalar Replacement of Aggregates — limited to aggregate field forwarding.
///
/// Propagates scalar operands from `StructLiteral`/`Tuple` assignments into
/// subsequent `FieldAccess` projections. Does **not** decompose stack slots,
/// model escapes, or handle stores through projections.
struct SroaPass;

impl FunctionPass for SroaPass {
    fn name(&self) -> &'static str {
        "sroa"
    }

    fn run(
        &self,
        func: &mut AmirFunc,
        _resources: &mut PassResources<'_>,
    ) -> Result<bool, Diagnostic> {
        Ok(sroa(func))
    }
}

/// Global Value Numbering (GVN).
struct GvnPass;

impl FunctionPass for GvnPass {
    fn name(&self) -> &'static str {
        "gvn"
    }

    fn run(
        &self,
        func: &mut AmirFunc,
        _resources: &mut PassResources<'_>,
    ) -> Result<bool, Diagnostic> {
        Ok(gvn(func))
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
/// O1 runs baseline simplification (SCCP → DCE → SimplifyCFG).
/// O2 adds SROA and GVN before the O1 core.
///
/// LICM and TCO exist as modules but are **not wired** until:
/// - LICM: DenseRange insertion is corrected, trapping ops are excluded from
///   hoisting, and iteration order is deterministic.
/// - TCO: parameter comparison uses `func.params` instead of
///   `block[0].params`, and `AmirStmtKind` is kept in sync.
pub fn pipeline_for_level(level: OptLevel) -> Vec<Box<dyn FunctionPass>> {
    match level {
        OptLevel::O0 => Vec::new(),
        OptLevel::O1 | OptLevel::Os => vec![
            Box::new(SccpPass),
            Box::new(MarkSweepDcePass),
            Box::new(SimplifyCfgPass),
        ],
        OptLevel::O2 => vec![
            Box::new(SroaPass),
            Box::new(GvnPass),
            Box::new(SccpPass),
            Box::new(MarkSweepDcePass),
            Box::new(SimplifyCfgPass),
        ],
    }
}
