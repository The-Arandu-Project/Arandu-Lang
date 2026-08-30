//! Filesystem watch buffer + package session (CLI `arandu watch` / future LSP FS).
//!
//! Raw OS events are coalesced via [`crate::debounce::DebouncedMap`]. A single
//! `WatchBuffer::commit` applies all due changes as **one** Salsa revision:
//! listing update, registry unregister/register, text `set_text`, optional
//! manifest re-parse. No intermediate "module not found" flash on rename.

use crate::db::{DatabaseImpl, SourceFile};
use crate::debounce::{DebouncedMap, DEFAULT_DEBOUNCE};
use crate::manifest::{
    hash_manifest_bytes, parse_manifest_bytes, register_manifest, ManifestError, ProjectManifest,
    LEGACY_MANIFEST_FILENAME, MANIFEST_FILENAME,
};
use crate::vfs::{scan_aru_entries, DirectoryListing, ModuleRoots};
use salsa::Setter;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

/// Logical filesystem change (already correlated when sourced from debouncer-full).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FsChange {
    /// Content of an existing file may have changed.
    Modify,
    /// File appeared (or was renamed onto this path).
    Create,
    /// File disappeared (or was renamed away).
    Remove,
    /// Atomic rename: treat as remove `from` + create `to` in **one** commit.
    Rename { to: PathBuf },
}

/// Debounced FS event buffer shared by CLI watch (and optional LSP file-watchers).
#[derive(Debug)]
pub struct WatchBuffer {
    /// Keyed by the path the event is about (`from` for rename).
    pending: DebouncedMap<PathBuf, FsChange>,
}

impl Default for WatchBuffer {
    fn default() -> Self {
        Self::new()
    }
}

impl WatchBuffer {
    #[must_use]
    pub fn new() -> Self {
        Self {
            pending: DebouncedMap::new(),
        }
    }

    #[must_use]
    pub fn with_debounce(debounce: Duration) -> Self {
        Self {
            pending: DebouncedMap::with_debounce(debounce),
        }
    }

    #[must_use]
    pub fn debounce(&self) -> Duration {
        self.pending.debounce()
    }

    /// Queue a logical change. Later events for the same path replace earlier ones.
    pub fn push(&mut self, path: PathBuf, change: FsChange) {
        self.pending.push(path, change);
    }

    /// Queue a correlated rename as a single pending entry (no Remove→Create gap).
    pub fn push_rename(&mut self, from: PathBuf, to: PathBuf) {
        let from = canonicalize_soft(&from);
        let to = canonicalize_soft(&to);
        let mut kept = Vec::new();
        for (p, c) in self.pending.take_all() {
            if p != from && p != to {
                kept.push((p, c));
            }
        }
        for (p, c) in kept {
            self.pending.push(p, c);
        }
        self.pending.push(from, FsChange::Rename { to });
    }

    #[must_use]
    pub fn has_pending(&self) -> bool {
        self.pending.has_pending()
    }

    #[must_use]
    pub fn next_deadline(&self) -> Option<Duration> {
        self.pending.next_deadline()
    }

    pub fn take_due(&mut self) -> Vec<(PathBuf, FsChange)> {
        self.pending.take_due()
    }

    pub fn take_all(&mut self) -> Vec<(PathBuf, FsChange)> {
        self.pending.take_all()
    }

    #[must_use]
    pub fn pending_count(&self) -> usize {
        self.pending.pending_count()
    }
}

/// Inputs to seed a [`PackageWatchSession`] (keeps `new` under clippy arg limit).
pub struct PackageWatchConfig {
    pub package_root: PathBuf,
    pub package_src: PathBuf,
    pub package_name: String,
    pub entry_rel: String,
    pub entry_abs: PathBuf,
    pub manifest_path: PathBuf,
    pub listing: DirectoryListing,
    pub module_roots: ModuleRoots,
    pub manifest: ProjectManifest,
}

