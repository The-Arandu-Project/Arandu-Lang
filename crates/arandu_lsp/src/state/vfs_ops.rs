//! VFS change queues, debounced flush coordination, and symbol offset indexing.

use arandu_middle::resolved::NodeKey;
use arandu_middle::SymbolId;
use arandu_query::DocumentId;
use arandu_semantics::TypeCheckResult;
use lsp_types::Uri;

use super::types::ServerState;
use crate::uri_util::parse_uri;

impl ServerState {
    pub(super) fn discard_pending(&mut self, uri: &Uri) {
        // Drop pending edits for this URI; re-queue the rest.
        let uri_s = uri.as_str();
        let remaining: Vec<(String, String)> = self
            .vfs
            .take_all()
            .into_iter()
            .filter(|(u, _)| u != uri_s)
            .collect();
        for (u, text) in remaining {
            self.vfs.push_full_text(u, text);
        }
    }

    /// Queue a change; does **not** bump Salsa revision until flush.
    pub fn queue_change(&mut self, uri: &Uri, text: String) {
        self.vfs.push_full_text(uri.as_str().to_string(), text);
    }

    pub fn set_version(&mut self, id: DocumentId, version: i32) {
        if self.docs.get(id).is_some() {
            self.versions.insert(id, version);
        }
    }

    #[must_use]
    pub fn version(&self, id: DocumentId) -> Option<i32> {
        self.versions.get(&id).copied()
    }

    #[must_use]
    pub fn text_for_change(&self, uri: &Uri) -> String {
        if let Some(text) = self.vfs.pending_text(uri.as_str()) {
            return text.to_string();
        }
        self.by_uri
            .get(uri.as_str())
            .and_then(|&id| self.docs.get(id))
            .map(|doc| doc.source.text(self.host.db()).to_string())
            .unwrap_or_default()
    }

    /// Commit due VFS edits; returns (uri, DocumentId) pairs that were committed.
    pub fn flush_due(&mut self) -> Vec<(Uri, DocumentId)> {
        let due = self.vfs.take_due();
        self.commit_edits(due)
    }

    /// Commit all pending (didSave / flush).
    pub fn flush_all(&mut self) -> Vec<(Uri, DocumentId)> {
        let all = self.vfs.take_all();
        self.commit_edits(all)
    }

    pub(super) fn commit_edits(&mut self, edits: Vec<(String, String)>) -> Vec<(Uri, DocumentId)> {
        let mut out = Vec::with_capacity(edits.len());
        for (uri_s, text) in edits {
            let Some(uri) = parse_uri(&uri_s) else {
                continue;
            };
            let id = self.open_or_commit(&uri, text);
            out.push((uri, id));
        }
        out
    }

    /// Tightest name/ref node containing `offset`.
    pub fn symbol_at(tc: &TypeCheckResult, offset: u32) -> Option<SymbolId> {
        let mut best: Option<(u32, SymbolId)> = None;
        let consider = |map: &rustc_hash::FxHashMap<NodeKey, SymbolId>,
                        best: &mut Option<(u32, SymbolId)>| {
            for (key, &sym) in map {
                if key.start <= offset && offset < key.end {
                    let w = key.end.saturating_sub(key.start);
                    if best.is_none_or(|(bw, _)| w < bw) {
                        *best = Some((w, sym));
                    }
                }
            }
        };
        consider(&tc.resolved.value_refs, &mut best);
        consider(&tc.resolved.type_refs, &mut best);
        consider(&tc.resolved.definitions, &mut best);
        best.map(|(_, s)| s)
    }
}
