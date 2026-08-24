//! Server state: AnalysisHost + DocumentStore + VFS + URI maps.

#[cfg(test)]
use crate::uri_util::uri_from_path;
use crate::uri_util::{parse_uri, path_from_uri};
use crate::vfs::Vfs;
use arandu_middle::resolved::NodeKey;
use arandu_query::db::SourceFile;
use arandu_query::{
    scan_aru_entries, AnalysisHost, AnalysisRevision, AnalysisSnapshot, DirectoryListing,
    DocumentId, DocumentStore, ManifestData,
};
use arandu_semantics::TypeCheckResult;
use lsp_types::Uri;
use rustc_hash::FxHashMap;
use rustc_hash::FxHashSet;
use std::path::PathBuf;
use std::sync::Arc;

pub struct PackageState {
    pub package_src: PathBuf,
    pub package_name: String,
    pub listing: DirectoryListing,
    entries: Vec<String>,
}

/// Cloneable view of a registered document handed to worker jobs.
#[derive(Clone)]
pub struct DocInfo {
    pub source: SourceFile,
    pub path: Arc<PathBuf>,
}

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
    /// Import registry keys owned by each compiler file identity. Filesystem
    /// events may arrive through a path spelling that cannot be reconstructed
    /// after rename (Windows verbatim paths, junctions), so removal uses this
    /// recorded ownership rather than guessing aliases from the stale path.
    package_aliases: FxHashMap<u32, Vec<String>>,
    /// Active package metadata. It is installed after the initialize handshake
    /// and its directory listing is the watched Salsa input for local imports.
    pub package: Option<PackageState>,
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
            package_aliases: FxHashMap::default(),
            package: None,
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

    /// URI → document view for all registered documents.
    pub(crate) fn doc_info_map(&self) -> FxHashMap<String, DocInfo> {
        let mut map = FxHashMap::default();
        for (uri, &id) in &self.by_uri {
            if let Some(doc) = self.docs.get(id) {
                map.insert(
                    uri.clone(),
                    DocInfo {
                        source: doc.source,
                        path: Arc::clone(&doc.path),
                    },
                );
            }
        }
        map
    }

    /// DocumentId → document view for all registered documents.
    pub(crate) fn doc_infos_by_id(&self) -> FxHashMap<DocumentId, DocInfo> {
        let mut by_id = FxHashMap::default();
        for &id in self.by_uri.values() {
            if let Some(doc) = self.docs.get(id) {
                by_id.insert(
                    id,
                    DocInfo {
                        source: doc.source,
                        path: Arc::clone(&doc.path),
                    },
                );
            }
        }
        by_id
    }

    fn path_of(uri: &Uri) -> PathBuf {
        path_from_uri(uri)
    }

    pub fn configure_package(
        &mut self,
        manifest_path: PathBuf,
        manifest_data: ManifestData,
        manifest_hash: String,
        package_src: PathBuf,
        entries: Vec<String>,
        stdlib_root: Option<PathBuf>,
    ) {
        let package_name = manifest_data.name.clone();
        let (_, listing, _) = self.host.configure_package(
            manifest_path,
            manifest_data,
            manifest_hash,
            package_src.clone(),
            entries.clone(),
            stdlib_root,
        );
        self.package = Some(PackageState {
            package_src,
            package_name,
            listing,
            entries,
        });

        // Files may have been opened before background project discovery
        // finished. Attach their import aliases without replacing overlays.
        let known: Vec<_> = self
            .by_uri
            .values()
            .filter_map(|&id| self.docs.get(id))
            .map(|doc| (doc.path.as_ref().clone(), doc.source))
            .collect();
        for (path, source) in known {
            self.register_package_aliases(&path, source);
        }
    }

    /// Rescan package structure outside Salsa queries and commit one listing
    /// input only when create/delete/rename changed its semantic contents.
    pub fn refresh_package_listing(&mut self) -> bool {
        let Some(package) = self.package.as_ref() else {
            return false;
        };
        let entries = scan_aru_entries(&package.package_src);
        if entries == package.entries {
            return false;
        }
        let package_src = package.package_src.clone();
        let package_name = package.package_name.clone();
        let listing = package.listing;
        self.host.set_directory_entries(listing, entries.clone());
        if let Some(package) = self.package.as_mut() {
            package.entries.clone_from(&entries);
        }

        // Register exact import keys from the authoritative relative listing.
        // This avoids deriving package identity from platform-specific URI
        // spellings and keeps goto attached to the already-known document.
        for rel in &entries {
            let absolute = package_src.join(rel);
            let normalized = normalize_path_soft(&absolute);
            let source = self
                .by_uri
                .values()
                .filter_map(|&id| self.docs.get(id))
                .find(|doc| normalize_path_soft(doc.path.as_ref()) == normalized)
                .map(|doc| doc.source);
            if let Some(source) = source {
                self.register_import_key(format!("{package_name}/{rel}"), source);
                self.register_import_key(rel.clone(), source);
            }
        }
        true
    }

    fn package_keys_for_path(&self, path: &std::path::Path) -> Vec<String> {
        let Some(package) = self.package.as_ref() else {
            return Vec::new();
        };
        let Some(rel) = package_relative_path(path, &package.package_src) else {
            return Vec::new();
        };
        vec![format!("{}/{}", package.package_name, rel), rel]
    }

    fn register_package_aliases(&mut self, path: &std::path::Path, source: SourceFile) {
        for key in self.package_keys_for_path(path) {
            self.register_import_key(key, source);
        }
    }

    fn register_import_key(&mut self, key: String, source: SourceFile) {
        let source_id = *source.file_id(self.host.db());
        let already_current = self
            .host
            .db()
            .source_file_by_path(&key)
            .is_some_and(|known| *known.file_id(self.host.db()) == source_id);
        if !already_current {
            if self.host.db().is_registered(&key) {
                self.host.unregister_source_file(&key);
            }
            self.host.register_source_file(key.clone(), source);
        }
        for owned in self.package_aliases.values_mut() {
            owned.retain(|candidate| candidate != &key);
        }
        let owned = self.package_aliases.entry(source_id).or_default();
        if !owned.contains(&key) {
            owned.push(key);
        }
    }

    fn unregister_package_aliases(&mut self, path: &std::path::Path, source_id: Option<u32>) {
        let mut keys = self.package_keys_for_path(path);
        if let Some(source_id) = source_id {
            keys.extend(self.package_aliases.remove(&source_id).unwrap_or_default());
        }
        keys.sort();
        keys.dedup();
        for key in keys {
            if self.host.db().is_registered(&key) {
                self.host.unregister_source_file(&key);
            }
        }
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
                self.register_package_aliases(&path, source);
                self.by_file_id.insert(fid, id);
                return id;
            }
            self.by_uri.remove(&uri_s);
        }
        // DatabaseImpl is the sole FileId allocator. A second LSP-side counter
        // can collide with lazily loaded stdlib/package files and corrupt goto.
        let source = self.host.new_file(registry_path_key(&path), text);
        let file_id = *source.file_id(self.host.db());
        self.register_package_aliases(&path, source);
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
        let id = self.by_uri.get(uri_s).copied();
        let source_id = id
            .and_then(|id| self.docs.get(id))
            .map(|doc| *doc.source.file_id(self.host.db()));
        self.unregister_package_aliases(&path, source_id);
        if self.host.db().is_registered(&path_key) {
            self.host.unregister_source_file(&path_key);
        }
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