/// Session that applies watch commits to a package-backed [`DatabaseImpl`].
pub struct PackageWatchSession {
    pub package_root: PathBuf,
    pub package_src: PathBuf,
    pub package_name: String,
    pub entry_rel: String,
    pub entry_abs: PathBuf,
    pub manifest_path: PathBuf,
    pub listing: DirectoryListing,
    pub module_roots: ModuleRoots,
    pub manifest: ProjectManifest,
    /// Absolute path → registry keys that refer to that file.
    path_keys: HashMap<PathBuf, Vec<String>>,
    buffer: WatchBuffer,
}

/// Result of a watch commit (for tests / DX status).
#[derive(Debug, Default, Clone)]
pub struct WatchCommitSummary {
    pub modified: usize,
    pub created: usize,
    pub removed: usize,
    pub renamed: usize,
    pub manifest_reloaded: bool,
    /// Import keys unregistered (deleted modules).
    pub unregistered_keys: Vec<String>,
}

impl PackageWatchSession {
    /// Seed a session after `load_project`-style setup.
    pub fn new(db: &mut DatabaseImpl, cfg: PackageWatchConfig) -> Self {
        let mut sess = Self {
            package_root: cfg.package_root,
            package_src: cfg.package_src,
            package_name: cfg.package_name,
            entry_rel: cfg.entry_rel,
            entry_abs: cfg.entry_abs,
            manifest_path: cfg.manifest_path,
            listing: cfg.listing,
            module_roots: cfg.module_roots,
            manifest: cfg.manifest,
            path_keys: HashMap::new(),
            buffer: WatchBuffer::with_debounce(DEFAULT_DEBOUNCE),
        };
        sess.reindex_keys(db);
        sess
    }

    #[must_use]
    pub fn buffer_mut(&mut self) -> &mut WatchBuffer {
        &mut self.buffer
    }

    /// Map relative package path (`util.aru`) to absolute.
    #[must_use]
    pub fn abs_in_src(&self, rel: &str) -> PathBuf {
        self.package_src.join(rel)
    }

    /// Import keys for a relative package file.
    #[must_use]
    pub fn keys_for_rel(&self, rel: &str) -> Vec<String> {
        let rel = rel.replace('\\', "/");
        vec![format!("{}/{}", self.package_name, rel), rel.clone()]
    }

    fn reindex_keys(&mut self, db: &DatabaseImpl) {
        self.path_keys.clear();
        let entries = self.listing.entries(db).clone();
        for rel in entries.iter() {
            let abs = self.abs_in_src(rel);
            let keys = self.keys_for_rel(rel);
            self.path_keys.insert(canonicalize_soft(&abs), keys);
        }
    }

    /// Refresh DirectoryListing from disk and push to Salsa (one setter).
    pub fn rescan_listing(&mut self, db: &mut DatabaseImpl) {
        let entries = scan_aru_entries(&self.package_src);
        self.listing.set_entries(db).to(Arc::new(entries));
        self.reindex_keys(db);
    }

    pub fn push(&mut self, path: PathBuf, change: FsChange) {
        self.buffer.push(canonicalize_soft(&path), change);
    }

    pub fn push_rename(&mut self, from: PathBuf, to: PathBuf) {
        self.buffer.push_rename(from, to);
    }

    /// Commit due events (or all if `force`). Returns summary.
    pub fn commit(&mut self, db: &mut DatabaseImpl, force: bool) -> WatchCommitSummary {
        let batch = if force {
            self.buffer.take_all()
        } else {
            self.buffer.take_due()
        };
        if batch.is_empty() {
            return WatchCommitSummary::default();
        }
        self.apply_batch(db, batch)
    }

