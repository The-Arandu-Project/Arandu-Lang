//! F2.1 + F2.2 — May-borrow facts refined by reference live ranges.
//!
//! ## F2.1
//! Tracks which stack locals **may** be under an active loan at block
//! boundaries (`shared` / `exclusive` dense bitsets, A9).
//!
//! ## F2.2 (gold)
//! A loan opened by `t = ref x` / `t = mut ref x` stays active **exactly while**
//! some holder of that reference is live:
//! - the SSA temp produced by `Borrow`/`BorrowMut`
//! - locals / temps that copy or load that reference
//!
//! So the borrow window **is** the live range of the reference value — the
//! same liveness the backend needs for register allocation
//! ([`crate::liveness::analyze_temp_liveness`] + local liveness). No second
//! “lifetime” engine.
//!
//! Escape via return/heap/closure (statically unbounded window) is F2.3.
//! Diagnostics O002/O003/O006 are M2 and call [`is_borrowed_at`].

use std::collections::VecDeque;

use crate::BitSet;
use crate::amir::reachability::terminator_targets;
use crate::amir::{
    AmirFunc, AmirOperand, AmirRvalue, AmirStmt, AmirTerminator, BlockId, LocalId, TempId,
};
use crate::liveness::{LocalLiveness, TempLiveness, analyze_local_liveness, analyze_temp_liveness};

/// Shared (`ref`) vs exclusive (`mut ref`) loan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LoanKind {
    Shared,
    Exclusive,
}

/// One loan opened by `Borrow` / `BorrowMut` (plus propagated holders).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Loan {
    pub kind: LoanKind,
    /// Root local of the borrowed place (`x` in `ref x` / `ref x.f`).
    pub place_local: LocalId,
    /// SSA temps that currently hold this reference value.
    pub holder_temps: BitSet<TempId>,
    /// Stack locals that currently hold this reference value (`let p = &x`).
    pub holder_locals: BitSet<LocalId>,
    pub origin_block: BlockId,
}

/// Program point inside a function (block + statement index).
///
/// `stmt_index == 0` is block entry (before the first statement).
/// `stmt_index == n` (after last stmt) is just before the terminator / block exit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ProgramPoint {
    pub block: BlockId,
    pub stmt_index: usize,
}

/// May-borrowed state for all locals at one program point.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BorrowState {
    pub shared: BitSet<LocalId>,
    pub exclusive: BitSet<LocalId>,
}

impl BorrowState {
    #[must_use]
    pub fn new(num_locals: usize) -> Self {
        Self {
            shared: BitSet::with_capacity(num_locals),
            exclusive: BitSet::with_capacity(num_locals),
        }
    }

    #[must_use]
    pub fn maybe_shared(&self, local: LocalId) -> bool {
        self.shared.contains(local)
    }

    #[must_use]
    pub fn maybe_exclusive(&self, local: LocalId) -> bool {
        self.exclusive.contains(local)
    }

    #[must_use]
    pub fn maybe_borrowed(&self, local: LocalId) -> bool {
        self.maybe_shared(local) || self.maybe_exclusive(local)
    }

    fn activate(&mut self, loan: &Loan) {
        match loan.kind {
            LoanKind::Shared => {
                self.shared.insert(loan.place_local);
            }
            LoanKind::Exclusive => {
                self.exclusive.insert(loan.place_local);
            }
        }
    }
}

/// Full-function borrow facts (F2.1 summaries + F2.2 loans/liveness).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuncBorrowFacts {
    pub block_in: Vec<BorrowState>,
    pub block_out: Vec<BorrowState>,
    pub borrow_site_counts: Vec<u32>,
    /// All loans with propagated holders (for M2 / [`is_borrowed_at`]).
    pub loans: Vec<Loan>,
    pub temp_live: TempLiveness,
    pub local_live: LocalLiveness,
}

impl FuncBorrowFacts {
    #[must_use]
    pub fn maybe_shared_at_entry(&self, block: BlockId, local: LocalId) -> bool {
        self.block_in
            .get(block.as_usize())
            .is_some_and(|s| s.maybe_shared(local))
    }

    #[must_use]
    pub fn maybe_exclusive_at_entry(&self, block: BlockId, local: LocalId) -> bool {
        self.block_in
            .get(block.as_usize())
            .is_some_and(|s| s.maybe_exclusive(local))
    }

    #[must_use]
    pub fn maybe_borrowed_at_entry(&self, block: BlockId, local: LocalId) -> bool {
        self.block_in
            .get(block.as_usize())
            .is_some_and(|s| s.maybe_borrowed(local))
    }

