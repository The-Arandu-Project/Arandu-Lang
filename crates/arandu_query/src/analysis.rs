//! Analysis host / snapshot handles for IDE-facing work (A10 gold + F2).
//!
//! Stale-safety of analysis is **revision of snapshot**, not generational
//! `SymbolId`. `DocumentId` (see [`crate::doc_store`]) remains the generational
//! buffer handle; dense compiler IDs stay dense.

use crate::db::{DatabaseImpl, SourceFile};
use arandu_middle::SymbolId;
use salsa::Setter;
use std::sync::Arc;

/// Monotonic analysis generation advanced on every input commit (`set_text` /
/// register). IDE handles that embed a revision must match the live host.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AnalysisRevision(u64);

impl AnalysisRevision {
    #[must_use]
    pub const fn new(raw: u64) -> Self {
        Self(raw)
    }

    #[must_use]
    pub const fn as_u64(self) -> u64 {
        self.0
    }

    #[must_use]
    pub const fn next(self) -> Self {
        Self(self.0.wrapping_add(1))
    }
}

/// Frozen view of the database for a worker: cheap `Clone` of Salsa storage +
/// the host revision at capture time.
#[derive(Clone)]
pub struct AnalysisSnapshot {
    pub revision: AnalysisRevision,
    pub db: DatabaseImpl,
}

impl AnalysisSnapshot {
    #[must_use]
    pub fn capture(db: &DatabaseImpl, revision: AnalysisRevision) -> Self {
        Self {
            revision,
            db: db.clone(),
        }
    }

    /// True if `other` is still the same analysis generation as this snapshot.
    #[must_use]
    pub fn is_current(&self, other: AnalysisRevision) -> bool {
        self.revision == other
    }
}

/// Symbol handle valid only for a specific [`AnalysisRevision`].
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct LspSymbolId {
    pub symbol: SymbolId,
    pub revision: AnalysisRevision,
}

impl LspSymbolId {
    #[must_use]
    pub fn new(symbol: SymbolId, revision: AnalysisRevision) -> Self {
        Self { symbol, revision }
    }

    /// Returns the dense symbol only if this handle matches `snap.revision`.
    #[must_use]
    pub fn resolve(self, snap: &AnalysisSnapshot) -> Option<SymbolId> {
        if self.revision == snap.revision {
            Some(self.symbol)
        } else {
            None
        }
    }
}

/// Single-threaded owner of the Salsa DB + analysis revision counter.
///
/// Writes go through this host so revision advances in lock-step with inputs.
/// Workers receive only [`AnalysisSnapshot`] (read-only clone).
///
/// **Deadlock rule:** never hold an [`AnalysisSnapshot`] (or any `DatabaseImpl`
/// clone) on the same thread that calls [`Self::set_text`] / other writes.
/// Salsa waits for all storage clones to drop before mutating; a live snapshot
/// on the writer thread blocks forever.
pub struct AnalysisHost {
    db: DatabaseImpl,
    revision: AnalysisRevision,
}

impl Default for AnalysisHost {
    fn default() -> Self {
        Self::new()
    }
}

impl AnalysisHost {
    #[must_use]
    pub fn new() -> Self {
        Self {
            db: DatabaseImpl::new(),
            revision: AnalysisRevision::new(0),
        }
    }

    #[must_use]
    pub fn with_db(db: DatabaseImpl) -> Self {
        Self {
            db,
            revision: AnalysisRevision::new(0),
        }
    }

    #[must_use]
    pub fn db(&self) -> &DatabaseImpl {
        &self.db
    }

    #[must_use]
    pub fn db_mut(&mut self) -> &mut DatabaseImpl {
        &mut self.db
    }

    #[must_use]
    pub fn revision(&self) -> AnalysisRevision {
        self.revision
    }

    /// O(1) snapshot for a worker (Salsa `Storage` clone shares memos).
    #[must_use]
    pub fn snapshot(&self) -> AnalysisSnapshot {
        AnalysisSnapshot::capture(&self.db, self.revision)
    }

    /// Commit new text for an input; advances revision.
    pub fn set_text(&mut self, source: SourceFile, text: impl Into<Arc<str>>) {
        source.set_text(&mut self.db).to(text.into());
        self.revision = self.revision.next();
    }

    /// Register a path without necessarily changing text revision policy:
    /// new files still bump revision so open/import order is observable.
    pub fn register_source_file(&mut self, path: String, file: SourceFile) {
        self.db.register_source_file(path, file);
        self.revision = self.revision.next();
    }

    /// Remove a source registration and advance the observable analysis revision.
    /// A later registration for the same path must receive a fresh `FileId`.
    pub fn unregister_source_file(&mut self, path: &str) {
        self.db.unregister_source_file(path);
        self.revision = self.revision.next();
    }

    /// Create + register a new file (CLI/test helper); bumps revision.
    pub fn new_file(&mut self, path: String, text: String) -> SourceFile {
        let file = self.db.new_file(path, text);
        self.revision = self.revision.next();
        file
    }

    /// Bump revision without writing (tests / synthetic cancel).
    pub fn bump_revision(&mut self) {
        self.revision = self.revision.next();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arandu_middle::SymbolId;
    use std::path::PathBuf;

    #[test]
    fn set_text_advances_revision_and_stales_handles() {
        let mut host = AnalysisHost::new();
        let file = SourceFile::new(
            host.db(),
            1,
            Arc::from("func main() {}"),
            Arc::new(PathBuf::from("a.aru")),
        );
        host.register_source_file("a.aru".into(), file);
        let r0 = host.revision();

        // Resolve against revision r0 without holding a Storage clone across the write
        // (Salsa blocks `set_text` until all clones drop — holding a snapshot would deadlock).
        {
            let snap0 = host.snapshot();
            assert_eq!(snap0.revision, r0);
            let sym = LspSymbolId::new(SymbolId::new(1, 0), r0);
            assert_eq!(sym.resolve(&snap0), Some(SymbolId::new(1, 0)));
        }

        let sym = LspSymbolId::new(SymbolId::new(1, 0), r0);
        host.set_text(file, Arc::from("func main() { let x = 1; }"));
        let snap1 = host.snapshot();
        assert_ne!(snap1.revision, r0);
        assert!(
            sym.resolve(&snap1).is_none(),
            "old revision must not resolve"
        );
        let sym1 = LspSymbolId::new(SymbolId::new(1, 0), snap1.revision);
        assert_eq!(sym1.resolve(&snap1), Some(SymbolId::new(1, 0)));
    }

    #[test]
    fn snapshot_clone_is_independent_handle() {
        let mut host = AnalysisHost::new();
        let _ = host.new_file("t.aru".into(), "func main() {}".into());
        {
            let a = host.snapshot();
            let b = host.snapshot();
            assert_eq!(a.revision, b.revision);
        }
        // After drop, writes must not deadlock.
        host.bump_revision();
        assert!(host.revision().as_u64() > 0);
    }
}
