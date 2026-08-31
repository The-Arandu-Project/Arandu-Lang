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

use std::collections::{BTreeSet, VecDeque};

use crate::BitSet;
use crate::amir::reachability::terminator_targets;
use crate::amir::{
    AmirFunc, AmirOperand, AmirRvalue, AmirStmt, AmirTerminator, BlockId, LocalId, TempId,
};
use crate::liveness::{LocalLiveness, TempLiveness, analyze_local_liveness, analyze_temp_liveness};
use crate::types::{BorrowPath, BorrowPathSegment};

/// Shared (`ref`) vs exclusive (`mut ref`) loan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LoanKind {
    Shared,
    Exclusive,
}

/// Stable structural location of a borrow inside a carrier value.
///
/// This is deliberately independent from source types: AMIR transfers can
/// preserve it without consulting the interner, which keeps borrow facts pure
/// and usable by Salsa. The empty path denotes the complete value.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct HolderPath(pub Vec<HolderProjection>);

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum HolderProjection {
    Slot(u32),
    NamedField { file_id: u32, local_id: u32 },
    Element,
    Deref,
    Variant(u32),
    Payload(u32),
    OptionSome,
    ResultOk,
    ResultErr,
    NullableValue,
    CoroutinePayload,
    PollReady,
    RangeElement,
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
    /// Paths held by each SSA temp. Empty sets mean that temp is not a holder.
    pub holder_temp_paths: Vec<BTreeSet<HolderPath>>,
    /// Paths held by each stack local.
    pub holder_local_paths: Vec<BTreeSet<HolderPath>>,
    /// Relative coroutine borrows survive state relocation; absolute borrows do not.
    pub relative: bool,
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
    /// Exact structural local holders before each statement and terminator.
    /// Indexed `[block][statement_point][loan]`.
    pub local_holders_at: Vec<Vec<Vec<BitSet<LocalId>>>>,
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
            let loan_index = self
                .loans
                .iter()
                .position(|candidate| std::ptr::eq(candidate, loan));
            let structurally_present = loan_index.is_some_and(|loan_index| {
                self.local_holders_at
                    .get(point.block.as_usize())
                    .and_then(|points| points.get(point.stmt_index))
                    .and_then(|loans| loans.get(loan_index))
                    .is_some_and(|holders| holders.contains(l))
            });
            if structurally_present && self.local_live_at(l, point) {
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
                        loans.push(new_loan(
                            LoanKind::Shared,
                            place.local,
                            *lhs,
                            block.id,
                            false,
                            num_temps,
                            num_locals,
                        ));
                    }
                    AmirRvalue::BorrowMut(place) => {
                        borrow_site_counts[bi] += 1;
                        loans.push(new_loan(
                            LoanKind::Exclusive,
                            place.local,
                            *lhs,
                            block.id,
                            false,
                            num_temps,
                            num_locals,
                        ));
                    }
                    // A3.4: same loan as absolute borrow of that local.
                    AmirRvalue::RelativeBorrow { local, mutable } => {
                        borrow_site_counts[bi] += 1;
                        loans.push(new_loan(
                            if *mutable {
                                LoanKind::Exclusive
                            } else {
                                LoanKind::Shared
                            },
                            *local,
                            *lhs,
                            block.id,
                            true,
                            num_temps,
                            num_locals,
                        ));
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
        if guard >= 100_000 {
            // The domain is finite and monotone. Reaching this defensive bound
            // means malformed AMIR, so retain the conservative facts collected
            // so far instead of panicking in production compiler code.
            break;
        }

        let block = &func.blocks[index];
        let mut changed = false;

        for stmt in func.block_stmts(block.id) {
            match stmt {
                AmirStmt::Assign { lhs, rhs } => {
                    for loan in &mut loans {
                        let produced = rvalue_holder_paths(rhs, loan);
                        changed |= merge_temp_paths(loan, *lhs, produced);
                    }
                }
                AmirStmt::Store { lhs, rhs } => {
                    if let Some(src) = operand_temp(rhs) {
                        let prefix = place_path(lhs);
                        for loan in &mut loans {
                            let source = loan
                                .holder_temp_paths
                                .get(src.as_usize())
                                .cloned()
                                .unwrap_or_default();
                            let produced = prefix_paths(&source, &prefix);
                            changed |= merge_local_paths(loan, lhs.local, produced);
                        }
                    }
                }
                AmirStmt::Call {
                    lhs: Some(lhs),
                    args,
                    return_borrow: Some(dependency),
                    ..
                } => {
                    for loan in &mut loans {
                        let mut output = BTreeSet::new();
                        for result in &dependency.dependencies {
                            for source in &result.sources {
                                let Ok(argument_index) = usize::try_from(source.parameter_index)
                                else {
                                    continue;
                                };
                                let Some(source_temp) =
                                    args.get(argument_index).and_then(operand_temp)
                                else {
                                    continue;
                                };
                                let input = loan
                                    .holder_temp_paths
                                    .get(source_temp.as_usize())
                                    .cloned()
                                    .unwrap_or_default();
                                let selected = strip_paths(
                                    &input,
                                    &holder_path_from_contract(&source.parameter_path),
                                );
                                output.extend(prefix_paths(
                                    &selected,
                                    &holder_path_from_contract(&result.result_path),
                                ));
                            }
                        }
                        changed |= merge_temp_paths(loan, *lhs, output);
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

fn new_loan(
    kind: LoanKind,
    place_local: LocalId,
    holder: TempId,
    origin_block: BlockId,
    relative: bool,
    num_temps: usize,
    num_locals: usize,
) -> Loan {
    let mut holder_temps = BitSet::with_capacity(num_temps);
    holder_temps.insert(holder);
    let mut holder_temp_paths = vec![BTreeSet::new(); num_temps];
    if let Some(paths) = holder_temp_paths.get_mut(holder.as_usize()) {
        paths.insert(HolderPath::default());
    }
    Loan {
        kind,
        place_local,
        holder_temps,
        holder_locals: BitSet::with_capacity(num_locals),
        holder_temp_paths,
        holder_local_paths: vec![BTreeSet::new(); num_locals],
        relative,
        origin_block,
    }
}

fn rvalue_holder_paths(rhs: &AmirRvalue, loan: &Loan) -> BTreeSet<HolderPath> {
    match rhs {
        AmirRvalue::Use(operand) | AmirRvalue::BlackBox { value: operand, .. } => {
            operand_holder_paths(*operand, loan)
        }
        AmirRvalue::Load(place) => {
            let input = loan
                .holder_local_paths
                .get(place.local.as_usize())
                .cloned()
                .unwrap_or_default();
            strip_paths(&input, &place_path(place))
        }
        AmirRvalue::Tuple { items } => aggregate_holder_paths(items, loan, HolderProjection::Slot),
        AmirRvalue::Array { items } => {
            aggregate_holder_paths(items, loan, |_| HolderProjection::Element)
        }
        AmirRvalue::StructLiteral { fields, .. } => {
            let operands = fields
                .iter()
                .map(|(_, operand)| *operand)
                .collect::<Vec<_>>();
            aggregate_holder_paths(&operands, loan, HolderProjection::Slot)
        }
        AmirRvalue::FieldAccess { base, field } => strip_paths(
            &operand_holder_paths(*base, loan),
            &HolderPath(vec![HolderProjection::Slot(
                u32::try_from(*field).unwrap_or(u32::MAX),
            )]),
        ),
        AmirRvalue::IndexAccess { base, .. } => strip_paths(
            &operand_holder_paths(*base, loan),
            &HolderPath(vec![HolderProjection::Element]),
        ),
        AmirRvalue::EnumConstruct {
            variant_tag,
            payload: Some(payload),
        } => {
            let prefix = HolderPath(vec![
                HolderProjection::Variant(u32::try_from(*variant_tag).unwrap_or(u32::MAX)),
                HolderProjection::Payload(0),
            ]);
            prefix_paths(&operand_holder_paths(*payload, loan), &prefix)
        }
        AmirRvalue::EnumPayload {
            value,
            variant: _,
            index,
        } => {
            let input = operand_holder_paths(*value, loan);
            // The tag is deliberately wildcarded here: AMIR identifies the
            // selected variant separately, while this local domain only needs
            // the payload slot to preserve the holder safely.
            strip_variant_payload(&input, u32::try_from(*index).unwrap_or(u32::MAX))
        }
        AmirRvalue::CoroutineReady { value, .. } => prefix_paths(
            &operand_holder_paths(*value, loan),
            &HolderPath(vec![HolderProjection::CoroutinePayload]),
        ),
        AmirRvalue::Borrow(_)
        | AmirRvalue::BorrowMut(_)
        | AmirRvalue::RelativeBorrow { .. }
        | AmirRvalue::Binary { .. }
        | AmirRvalue::Unary { .. }
        | AmirRvalue::Discriminant { .. }
        | AmirRvalue::Len(_)
        | AmirRvalue::Alloc(_)
        | AmirRvalue::EnumConstruct { payload: None, .. }
        | AmirRvalue::GenInsert { .. }
        | AmirRvalue::GenGet { .. }
        | AmirRvalue::GenSet { .. }
        | AmirRvalue::GenUpsert { .. }
        | AmirRvalue::GenRemove { .. }
        | AmirRvalue::StringInterp { .. }
        | AmirRvalue::ToStr { .. } => BTreeSet::new(),
    }
}

fn operand_holder_paths(operand: AmirOperand, loan: &Loan) -> BTreeSet<HolderPath> {
    operand_temp(&operand)
        .and_then(|temp| loan.holder_temp_paths.get(temp.as_usize()))
        .cloned()
        .unwrap_or_default()
}

fn aggregate_holder_paths(
    operands: &[AmirOperand],
    loan: &Loan,
    segment: impl Fn(u32) -> HolderProjection,
) -> BTreeSet<HolderPath> {
    let mut output = BTreeSet::new();
    for (index, operand) in operands.iter().enumerate() {
        let Ok(index) = u32::try_from(index) else {
            break;
        };
        output.extend(prefix_paths(
            &operand_holder_paths(*operand, loan),
            &HolderPath(vec![segment(index)]),
        ));
    }
    output
}

fn merge_temp_paths(loan: &mut Loan, temp: TempId, paths: BTreeSet<HolderPath>) -> bool {
    let Some(target) = loan.holder_temp_paths.get_mut(temp.as_usize()) else {
        return false;
    };
    let old_len = target.len();
    target.extend(paths);
    let changed = target.len() != old_len;
    if !target.is_empty() {
        loan.holder_temps.insert(temp);
    }
    changed
}

fn merge_local_paths(loan: &mut Loan, local: LocalId, paths: BTreeSet<HolderPath>) -> bool {
    let Some(target) = loan.holder_local_paths.get_mut(local.as_usize()) else {
        return false;
    };
    let old_len = target.len();
    target.extend(paths);
    let changed = target.len() != old_len;
    if !target.is_empty() {
        loan.holder_locals.insert(local);
    }
    changed
}

fn prefix_paths(input: &BTreeSet<HolderPath>, prefix: &HolderPath) -> BTreeSet<HolderPath> {
    input
        .iter()
        .map(|path| {
            let mut projections = Vec::with_capacity(prefix.0.len() + path.0.len());
            projections.extend(prefix.0.iter().cloned());
            projections.extend(path.0.iter().cloned());
            HolderPath(projections)
        })
        .collect()
}

fn strip_paths(input: &BTreeSet<HolderPath>, prefix: &HolderPath) -> BTreeSet<HolderPath> {
    input
        .iter()
        .filter(|path| path.0.starts_with(&prefix.0))
        .map(|path| HolderPath(path.0[prefix.0.len()..].to_vec()))
        .collect()
}

fn strip_variant_payload(input: &BTreeSet<HolderPath>, index: u32) -> BTreeSet<HolderPath> {
    input
        .iter()
        .filter_map(|path| match path.0.as_slice() {
            [
                HolderProjection::Variant(_),
                HolderProjection::Payload(found),
                rest @ ..,
            ] if *found == index => Some(HolderPath(rest.to_vec())),
            _ => None,
        })
        .collect()
}

fn place_path(place: &crate::amir::AmirPlace) -> HolderPath {
    HolderPath(
        place
            .projections
            .iter()
            .map(|projection| match projection {
                crate::amir::AmirProjection::Field(symbol) => HolderProjection::NamedField {
                    file_id: symbol.file_id,
                    local_id: symbol.local_id.0,
                },
                crate::amir::AmirProjection::Index(_) => HolderProjection::Element,
                crate::amir::AmirProjection::Deref => HolderProjection::Deref,
            })
            .collect(),
    )
}

fn holder_path_from_contract(path: &BorrowPath) -> HolderPath {
    HolderPath(
        path.0
            .iter()
            .map(|segment| match segment {
                BorrowPathSegment::Tuple(index) => HolderProjection::Slot(*index),
                BorrowPathSegment::Payload(index) => HolderProjection::Payload(*index),
                BorrowPathSegment::Field(name) => {
                    // Exported contracts use names so they survive recompilation.
                    // Local AMIR field accesses use slots; a stable digest keeps
                    // named paths distinct without introducing a hash map order.
                    let mut digest = 2_166_136_261_u32;
                    for byte in name.as_bytes() {
                        digest ^= u32::from(*byte);
                        digest = digest.wrapping_mul(16_777_619);
                    }
                    HolderProjection::NamedField {
                        file_id: u32::MAX,
                        local_id: digest,
                    }
                }
                BorrowPathSegment::Variant(tag) => HolderProjection::Variant(*tag),
                BorrowPathSegment::OptionSome => HolderProjection::OptionSome,
                BorrowPathSegment::ResultOk => HolderProjection::ResultOk,
                BorrowPathSegment::ResultErr => HolderProjection::ResultErr,
                BorrowPathSegment::ArrayElement => HolderProjection::Element,
                BorrowPathSegment::NullableValue => HolderProjection::NullableValue,
                BorrowPathSegment::CoroutinePayload => HolderProjection::CoroutinePayload,
                BorrowPathSegment::PollReady => HolderProjection::PollReady,
                BorrowPathSegment::RangeElement => HolderProjection::RangeElement,
            })
            .collect(),
    )
}

type LocalHolderState = Vec<Vec<BTreeSet<HolderPath>>>;

/// Forward may-analysis for stack carriers. Unlike the global holder index,
/// this state models overwrites: assigning one projection kills only that
/// subtree, while joins union the paths arriving from each predecessor.
fn analyze_local_holder_states(func: &AmirFunc, loans: &[Loan]) -> Vec<Vec<Vec<BitSet<LocalId>>>> {
    let empty_state = || vec![vec![BTreeSet::new(); func.locals.len()]; loans.len()];
    let mut block_in = vec![empty_state(); func.blocks.len()];
    let mut block_out = vec![empty_state(); func.blocks.len()];
    let mut queue = (0..func.blocks.len())
        .map(BlockId::from_usize)
        .collect::<VecDeque<_>>();
    let mut queued = vec![true; func.blocks.len()];

    while let Some(block_id) = queue.pop_front() {
        let bi = block_id.as_usize();
        queued[bi] = false;
        let mut state = block_in[bi].clone();
        for stmt in func.block_stmts(block_id) {
            transfer_local_holders(stmt, loans, &mut state);
        }
        if state == block_out[bi] {
            continue;
        }
        block_out[bi] = state.clone();
        let block = &func.blocks[bi];
        for (target, args) in terminator_edges(&block.terminator) {
            let mut edge = state.clone();
            if let Some(successor) = func.blocks.get(target.as_usize()) {
                for (parameter, argument) in successor.params.iter().zip(args) {
                    let source = operand_temp(argument);
                    for (loan_index, loan) in loans.iter().enumerate() {
                        let paths = source
                            .and_then(|temp| loan.holder_temp_paths.get(temp.as_usize()))
                            .cloned()
                            .unwrap_or_default();
                        edge[loan_index][parameter.local.as_usize()] = paths;
                    }
                }
            }
            if merge_local_state(&mut block_in[target.as_usize()], &edge)
                && !queued[target.as_usize()]
            {
                queue.push_back(target);
                queued[target.as_usize()] = true;
            }
        }
    }

    let mut points = Vec::with_capacity(func.blocks.len());
    for block in &func.blocks {
        let mut state = block_in[block.id.as_usize()].clone();
        let mut block_points = Vec::new();
        block_points.push(holder_bits(&state, func.locals.len()));
        for stmt in func.block_stmts(block.id) {
            transfer_local_holders(stmt, loans, &mut state);
            block_points.push(holder_bits(&state, func.locals.len()));
        }
        points.push(block_points);
    }
    points
}

fn transfer_local_holders(stmt: &AmirStmt, loans: &[Loan], state: &mut LocalHolderState) {
    match stmt {
        AmirStmt::Store { lhs, rhs } => {
            let destination = place_path(lhs);
            let source = operand_temp(rhs);
            for (loan_index, loan) in loans.iter().enumerate() {
                let local = &mut state[loan_index][lhs.local.as_usize()];
                local.retain(|path| !path.0.starts_with(&destination.0));
                if let Some(source) = source {
                    let paths = loan
                        .holder_temp_paths
                        .get(source.as_usize())
                        .cloned()
                        .unwrap_or_default();
                    local.extend(prefix_paths(&paths, &destination));
                }
            }
        }
        AmirStmt::StorageDead(local) => {
            for loan_state in state.iter_mut() {
                loan_state[local.as_usize()].clear();
            }
        }
        AmirStmt::Assign { .. }
        | AmirStmt::Call { .. }
        | AmirStmt::Free(_)
        | AmirStmt::StorageLive(_)
        | AmirStmt::Destroy(_)
        | AmirStmt::Nop => {}
    }
}

fn merge_local_state(target: &mut LocalHolderState, source: &LocalHolderState) -> bool {
    let mut changed = false;
    for (target_loan, source_loan) in target.iter_mut().zip(source) {
        for (target_local, source_local) in target_loan.iter_mut().zip(source_loan) {
            let old_len = target_local.len();
            target_local.extend(source_local.iter().cloned());
            changed |= target_local.len() != old_len;
        }
    }
    changed
}

fn holder_bits(state: &LocalHolderState, num_locals: usize) -> Vec<BitSet<LocalId>> {
    state
        .iter()
        .map(|loan| {
            let mut bits = BitSet::with_capacity(num_locals);
            for (index, paths) in loan.iter().enumerate() {
                if !paths.is_empty() {
                    bits.insert(LocalId::from_usize(index));
                }
            }
            bits
        })
        .collect()
}

fn terminator_edges(terminator: &AmirTerminator) -> Vec<(BlockId, &[AmirOperand])> {
    match terminator {
        AmirTerminator::Goto { target, args } => vec![(*target, args)],
        AmirTerminator::Suspend { resume, args, .. } => vec![(*resume, args)],
        AmirTerminator::Branch {
            if_true,
            true_args,
            if_false,
            false_args,
            ..
        } => vec![(*if_true, true_args), (*if_false, false_args)],
        AmirTerminator::SwitchInt {
            targets, otherwise, ..
        } => {
            let mut edges = targets
                .iter()
                .map(|(_, target, args)| (*target, args.as_slice()))
                .collect::<Vec<_>>();
            edges.push((otherwise.0, otherwise.1.as_slice()));
            edges
        }
        AmirTerminator::Return | AmirTerminator::Unreachable => Vec::new(),
    }
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
            let paths = loan
                .holder_temp_paths
                .get(src.as_usize())
                .cloned()
                .unwrap_or_default();
            *changed |= merge_temp_paths(loan, param.id, paths.clone());
            // Block params often alias a local.
            *changed |= merge_local_paths(loan, param.local, paths);
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
            local_holders_at: vec![],
        };
    }

    let (loans, borrow_site_counts) = collect_loans(func);
    let temp_live = analyze_temp_liveness(func);
    let local_live = analyze_local_liveness(func);
    let local_holders_at = analyze_local_holder_states(func, &loans);

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
        local_holders_at,
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
