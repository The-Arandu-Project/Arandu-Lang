//! Deterministic per-pass change metrics.
//!
//! Ordering follows first-seen pipeline order (no hashing), so identical
//! inputs always produce identical [`PassStats`] — a requirement inherited
//! from the workspace determinism invariants.

/// Aggregated change counters for one [`super::PassManager`] run.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PassStats {
    /// Number of full-pipeline iterations executed (O0 reports 0).
    pub iterations: usize,
    changes: Vec<(&'static str, u32)>,
}

impl PassStats {
    /// Records one more change attributed to `pass`.
    pub fn note_change(&mut self, pass: &'static str) {
        match self.changes.iter_mut().find(|(name, _)| *name == pass) {
            Some((_, count)) => *count += 1,
            None => self.changes.push((pass, 1)),
        }
    }

    /// Change counts per pass, in first-seen pipeline order.
    pub fn changes(&self) -> &[(&'static str, u32)] {
        &self.changes
    }

    /// Total number of individual pass applications that changed the IR.
    #[must_use]
    pub fn total_changes(&self) -> u64 {
        self.changes
            .iter()
            .map(|(_, count)| u64::from(*count))
            .sum()
    }

    /// Merges `other` into `self`, summing iterations and counters.
    pub fn merge(&mut self, other: PassStats) {
        self.iterations += other.iterations;
        for (name, count) in other.changes {
            match self.changes.iter_mut().find(|(n, _)| *n == name) {
                Some((_, c)) => *c += count,
                None => self.changes.push((name, count)),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn note_change_appends_in_first_seen_order() {
        let mut stats = PassStats::default();
        stats.note_change("b");
        stats.note_change("a");
        stats.note_change("b");
        assert_eq!(stats.changes(), &[("b", 2), ("a", 1)]);
        assert_eq!(stats.total_changes(), 3);
    }

    #[test]
    fn merge_sums_iterations_and_counters() {
        let mut total = PassStats {
            iterations: 2,
            ..Default::default()
        };
        total.note_change("p");

        let mut other = PassStats {
            iterations: 3,
            ..Default::default()
        };
        other.note_change("q");
        other.note_change("p");

        total.merge(other);
        assert_eq!(total.iterations, 5);
        assert_eq!(total.changes(), &[("p", 2), ("q", 1)]);
    }
}
