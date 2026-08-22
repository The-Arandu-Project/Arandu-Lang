//! Server state: AnalysisHost + DocumentStore + VFS + URI maps.

#[cfg(test)]
use crate::uri_util::uri_from_path;
use crate::uri_util::{parse_uri, path_from_uri};
use crate::vfs::Vfs;
use arandu_middle::resolved::NodeKey;
use arandu_query::db::SourceFile;
use arandu_query::{AnalysisHost, AnalysisRevision, AnalysisSnapshot, DocumentId, DocumentStore};
use arandu_semantics::TypeCheckResult;
use lsp_types::Uri;
use rustc_hash::FxHashMap;
use rustc_hash::FxHashSet;
use std::path::PathBuf;
use std::sync::Arc;

pub struct ServerState {
    pub host: AnalysisHost,
    pub docs: DocumentStore,
    pub vfs: Vfs,
    pub by_uri: FxHashMap<String, DocumentId>,
    /// URIs currently owned by editor overlays. Known workspace files may be
    /// registered and queryable without being open.
    pub open_uris: FxHashSet<String>,
    /// Numeric compiler `file_id` → open document (multi-file workspace).
    pub by_file_id: FxHashMap<u32, DocumentId>,
    /// Latest client document version for each open buffer.
    pub versions: FxHashMap<DocumentId, i32>,
    /// Last published diagnostic fingerprint per document (skip no-op publish).
    pub last_diag_fp: FxHashMap<DocumentId, ([u8; 32], Option<i32>)>,
    /// P3: last per-item IDE diag fingerprints (DocumentId, item local key).
    pub last_item_diag_fp: FxHashMap<(DocumentId, u32, u32), [u8; 32]>,
    next_file_id: u32,
}

impl ServerState {
    #[must_use]
    pub fn new() -> Self {
        Self {
            host: AnalysisHost::new(),
            docs: DocumentStore::new(),
            vfs: Vfs::new(),
            by_uri: FxHashMap::default(),
            open_uris: FxHashSet::default(),
            by_file_id: FxHashMap::default(),
            versions: FxHashMap::default(),
            last_diag_fp: FxHashMap::default(),
            last_item_diag_fp: FxHashMap::default(),
            next_file_id: 10_000,
        }
    }

    #[must_use]
    pub fn snapshot(&self) -> AnalysisSnapshot {
        self.host.snapshot()
    }

    #[must_use]
    pub fn revision(&self) -> AnalysisRevision {
        self.host.revision()
    }

    fn path_of(uri: &Uri) -> PathBuf {
        path_from_uri(uri)
    }

    /// Open document or apply committed text (after VFS flush).
    pub fn open_or_commit(&mut self, uri: &Uri, text: String) -> DocumentId {
        let path = Self::path_of(uri);
        let uri_s = uri.as_str().to_string();
        if let Some(&id) = self.by_uri.get(&uri_s) {
            if let Some(doc) = self.docs.get_mut(id) {
                let source = doc.source;
                let fid = *source.file_id(self.host.db());
                self.host.set_text(source, Arc::from(text));
                let path_key = registry_path_key(&path);
                if !self.host.db().is_registered(&path_key) {
                    self.host.register_source_file(path_key, source);
                }
                self.by_file_id.insert(fid, id);
                return id;
            }
            self.by_uri.remove(&uri_s);
        }
        let file_id = self.next_file_id;
        self.next_file_id = self.next_file_id.wrapping_add(1);
        let source = SourceFile::new(
            self.host.db(),
            file_id,
            Arc::from(text),
            Arc::new(path.clone()),
        );
        self.host
            .register_source_file(registry_path_key(&path), source);
        let id = self.docs.open(path, source);
        self.by_uri.insert(uri_s, id);
        self.by_file_id.insert(file_id, id);
        id
    }

    pub fn mark_open(&mut self, uri: &Uri) {
        self.open_uris.insert(uri.as_str().to_string());
    }

    #[must_use]
    pub fn is_open(&self, uri: &Uri) -> bool {
        self.open_uris.contains(uri.as_str())
    }

    /// Close an editor overlay. Restore the authoritative disk contents when
    /// the workspace file still exists; otherwise remove its registration.
    pub fn close_uri(&mut self, uri: &Uri) {
        self.open_uris.remove(uri.as_str());
        self.discard_pending(uri);
        let path = Self::path_of(uri);
        if path.extension().and_then(|ext| ext.to_str()) == Some("aru") {
            if let Ok(text) = std::fs::read_to_string(&path) {
                self.replace_closed_overlay(uri, path, text);
                return;
            }
        }
        self.remove_uri(uri);
    }