    fn apply_batch(
        &mut self,
        db: &mut DatabaseImpl,
        batch: Vec<(PathBuf, FsChange)>,
    ) -> WatchCommitSummary {
        let mut summary = WatchCommitSummary::default();
        let mut listing_dirty = false;
        let mut manifest_touch = false;

        // Expand renames into ordered remove+create without yielding mid-batch.
        let mut ops: Vec<(PathBuf, FsOp)> = Vec::new();
        for (path, change) in batch {
            if path == self.manifest_path
                || matches!(
                    path.file_name().and_then(|s| s.to_str()),
                    Some(MANIFEST_FILENAME | LEGACY_MANIFEST_FILENAME)
                )
            {
                manifest_touch = true;
                continue;
            }
            match change {
                FsChange::Modify => ops.push((path, FsOp::Modify)),
                FsChange::Create => ops.push((path, FsOp::Create)),
                FsChange::Remove => ops.push((path, FsOp::Remove)),
                FsChange::Rename { to } => {
                    ops.push((path, FsOp::Remove));
                    ops.push((to, FsOp::Create));
                    summary.renamed += 1;
                }
            }
        }

        for (path, op) in ops {
            match op {
                FsOp::Modify => {
                    if let Ok(text) = std::fs::read_to_string(&path) {
                        self.apply_modify(db, &path, text);
                        summary.modified += 1;
                    }
                }
                FsOp::Create => {
                    if path.extension().and_then(|e| e.to_str()) == Some("aru") {
                        listing_dirty = true;
                        if let Ok(text) = std::fs::read_to_string(&path) {
                            self.apply_create(db, &path, text);
                            summary.created += 1;
                        }
                    }
                }
                FsOp::Remove => {
                    if path.extension().and_then(|e| e.to_str()) == Some("aru")
                        || self.path_keys.contains_key(&path)
                    {
                        listing_dirty = true;
                        let keys = self.apply_remove(db, &path);
                        summary.unregistered_keys.extend(keys);
                        summary.removed += 1;
                    }
                }
            }
        }

        if listing_dirty {
            // One listing setter for the whole batch (not per-file).
            let entries = scan_aru_entries(&self.package_src);
            self.listing.set_entries(db).to(Arc::new(entries));
            self.reindex_keys(db);
        }

        if manifest_touch {
            if let Ok(()) = self.reload_manifest(db) {
                summary.manifest_reloaded = true;
            }
        }

        summary
    }

    fn apply_modify(&mut self, db: &mut DatabaseImpl, path: &Path, text: String) {
        let path = canonicalize_soft(path);
        let keys = self
            .path_keys
            .get(&path)
            .cloned()
            .unwrap_or_else(|| self.infer_keys(&path));
        for key in &keys {
            if let Some(file) = db.source_file_by_path(key) {
                file.set_text(db).to(Arc::from(text.as_str()));
            } else {
                db.new_file(key.clone(), text.clone());
            }
        }
        // Entry may be registered under its absolute path string.
        if path == canonicalize_soft(&self.entry_abs) {
            let key = self.entry_abs.to_string_lossy().into_owned();
            if let Some(file) = db.source_file_by_path(&key) {
                file.set_text(db).to(Arc::from(text.as_str()));
            }
        }
    }

    fn apply_create(&mut self, db: &mut DatabaseImpl, path: &Path, text: String) {
        let keys = self.infer_keys(path);
        for key in &keys {
            db.new_file(key.clone(), text.clone());
        }
        self.path_keys.insert(canonicalize_soft(path), keys);
    }

    fn apply_remove(&mut self, db: &mut DatabaseImpl, path: &Path) -> Vec<String> {
        let canonical = canonicalize_soft(path);
        let keys = self
            .path_keys
            .remove(&canonical)
            .or_else(|| {
                // A deleted/renamed path cannot be canonicalized anymore. Match
                // the stable lexical identity used when the file was indexed.
                let wanted = stable_path_identity(path);
                let matched = self
                    .path_keys
                    .keys()
                    .find(|candidate| stable_path_identity(candidate) == wanted)
                    .cloned();
                matched.and_then(|candidate| self.path_keys.remove(&candidate))
            })
            .unwrap_or_else(|| self.infer_keys(path));
        for key in &keys {
            db.unregister_source_file(key);
        }
        keys
    }

    fn infer_keys(&self, path: &Path) -> Vec<String> {
        // Event paths are canonicalized before they enter the buffer. Canonicalize
        // the root as well so Windows `\\?\` paths and ordinary absolute paths do
        // not fall through to an unusable absolute-only registry key.
        let package_src = canonicalize_soft(&self.package_src);
        if let Ok(rel) = path.strip_prefix(&package_src) {
            let rel = rel.to_string_lossy().replace('\\', "/");
            return self.keys_for_rel(&rel);
        }
        vec![path.to_string_lossy().into_owned()]
    }