fn normalize_path_soft(path: &std::path::Path) -> PathBuf {
    let resolved = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    PathBuf::from(registry_path_key(&resolved))
}

fn package_relative_path(path: &std::path::Path, package_src: &std::path::Path) -> Option<String> {
    // Rename notifications arrive after the old path has ceased to exist.
    // Derive its import key lexically before trying filesystem normalization,
    // which may otherwise retain a Windows verbatim prefix on one side only.
    let lexical_path = PathBuf::from(registry_path_key(path));
    let lexical_src = PathBuf::from(registry_path_key(package_src));
    let relative = lexical_path
        .strip_prefix(&lexical_src)
        .ok()
        .map(std::path::Path::to_path_buf)
        .or_else(|| {
            let normalized_path = normalize_path_soft(path);
            let normalized_src = normalize_path_soft(package_src);
            normalized_path
                .strip_prefix(normalized_src)
                .ok()
                .map(std::path::Path::to_path_buf)
        })?;
    Some(relative.to_string_lossy().replace('\\', "/"))
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

    #[cfg(windows)]
    #[test]
    fn missing_verbatim_path_keeps_package_relative_identity() {
        let src = std::path::Path::new(r"\\?\D:\a\Arandu-Lang\src");
        let removed = std::path::Path::new(r"D:\a\Arandu-Lang\src\util.aru");
        assert_eq!(
            package_relative_path(removed, src).as_deref(),
            Some("util.aru")
        );
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
    fn package_create_registers_import_alias_and_invalidates_missing_import() {
        let root = std::env::temp_dir().join(format!(
            "arandu-lsp-package-state-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock after epoch")
                .as_nanos()
        ));
        let src = root.join("src");
        std::fs::create_dir_all(&src).expect("create fixture");
        let discovered_src = std::fs::canonicalize(&src).expect("canonical source root");
        let main_path = src.join("main.aru");
        let main_text = concat!(
            "module editor_gold\n",
            "import editor_gold.util as util\n",
            "import std.path as path\n",
            "func main(): int {\n",
            "    if path.is_empty(\"\") { return util.answer() }\n",
            "    return 0\n",
            "}\n",
        );
        std::fs::write(&main_path, main_text).expect("write entry");
        let mut state = ServerState::new();
        state.configure_package(
            root.join("Arandu.toml"),
            ManifestData {
                name: "editor_gold".into(),
                version: "0.1.0".into(),
                entry: "src/main.aru".into(),
            },
            "fixture".into(),
            discovered_src,
            vec!["main.aru".into()],
            arandu_query::resolve_stdlib_root(arandu_query::StdlibResolveOpts::default())
                .ok()
                .map(|stdlib| stdlib.path),
        );
        let main_uri = uri_from_path(&main_path).expect("main URI");
        let _discovered_id = state.open_or_commit(&main_uri, main_text.into());
        let main_id = state.open_or_commit(&main_uri, main_text.into());
        state.mark_open(&main_uri);
        let main = state.docs.get(main_id).expect("main document").source;
        let _ = arandu_query::passes::module_signatures(state.host.db(), main);
        assert!(arandu_query::passes::type_check(state.host.db(), main)
            .diagnostics
            .iter()
            .any(|diag| matches!(diag.code, arandu_middle::DiagCode::M001UnresolvedImport)));
        let stdlib_file = state
            .host
            .db()
            .source_file_by_path("stdlib/std/path.aru")
            .expect("stdlib module loaded by initial typecheck");
        let stdlib_file_id = *stdlib_file.file_id(state.host.db());

        let util_path = src.join("util.aru");
        std::fs::write(
            &util_path,
            "/// Package answer.\npublic func answer(): int { return 42 }\n",
        )
        .expect("write module");
        let util_uri = uri_from_path(&util_path).expect("util URI");
        state
            .reload_uri_from_disk(&util_uri)
            .expect("register module");
        let util_id = state
            .by_uri
            .get(util_uri.as_str())
            .and_then(|&id| state.docs.get(id))
            .map(|doc| *doc.source.file_id(state.host.db()))
            .expect("created module id");
        assert_ne!(stdlib_file_id, util_id, "FileId allocator must be global");
        assert_eq!(
            state
                .host
                .db()
                .source_file_by_id(stdlib_file_id)
                .map(|file| *file.file_id(state.host.db())),
            Some(stdlib_file_id),
            "creating a workspace file must not replace stdlib reverse identity"
        );
        assert!(state.host.db().is_registered("editor_gold/util.aru"));
        assert!(state.refresh_package_listing());
        assert!(
            arandu_middle::db::SourceDatabase::resolve_module_path(
                state.host.db(),
                "editor_gold/util.aru"
            )
            .is_some(),
            "import registry must resolve the created module"
        );
        let resolved = arandu_query::passes::resolve(state.host.db(), main);
        assert!(
            resolved
                .diagnostics
                .iter()
                .all(|diag| !matches!(diag.code, arandu_middle::DiagCode::M001UnresolvedImport)),
            "stale resolve diagnostics: {:?}",
            resolved.diagnostics
        );
        let signatures = arandu_query::passes::module_signatures(state.host.db(), main);
        assert!(
            signatures
                .diagnostics
                .iter()
                .all(|diag| !matches!(diag.code, arandu_middle::DiagCode::M001UnresolvedImport)),
            "stale signature diagnostics: {:?}",
            signatures.diagnostics
        );
        let file_view = arandu_query::passes::file_typeck_view(state.host.db(), main);
        assert!(
            file_view
                .diagnostics
                .iter()
                .all(|diag| !matches!(diag.code, arandu_middle::DiagCode::M001UnresolvedImport)),
            "stale file view diagnostics: {:?}",
            file_view.diagnostics
        );
        let diagnostics = &arandu_query::passes::type_check(state.host.db(), main).diagnostics;
        assert!(diagnostics.is_empty(), "stale diagnostics: {diagnostics:?}");
        let tc = arandu_query::passes::type_check(state.host.db(), main);
        assert!(
            tc.symbols
                .module_members
                .get("util")
                .is_some_and(|members| members.contains_key("answer")),
            "created module members must be available to completion"
        );
        let call = main_text.find("util.answer").expect("util call");
        let items = crate::ide::completions(
            &state.snapshot(),
            main,
            main_text,
            crate::conv::offset_to_position(
                &arandu_base::LineIndex::new(main_text),
                u32::try_from(call + "util.".len()).expect("fixture offset"),
            ),
        );
        assert!(
            items.iter().any(|item| item.label == "answer"),
            "completion must include created module member: {items:?}"
        );
        let answer_offset = u32::try_from(call + "util.".len() + 2).expect("answer offset");
        let program = arandu_query::passes::parse(state.host.db(), main);
        let symbol = crate::ide::expr_symbol_at(
            program.as_ref().as_ref().expect("parsed entry"),
            tc,
            answer_offset,
        )
        .expect("symbol at imported member");
        let definition = arandu_query::passes::symbol_span(state.host.db(), symbol);
        assert_ne!(
            definition.file_id,
            *main.file_id(state.host.db()),
            "imported member must retain its definition file identity"
        );

        let helper_path = src.join("helper.aru");
        std::fs::rename(&util_path, &helper_path).expect("rename module fixture");
        let helper_uri = uri_from_path(&helper_path).expect("helper URI");
        state
            .rename_uri(&util_uri, &helper_uri)
            .expect("apply module rename");
        assert!(state.refresh_package_listing());
        assert!(!state.host.db().is_registered("editor_gold/util.aru"));
        assert!(!state.host.db().is_registered("util.aru"));
        assert!(
            arandu_query::passes::type_check(state.host.db(), main)
                .diagnostics
                .iter()
                .any(|diag| matches!(diag.code, arandu_middle::DiagCode::M001UnresolvedImport)),
            "renaming an imported module must invalidate its importers"
        );

        std::fs::remove_dir_all(root).expect("remove fixture");
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
