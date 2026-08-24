use super::constraints::Constraint;
use super::{TypeChecker, errors};

// ── Solver (roadmap 4.1) ────────────────────────────────────────────

/// A constraint failure that has been through the solver, in generation
/// order.
///
/// The checker unifies eagerly, so solving a failed constraint currently
/// means rendering its flow-based diagnostic. Keeping the structured
/// [`Constraint`] alongside the index of its rendered diagnostic gives the
/// Causal Provenance Graph (roadmap 4.2) a stable, ordered input without
/// touching generation call sites again.
#[derive(Debug, Clone)]
pub struct SolvedConstraint {
    /// The failed constraint as generated; both types are interned in
    /// `TypeInfo::type_interner`.
    pub constraint: Constraint,
    /// Index of the rendered diagnostic inside [`TypeChecker::diagnostics`].
    pub diag_index: usize,
}

/// Solver entry point: render `constraint` into a diagnostic, push it onto
/// `tc.diagnostics` and record the solve in generation order.
pub(crate) fn fail_constraint(tc: &mut TypeChecker<'_>, constraint: Constraint) {
    let diag = errors::constraint_to_diagnostic(&constraint, &tc.symbols, &tc.type_info);
    let diag_index = tc.diagnostics.len();
    tc.diagnostics.push(diag);
    tc.solved_constraints.push(SolvedConstraint {
        constraint,
        diag_index,
    });
}

impl TypeChecker<'_> {
    /// All constraint failures solved so far, oldest first. Indices align
    /// with generation order, not with `diagnostics` filtering.
    #[must_use]
    pub fn solved_constraints(&self) -> &[SolvedConstraint] {
        &self.solved_constraints
    }
}