    /// F2.2: is `local` under any loan whose reference holder is live at `point`?
    ///
    /// Statement-level precision walks the block from entry, tracking which
    /// temps/locals are still live (start from live-out, walk reverse once
    /// offline would be ideal; here we use entry/exit bits + “defined after
    /// point” approximation for temps defined in-block).
    #[must_use]
    pub fn is_borrowed_at(&self, local: LocalId, point: ProgramPoint) -> bool {
        self.is_borrowed_kind_at(local, point, None)
    }

    #[must_use]
    pub fn is_shared_borrowed_at(&self, local: LocalId, point: ProgramPoint) -> bool {
        self.is_borrowed_kind_at(local, point, Some(LoanKind::Shared))
    }

    #[must_use]
    pub fn is_exclusive_borrowed_at(&self, local: LocalId, point: ProgramPoint) -> bool {
        self.is_borrowed_kind_at(local, point, Some(LoanKind::Exclusive))
    }

    fn is_borrowed_kind_at(
        &self,
        local: LocalId,
        point: ProgramPoint,
        only: Option<LoanKind>,
    ) -> bool {
        let bi = point.block.as_usize();
        if bi >= self.block_in.len() {
            return false;
        }
        // Fast path: empty at both IN and OUT ⇒ no loan of this local in window.
        let in_b = &self.block_in[bi];
        let out_b = &self.block_out[bi];
        let relevant = |s: &BorrowState| match only {
            Some(LoanKind::Shared) => s.maybe_shared(local),
            Some(LoanKind::Exclusive) => s.maybe_exclusive(local),
            None => s.maybe_borrowed(local),
        };
        if !relevant(in_b) && !relevant(out_b) {
            // Loan may open and close entirely inside the block.
            // Fall through to loan walk.
        }

        for loan in &self.loans {
            if loan.place_local != local {
                continue;
            }
            if let Some(k) = only
                && loan.kind != k
            {
                continue;
            }
            if self.loan_active_at(loan, point) {
                return true;
            }
        }
        false
    }

    fn loan_active_at(&self, loan: &Loan, point: ProgramPoint) -> bool {
        // Holder temp live at point?
        for t in loan.holder_temps.iter() {
            if self.temp_live_at(t, point) {
                return true;
            }
        }
        for l in loan.holder_locals.iter() {
            if self.local_live_at(l, point) {
                return true;
            }
        }
        false
    }

    /// Holder temp live at `point`?
    /// Entry uses live-in; interior/exit uses live-in ∪ live-out (sound over-approx).
    fn temp_live_at(&self, temp: TempId, point: ProgramPoint) -> bool {
        if point.stmt_index == 0 {
            return self.temp_live.live_in(point.block).contains(temp);
        }
        self.temp_live.live_in(point.block).contains(temp)
            || self.temp_live.live_out(point.block).contains(temp)
    }

    fn local_live_at(&self, local: LocalId, point: ProgramPoint) -> bool {
        if point.stmt_index == 0 {
            return self.local_live.live_in(point.block).contains(local);
        }
        self.local_live.live_in(point.block).contains(local)
            || self.local_live.live_out(point.block).contains(local)
    }
}