    fn replace_closed_overlay(&mut self, uri: &Uri, path: PathBuf, text: String) {
        let uri_s = uri.as_str().to_string();
        let Some(old_id) = self.by_uri.get(&uri_s).copied() else {
            self.open_or_commit(uri, text);
            return;
        };
        let Some(old_doc) = self.docs.get(old_id).cloned() else {
            self.by_uri.remove(&uri_s);
            self.open_or_commit(uri, text);
            return;
        };
        let source = old_doc.source;
        let file_id = *source.file_id(self.host.db());
        self.host.set_text(source, Arc::from(text));
        self.docs.close(old_id);
        let disk_id = self.docs.open(path, source);
        self.by_uri.insert(uri_s, disk_id);
        self.by_file_id.insert(file_id, disk_id);
        self.versions.remove(&old_id);
        self.last_diag_fp.remove(&old_id);
        self.last_item_diag_fp
            .retain(|&(doc, _, _), _| doc != old_id);
    }

    /// Remove a workspace source. An open overlay remains locally usable but
    /// is unregistered so imports cannot resolve a file deleted on disk.
    pub fn remove_uri(&mut self, uri: &Uri) {
        let uri_s = uri.as_str();
        self.discard_pending(uri);
        let path = Self::path_of(uri);
        let path_key = registry_path_key(&path);
        if self.host.db().is_registered(&path_key) {
            self.host.unregister_source_file(&path_key);
        }
        let id = self.by_uri.get(uri_s).copied();
        if let Some(id) = id {
            self.versions.remove(&id);
            self.last_diag_fp.remove(&id);
            self.last_item_diag_fp.retain(|&(doc, _, _), _| doc != id);
        }
        if !self.open_uris.contains(uri_s) {
            let Some(id) = self.by_uri.remove(uri_s) else {
                return;
            };
            if let Some(doc) = self.docs.get(id) {
                let fid = doc.source.file_id(self.host.db());
                self.by_file_id.remove(fid);
            }
            self.docs.close(id);
        }
    }

    /// Reload a closed `.aru` file after a client filesystem notification.
    pub fn reload_uri_from_disk(&mut self, uri: &Uri) -> Option<DocumentId> {
        if self.is_open(uri) {
            return self.by_uri.get(uri.as_str()).copied();
        }
        let path = Self::path_of(uri);
        if path.extension().and_then(|ext| ext.to_str()) != Some("aru") {
            return None;
        }
        let text = std::fs::read_to_string(path).ok()?;
        Some(self.open_or_commit(uri, text))
    }

    /// Move a known source to a fresh path/identity after a filesystem rename.
    pub fn rename_uri(&mut self, old_uri: &Uri, new_uri: &Uri) -> Option<DocumentId> {
        let was_open = self.open_uris.remove(old_uri.as_str());
        let overlay_text = was_open.then(|| self.text_for_change(old_uri));
        self.remove_uri(old_uri);
        let text = overlay_text.or_else(|| {
            let path = Self::path_of(new_uri);
            std::fs::read_to_string(path).ok()
        })?;
        let id = self.open_or_commit(new_uri, text);
        if was_open {
            self.mark_open(new_uri);
        }
        Some(id)
    }