    /// Re-read Arandu.toml; if `name` changes, rebuild ModuleRoots (full local invalidation).
    pub fn reload_manifest(&mut self, db: &mut DatabaseImpl) -> Result<(), ManifestError> {
        let bytes = std::fs::read(&self.manifest_path).map_err(|e| ManifestError::Io {
            path: self.manifest_path.clone(),
            message: e.to_string(),
        })?;
        let data = parse_manifest_bytes(&self.manifest_path, &bytes)?;
        let hash = hash_manifest_bytes(&bytes);
        let old_name = self.package_name.clone();
        self.package_name = data.name.clone();
        self.entry_rel = data.entry.clone();
        self.entry_abs = self.package_root.join(&data.entry);
        self.package_src = self
            .entry_abs
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| self.package_root.clone());

        // Unregister old package-prefixed keys when name changes.
        if old_name != self.package_name {
            let old_prefix = format!("{old_name}/");
            // Collect keys to drop from current path_keys.
            let all_keys: Vec<String> = self
                .path_keys
                .values()
                .flatten()
                .filter(|k| k.starts_with(&old_prefix))
                .cloned()
                .collect();
            for k in all_keys {
                db.unregister_source_file(&k);
            }
        }

        let entries = scan_aru_entries(&self.package_src);
        self.listing =
            DirectoryListing::new(db, Arc::new(self.package_src.clone()), Arc::new(entries));
        let stdlib = self.module_roots.stdlib_root(db);
        self.module_roots = ModuleRoots::new(
            db,
            self.package_name.clone(),
            Arc::new(self.package_src.clone()),
            stdlib.clone(),
            self.listing,
        );
        db.set_module_roots(self.module_roots);
        self.manifest = register_manifest(db, self.manifest_path.clone(), data, hash);
        db.set_project_manifest(self.manifest);
        let _ = crate::manifest::manifest_fingerprint(db, self.manifest);
        self.reindex_keys(db);

        // Re-register package files under new keys.
        let mut registrations: Vec<_> = self.path_keys.clone().into_iter().collect();
        registrations.sort_by(|(left, _), (right, _)| left.cmp(right));
        for (abs, keys) in registrations {
            if let Ok(text) = std::fs::read_to_string(&abs) {
                for key in keys {
                    db.new_file(key, text.clone());
                }
            }
        }
        Ok(())
    }

    /// Type-check the package entry; returns diagnostics (including M001 for deleted imports).
    pub fn check_entry(
        &self,
        db: &DatabaseImpl,
        entry: SourceFile,
    ) -> Vec<arandu_middle::Diagnostic> {
        let tc = crate::passes::type_check(db, entry);
        tc.diagnostics.clone()
    }
}

#[derive(Clone, Copy)]
enum FsOp {
    Modify,
    Create,
    Remove,
}

fn canonicalize_soft(p: &Path) -> PathBuf {
    let path = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    normalize_windows_verbatim(path)
}

fn stable_path_identity(path: &Path) -> String {
    let value = path.to_string_lossy().replace('\\', "/");
    value
        .strip_prefix("//?/")
        .unwrap_or(&value)
        .to_ascii_lowercase()
}

#[cfg(windows)]
fn normalize_windows_verbatim(path: PathBuf) -> PathBuf {
    let value = path.to_string_lossy();
    if let Some(rest) = value.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{rest}"));
    }
    value
        .strip_prefix(r"\\?\")
        .map_or(path.clone(), PathBuf::from)
}

#[cfg(not(windows))]
fn normalize_windows_verbatim(path: PathBuf) -> PathBuf {
    path
}

/// Ensure path is absolute for stable map keys.
#[must_use]
pub fn abs_path(p: &Path) -> PathBuf {
    if p.is_absolute() {
        canonicalize_soft(p)
    } else {
        canonicalize_soft(&std::env::current_dir().unwrap_or_default().join(p))
    }
}