/// Collect primary loans and propagate holders through copies/loads (fixpoint).
fn collect_loans(func: &AmirFunc) -> (Vec<Loan>, Vec<u32>) {
    let num_temps = func.temps.len();
    let num_locals = func.locals.len();
    let mut loans = Vec::new();
    let mut borrow_site_counts = vec![0u32; func.blocks.len()];

    for block in &func.blocks {
        let bi = block.id.as_usize();
        for stmt in func.block_stmts(block.id) {
            if let AmirStmt::Assign { lhs, rhs } = stmt {
                match rhs {
                    AmirRvalue::Borrow(place) => {
                        borrow_site_counts[bi] += 1;
                        let mut holder_temps = BitSet::with_capacity(num_temps);
                        holder_temps.insert(*lhs);
                        loans.push(Loan {
                            kind: LoanKind::Shared,
                            place_local: place.local,
                            holder_temps,
                            holder_locals: BitSet::with_capacity(num_locals),
                            origin_block: block.id,
                        });
                    }
                    AmirRvalue::BorrowMut(place) => {
                        borrow_site_counts[bi] += 1;
                        let mut holder_temps = BitSet::with_capacity(num_temps);
                        holder_temps.insert(*lhs);
                        loans.push(Loan {
                            kind: LoanKind::Exclusive,
                            place_local: place.local,
                            holder_temps,
                            holder_locals: BitSet::with_capacity(num_locals),
                            origin_block: block.id,
                        });
                    }
                    // A3.4: same loan as absolute borrow of that local.
                    AmirRvalue::RelativeBorrow { local, mutable } => {
                        borrow_site_counts[bi] += 1;
                        let mut holder_temps = BitSet::with_capacity(num_temps);
                        holder_temps.insert(*lhs);
                        loans.push(Loan {
                            kind: if *mutable {
                                LoanKind::Exclusive
                            } else {
                                LoanKind::Shared
                            },
                            place_local: *local,
                            holder_temps,
                            holder_locals: BitSet::with_capacity(num_locals),
                            origin_block: block.id,
                        });
                    }
                    _ => {}
                }
            }
        }
    }

    // Propagate holders: copies of the reference value alias the same loan.
    let mut worklist = VecDeque::new();
    let num_blocks = func.blocks.len();
    let mut in_worklist = vec![false; num_blocks];
    for block in &func.blocks {
        worklist.push_back(block.id);
        in_worklist[block.id.as_usize()] = true;
    }

    let mut guard = 0;
    while let Some(block_id) = worklist.pop_front() {
        let index = block_id.as_usize();
        in_worklist[index] = false;
        guard += 1;
        assert!(
            guard < 100_000,
            "loan holder propagation failed to converge"
        );

        let block = &func.blocks[index];
        let mut changed = false;

        for stmt in func.block_stmts(block.id) {
            match stmt {
                AmirStmt::Assign { lhs, rhs } => match rhs {
                    AmirRvalue::Use(op) => {
                        if let Some(src) = operand_temp(op) {
                            for loan in &mut loans {
                                if loan.holder_temps.contains(src) && loan.holder_temps.insert(*lhs)
                                {
                                    changed = true;
                                }
                            }
                        }
                    }
                    AmirRvalue::Load(place) if place.projections.is_empty() => {
                        for loan in &mut loans {
                            if loan.holder_locals.contains(place.local)
                                && loan.holder_temps.insert(*lhs)
                            {
                                changed = true;
                            }
                        }
                    }
                    _ => {}
                },
                AmirStmt::Store { lhs, rhs } if lhs.projections.is_empty() => {
                    if let Some(src) = operand_temp(rhs) {
                        for loan in &mut loans {
                            if loan.holder_temps.contains(src)
                                && loan.holder_locals.insert(lhs.local)
                            {
                                changed = true;
                            }
                        }
                    }
                }
                AmirStmt::Call {
                    lhs: Some(lhs),
                    args,
                    return_borrow: Some(dependency),
                    ..
                } => {
                    for source in dependency
                        .dependencies
                        .iter()
                        .flat_map(|dependency| &dependency.sources)
                    {
                        let Ok(argument_index) = usize::try_from(source.parameter_index) else {
                            continue;
                        };
                        let Some(source) = args.get(argument_index).and_then(operand_temp) else {
                            continue;
                        };
                        for loan in &mut loans {
                            if loan.holder_temps.contains(source) && loan.holder_temps.insert(*lhs)
                            {
                                changed = true;
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        // Terminator args → successor block params (phi-like).
        match &block.terminator {
            AmirTerminator::Goto { target, args } => {
                propagate_terminator_args(func, *target, args, &mut loans, &mut changed);
            }
            AmirTerminator::Suspend { resume, args, .. } => {
                propagate_terminator_args(func, *resume, args, &mut loans, &mut changed);
            }
            AmirTerminator::Branch {
                if_true,
                true_args,
                if_false,
                false_args,
                ..
            } => {
                propagate_terminator_args(func, *if_true, true_args, &mut loans, &mut changed);
                propagate_terminator_args(func, *if_false, false_args, &mut loans, &mut changed);
            }
            AmirTerminator::SwitchInt {
                targets, otherwise, ..
            } => {
                for (_, tgt, args) in targets {
                    propagate_terminator_args(func, *tgt, args, &mut loans, &mut changed);
                }
                propagate_terminator_args(
                    func,
                    otherwise.0,
                    &otherwise.1,
                    &mut loans,
                    &mut changed,
                );
            }
            AmirTerminator::Return | AmirTerminator::Unreachable => {}
        }

        if changed {
            for successor in terminator_targets(&block.terminator) {
                let succ_index = successor.as_usize();
                if !in_worklist[succ_index] {
                    worklist.push_back(successor);
                    in_worklist[succ_index] = true;
                }
            }
        }
    }

    (loans, borrow_site_counts)
}

fn propagate_terminator_args(
    func: &AmirFunc,
    target: BlockId,
    args: &[AmirOperand],
    loans: &mut [Loan],
    changed: &mut bool,
) {
    let Some(tb) = func.blocks.get(target.as_usize()) else {
        return;
    };
    for (i, arg) in args.iter().enumerate() {
        let Some(src) = operand_temp(arg) else {
            continue;
        };
        let Some(param) = tb.params.get(i) else {
            continue;
        };
        for loan in loans.iter_mut() {
            if loan.holder_temps.contains(src) && loan.holder_temps.insert(param.id) {
                *changed = true;
            }
            // Block params often alias a local.
            if loan.holder_temps.contains(src) && loan.holder_locals.insert(param.local) {
                *changed = true;
            }
        }
    }
}

fn operand_temp(op: &AmirOperand) -> Option<TempId> {
    match op {
        AmirOperand::Copy(t) | AmirOperand::Move(t) => Some(*t),
        _ => None,
    }
}

fn state_from_live_holders(
    loans: &[Loan],
    num_locals: usize,
    temp_live: &BitSet<TempId>,
    local_live: &BitSet<LocalId>,
) -> BorrowState {
    let mut st = BorrowState::new(num_locals);
    for loan in loans {
        let temp_active = loan.holder_temps.iter().any(|t| temp_live.contains(t));
        let local_active = loan.holder_locals.iter().any(|l| local_live.contains(l));
        if temp_active || local_active {
            st.activate(loan);
        }
    }
    st
}

/// F2.2-aware borrow facts: block IN/OUT = loans whose holders are live there.
#[must_use]
pub fn analyze_borrow_facts(func: &AmirFunc) -> FuncBorrowFacts {
    let num_locals = func.locals.len();
    let num_blocks = func.blocks.len();

    if num_blocks == 0 {
        return FuncBorrowFacts {
            block_in: vec![],
            block_out: vec![],
            borrow_site_counts: vec![],
            loans: vec![],
            temp_live: analyze_temp_liveness(func),
            local_live: analyze_local_liveness(func),
        };
    }

    let (loans, borrow_site_counts) = collect_loans(func);
    let temp_live = analyze_temp_liveness(func);
    let local_live = analyze_local_liveness(func);

    let mut block_in = Vec::with_capacity(num_blocks);
    let mut block_out = Vec::with_capacity(num_blocks);
    for bi in 0..num_blocks {
        let bid = BlockId::from_usize(bi);
        block_in.push(state_from_live_holders(
            &loans,
            num_locals,
            temp_live.live_in(bid),
            local_live.live_in(bid),
        ));
        block_out.push(state_from_live_holders(
            &loans,
            num_locals,
            temp_live.live_out(bid),
            local_live.live_out(bid),
        ));
    }

    FuncBorrowFacts {
        block_in,
        block_out,
        borrow_site_counts,
        loans,
        temp_live,
        local_live,
    }
}

/// Free function for M2 / Salsa consumers (same as [`FuncBorrowFacts::is_borrowed_at`]).
#[must_use]
pub fn is_borrowed_at(facts: &FuncBorrowFacts, local: LocalId, point: ProgramPoint) -> bool {
    facts.is_borrowed_at(local, point)
}

/// Shared-loan cardinality at each block entry (for Salsa / HashEq).
#[must_use]
pub fn shared_in_counts(func: &AmirFunc) -> Vec<u32> {
    analyze_borrow_facts(func)
        .block_in
        .iter()
        .map(|s| s.shared.len() as u32)
        .collect()
}

/// Exclusive-loan cardinality at each block entry.
#[must_use]
pub fn exclusive_in_counts(func: &AmirFunc) -> Vec<u32> {
    analyze_borrow_facts(func)
        .block_in
        .iter()
        .map(|s| s.exclusive.len() as u32)
        .collect()
}

/// Compact per-block borrow summary for memoization.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockBorrowSummary {
    pub shared_in: u32,
    pub exclusive_in: u32,
    pub borrow_sites: u32,
    /// Locals still may-borrowed at block **exit** (F2.2: after live-range kill).
    pub shared_out: u32,
    pub exclusive_out: u32,
}

/// Summaries for all blocks in one pure call.
#[must_use]
pub fn block_borrow_summaries(func: &AmirFunc) -> Vec<BlockBorrowSummary> {
    let facts = analyze_borrow_facts(func);
    facts
        .block_in
        .iter()
        .zip(facts.block_out.iter())
        .zip(facts.borrow_site_counts.iter())
        .map(|((inn, out), &sites)| BlockBorrowSummary {
            shared_in: inn.shared.len() as u32,
            exclusive_in: inn.exclusive.len() as u32,
            borrow_sites: sites,
            shared_out: out.shared.len() as u32,
            exclusive_out: out.exclusive.len() as u32,
        })
        .collect()
}

#[cfg(test)]
#[path = "borrow_facts_tests.rs"]
mod tests;