    fn discard_pending(&mut self, uri: &Uri) {
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

    fn commit_edits(&mut self, edits: Vec<(String, String)>) -> Vec<(Uri, DocumentId)> {
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
    pub fn symbol_at(tc: &TypeCheckResult, offset: u32) -> Option<arandu_middle::SymbolId> {
        let mut best: Option<(u32, arandu_middle::SymbolId)> = None;
        let consider = |map: &rustc_hash::FxHashMap<NodeKey, arandu_middle::SymbolId>,
                        best: &mut Option<(u32, arandu_middle::SymbolId)>| {
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

fn registry_path_key(path: &std::path::Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    #[cfg(windows)]
    let text = text.strip_prefix("//?/").unwrap_or(&text).to_string();
    text
}

impl Default for ServerState {
    fn default() -> Self {
        Self::new()
    }
}

/// Read a deterministic, bounded set of workspace sources outside the LSP
/// handshake. Registration remains on the main server thread.
#[must_use]
pub fn discover_aru_files(roots: &[PathBuf]) -> Vec<(PathBuf, String)> {
    const MAX_FILES: usize = 256;

    let mut stack = roots.to_vec();
    stack.sort();
    stack.reverse();
    let mut paths = std::collections::BTreeSet::new();

    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut entries: Vec<_> = rd.flatten().collect();
        entries.sort_by_key(std::fs::DirEntry::path);
        for ent in entries.into_iter().rev() {
            let p = ent.path();
            if p.is_dir() {
                if matches!(
                    p.file_name().and_then(|s| s.to_str()),
                    Some("target" | ".git" | "node_modules")
                ) {
                    continue;
                }
                stack.push(p);
            } else if p.extension().and_then(|s| s.to_str()) == Some("aru") {
                paths.insert(p);
                if paths.len() >= MAX_FILES {
                    break;
                }
            }
        }
        if paths.len() >= MAX_FILES {
            break;
        }
    }

    paths
        .into_iter()
        .filter_map(|path| {
            let text = std::fs::read_to_string(&path).ok()?;
            Some((path, text))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn file_url(name: &str) -> Uri {
        uri_from_path(std::path::Path::new(&format!("/tmp/{name}")))
            .or_else(|| parse_uri(&format!("file:///tmp/{name}")))
            .expect("uri")
    }

    #[test]
    fn workspace_discovery_is_sorted_bounded_and_skips_build_trees() {
        let root = std::env::temp_dir().join(format!(
            "arandu-lsp-discovery-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock after Unix epoch")
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).expect("create source fixture");
        std::fs::create_dir_all(root.join("target")).expect("create ignored fixture");
        std::fs::write(root.join("src/z.aru"), "func z() {}").expect("write z");
        std::fs::write(root.join("src/a.aru"), "func a() {}").expect("write a");
        std::fs::write(root.join("target/ignored.aru"), "func ignored() {}")
            .expect("write ignored");

        let files = discover_aru_files(std::slice::from_ref(&root));
        let names: Vec<_> = files
            .iter()
            .filter_map(|(path, _)| path.file_name().and_then(|name| name.to_str()))
            .collect();
        assert_eq!(names, vec!["a.aru", "z.aru"]);

        std::fs::remove_dir_all(root).expect("remove discovery fixture");
    }

    #[test]
    fn queue_change_does_not_bump_revision_until_flush() {
        let mut st = ServerState::new();
        // Instant flush for test.
        st.vfs = Vfs::with_debounce(Duration::from_millis(0));
        let uri = file_url("a.aru");
        let id = st.open_or_commit(&uri, "func main() {}".into());
        let r0 = st.revision();

        st.queue_change(&uri, "func main() { let x = 1; }".into());
        assert_eq!(
            st.revision(),
            r0,
            "pending VFS must not touch AnalysisRevision"
        );

        let committed = st.flush_all();
        assert_eq!(committed.len(), 1);
        assert_eq!(committed[0].1, id);
        assert_ne!(st.revision(), r0, "commit must advance revision");
    }

    #[test]
    fn n_changes_one_commit_one_revision_bump() {
        let mut st = ServerState::new();
        st.vfs = Vfs::with_debounce(Duration::from_millis(0));
        let uri = file_url("b.aru");
        st.open_or_commit(&uri, "func main() {}".into());
        let r0 = st.revision();

        st.queue_change(&uri, "v1".into());
        st.queue_change(&uri, "v2".into());
        st.queue_change(&uri, "v3".into());
        assert_eq!(st.vfs.pending_count(), 1);

        let committed = st.flush_all();
        assert_eq!(committed.len(), 1);
        // One flush of one file → one set_text → one bump from r0.
        assert_eq!(st.revision().as_u64(), r0.as_u64() + 1);
    }

    #[test]
    fn text_for_change_prefers_latest_pending_vfs_text() {
        let mut st = ServerState::new();
        let uri = file_url("rapid.aru");
        st.open_or_commit(&uri, "committed".into());

        st.queue_change(&uri, "pending-1".into());
        assert_eq!(st.text_for_change(&uri), "pending-1");

        st.queue_change(&uri, "pending-2".into());
        assert_eq!(st.text_for_change(&uri), "pending-2");
    }

    #[test]
    fn closed_document_id_is_stale() {
        let mut st = ServerState::new();
        let uri = file_url("c.aru");
        let id = st.open_or_commit(&uri, "func main() {}".into());
        assert!(st.docs.get(id).is_some());
        st.close_uri(&uri);
        assert!(st.docs.get(id).is_none());
        assert!(!st.by_uri.contains_key(uri.as_str()));
    }

    #[test]
    fn close_discards_pending_edit_without_reopening_document() {
        let mut st = ServerState::new();
        st.vfs = Vfs::with_debounce(Duration::from_millis(0));
        let uri = file_url("closed-pending.aru");
        let id = st.open_or_commit(&uri, "func main() {}".into());
        st.queue_change(&uri, "func main() { let stale = 1; }".into());

        st.close_uri(&uri);

        assert!(st.flush_all().is_empty());
        assert!(st.docs.get(id).is_none());
        assert!(!st.by_uri.contains_key(uri.as_str()));
    }

    #[test]
    fn close_stales_overlay_and_restores_disk_source() {
        let root = std::env::temp_dir().join(format!(
            "arandu-lsp-close-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock after Unix epoch")
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).expect("create close fixture");
        let path = root.join("close.aru");
        std::fs::write(&path, "func disk(): int { return 1 }").expect("write disk fixture");
        let uri = uri_from_path(&path).expect("file URI");
        let mut st = ServerState::new();
        let overlay = st.open_or_commit(&uri, "func overlay(): int { return 2 }".into());
        st.mark_open(&uri);
        {
            let snap = st.snapshot();
            let source = st.docs.get(overlay).expect("overlay document").source;
            assert!(
                crate::ide::workspace_symbols(
                    &snap,
                    &[crate::ide::DocSnap {
                        source,
                        path: Arc::new(path.clone()),
                        uri: uri.clone(),
                    }],
                    "overlay",
                )
                .iter()
                .any(|symbol| symbol.name == "overlay"),
                "workspace symbols must observe the open overlay"
            );
        }

        st.close_uri(&uri);

        assert!(
            st.docs.get(overlay).is_none(),
            "closed overlay ID must be stale"
        );
        let disk_id = st.by_uri[uri.as_str()];
        assert_ne!(disk_id, overlay);
        let disk = st.docs.get(disk_id).expect("known disk source");
        assert_eq!(
            disk.source.text(st.host.db()).as_ref(),
            "func disk(): int { return 1 }"
        );
        assert!(!st.is_open(&uri));
        std::fs::remove_dir_all(root).expect("remove close fixture");
    }

    #[test]
    fn delete_and_rename_never_reuse_file_identity() {
        let root = std::env::temp_dir().join(format!(
            "arandu-lsp-rename-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock after Unix epoch")
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).expect("create rename fixture");
        let old_path = root.join("old.aru");
        let new_path = root.join("new.aru");
        std::fs::write(&old_path, "func value(): int { return 1 }").expect("write old");
        let old_uri = uri_from_path(&old_path).expect("old URI");
        let new_uri = uri_from_path(&new_path).expect("new URI");
        let mut st = ServerState::new();
        let old_doc = st.reload_uri_from_disk(&old_uri).expect("load old");
        let old_file = *st
            .docs
            .get(old_doc)
            .expect("old document")
            .source
            .file_id(st.host.db());
        std::fs::rename(&old_path, &new_path).expect("rename fixture");

        let new_doc = st.rename_uri(&old_uri, &new_uri).expect("apply rename");
        let new_file = *st
            .docs
            .get(new_doc)
            .expect("new document")
            .source
            .file_id(st.host.db());
        assert!(st.docs.get(old_doc).is_none());
        assert!(new_file > old_file, "FileId allocation must be monotonic");
        assert!(!st.host.db().is_registered(&registry_path_key(&old_path)));
        assert!(st.host.db().is_registered(&registry_path_key(&new_path)));

        st.remove_uri(&new_uri);
        assert!(st.docs.get(new_doc).is_none());
        assert!(!st.host.db().is_registered(&registry_path_key(&new_path)));
        std::fs::remove_dir_all(root).expect("remove rename fixture");
    }

    #[test]
    fn s2_endurance_batches_edits_and_discards_stale_snapshots() {
        const BATCHES: u64 = 100;
        const CHANGES_PER_BATCH: u64 = 20;

        let mut st = ServerState::new();
        st.vfs = Vfs::with_debounce(Duration::from_millis(0));
        let uri = file_url("s2-endurance.aru");
        let id = st.open_or_commit(&uri, "func main(): int { return 0 }".into());
        let initial_revision = st.revision().as_u64();

        for batch in 1..=BATCHES {
            for change in 1..=CHANGES_PER_BATCH {
                let value = batch * CHANGES_PER_BATCH + change;
                st.queue_change(&uri, format!("func main(): int {{ return {value} }}"));
            }
            assert_eq!(
                st.vfs.pending_count(),
                1,
                "full-text changes for one document must coalesce"
            );

            let stale_revision = st.revision();
            let source = st.docs.get(id).expect("live document").source;
            let snapshot = st.snapshot();
            let worker = std::thread::spawn(move || {
                let diagnostics = arandu_query::file_ide_diagnostics(&snapshot.db, source);
                (snapshot.revision, diagnostics.len())
            });
            let (worker_revision, _) = worker.join().expect("snapshot worker must finish");
            assert_eq!(worker_revision, stale_revision);

            let committed = st.flush_all();
            assert_eq!(committed, vec![(uri.clone(), id)]);
            assert_ne!(st.revision(), stale_revision);
        }

        assert_eq!(
            st.revision().as_u64(),
            initial_revision + BATCHES,
            "2,000 on-type changes must become exactly 100 Salsa commits"
        );
        assert_eq!(st.docs.len(), 1, "edits must not leak document identities");
        assert_eq!(st.by_uri.len(), 1);
        assert_eq!(st.by_file_id.len(), 1);

        st.close_uri(&uri);
        assert!(st.docs.get(id).is_none(), "closed ID must remain stale");
        let reopened = st.open_or_commit(&uri, "func main(): int { return 7 }".into());
        assert_ne!(
            reopened, id,
            "reopen must allocate a new DocumentId generation"
        );
    }
}
