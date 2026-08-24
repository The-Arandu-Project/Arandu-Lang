pub mod borrow_check;
pub mod borrow_facts;
pub(crate) mod dce;
pub mod definite_init;
pub mod escape_analysis;
pub mod gen_promote;
pub mod liveness;
pub mod lower_amir;
pub mod move_checker;
pub mod optimize;
pub mod pass_manager;
pub mod pin_free;
pub(crate) mod sccp;
pub(crate) mod simplify_cfg;
pub mod suspend_check;

pub use borrow_check::check_borrows;
pub use lower_amir::lower_to_amir;
pub use move_checker::check_moves;
pub use optimize::{optimize_amir, optimize_amir_checked, optimize_amir_with_level};
pub use pass_manager::{FunctionPass, OptLevel, PassManager, PassStats};
pub use pin_free::apply_pin_free_refs;
pub use suspend_check::check_borrow_across_suspend;

pub use arandu_middle::{
    BitMatrix, BitSet, CodeReplacement, DiagCode, Diagnostic, DocCommentMap, Label,
    NO_GENERATIONAL_FALLBACK, NodeKey, ResolvedNames, ScopeId, Severity, Span, SymbolId,
    SymbolKind, SymbolTable, amir, amir_validate, cfg, diagnostics, hir, layout, literal_pool, ops,
    types,
};

pub use arandu_typeck::TypeCheckResult;

pub mod passes {
    pub mod type_checker {
        pub use arandu_middle::types;
        pub use arandu_typeck::EnumPayloadShape;
    }
}
