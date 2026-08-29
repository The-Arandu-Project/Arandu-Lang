use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use arandu_middle::{Severity, SymbolId};
use arandu_query::{
    hash_manifest_bytes, parse_manifest_bytes, register_manifest, scan_aru_entries,
    AnalysisRevision, AnalysisSnapshot, ArandCompilerDb, DatabaseImpl, DirectoryListing,
    DocumentStore, FsChange, LspSymbolId, ModuleRoots, PackageWatchConfig, PackageWatchSession,
    MANIFEST_FILENAME,
};

const OPERATIONS: usize = 10_000;
const CHECKPOINT_INTERVAL: usize = 250;

pub fn cmd_check_project_churn(workspace_root: &Path) -> i32 {
    match check_project_churn(workspace_root) {
        Ok(summary) => {
            println!(
                "check-project-churn: ok ({} operations, {} checkpoints, {} FileIds, {} stale documents)",
                summary.operations, summary.checkpoints, summary.file_ids, summary.stale_documents
            );
            0
        }
        Err(error) => {
            eprintln!("check-project-churn: error: {error}");
            1
        }
    }
}

struct ChurnSummary {
    operations: usize,
    checkpoints: usize,
    file_ids: usize,
    stale_documents: usize,
}

struct ProjectSession {
    root: PathBuf,
    db: DatabaseImpl,
    watch: PackageWatchSession,
    package: String,
    active: Option<PathBuf>,
    seen_ids: BTreeSet<u32>,
    max_id: u32,
}

fn check_project_churn(workspace_root: &Path) -> Result<ChurnSummary, String> {
    let root = unique_temp_root(workspace_root);
    let result = (|| {
        seed_project(&root, "churn_a")?;
        let mut project = ProjectSession::open(root.clone())?;
        let mut checkpoints = 0;
        for operation in 1..=OPERATIONS {
            project.apply(operation)?;
            if operation.is_multiple_of(CHECKPOINT_INTERVAL) {
                project.assert_clean_oracle(operation)?;
                project.assert_stale_revision(operation)?;
                checkpoints += 1;
            }
        }
        let stale_documents = check_document_generations(&project.db)?;
        Ok(ChurnSummary {
            operations: OPERATIONS,
            checkpoints,
            file_ids: project.seen_ids.len(),
            stale_documents,
        })
    })();
    let _ = fs::remove_dir_all(&root);
    result
}

impl ProjectSession {
    fn open(root: PathBuf) -> Result<Self, String> {
        let mut db = DatabaseImpl::new();
        let watch = open_watch_session(&mut db, &root)?;
        let package = watch.package_name.clone();
        let mut session = Self {
            root,
            db,
            watch,
            package,
            active: None,
            seen_ids: BTreeSet::new(),
            max_id: 0,
        };
        session.record_current_ids()?;
        Ok(session)
    }

    fn apply(&mut self, operation: usize) -> Result<(), String> {
        if operation % 500 == 250 {
            return self.enable_cycle(operation);
        }
        if operation.is_multiple_of(500) && !operation.is_multiple_of(1_000) {
            return self.disable_cycle(operation);
        }
        if operation.is_multiple_of(1_000) {
            return self.rename_package(operation);
        }
        let slot = self.root.join("src/scratch.aru");
        match operation % 5 {
            0 => {
                let util = self.root.join("src/util.aru");
                write_text(
                    &util,
                    &format!(
                        "public func answer(): int {{ return {} }}\n",
                        operation % 97
                    ),
                )?;
                self.watch.push(util, FsChange::Modify);
            }
            1 => {
                write_text(&slot, "public func scratch(): int { return 1 }\n")?;
                self.watch.push(slot.clone(), FsChange::Create);
                self.active = Some(slot);
            }
            2 => {
                let from = self.active.take().unwrap_or_else(|| slot.clone());
                let to = self.root.join("src/scratch_renamed.aru");
                if from.exists() {
                    fs::rename(&from, &to).map_err(|error| error.to_string())?;
                    self.watch.push_rename(from, to.clone());
                    self.active = Some(to);
                } else {
                    write_text(&to, "public func scratch(): int { return 2 }\n")?;
                    self.watch.push(to.clone(), FsChange::Create);
                    self.active = Some(to);
                }
            }
            3 => {
                if let Some(path) = self.active.take() {
                    if path.exists() {
                        fs::remove_file(&path).map_err(|error| error.to_string())?;
                    }
                    self.watch.push(path, FsChange::Remove);
                } else {
                    // Duplicate/out-of-order watcher notifications must be harmless.
                    self.watch.push(slot, FsChange::Remove);
                }
            }
            _ => {
                write_text(&slot, "public func scratch(): int { return 4 }\n")?;
                self.watch.push(slot.clone(), FsChange::Create);
                self.active = Some(slot);
            }
        }
        let _ = self.watch.commit(&mut self.db, true);
        self.record_current_ids()
    }

    fn rename_package(&mut self, operation: usize) -> Result<(), String> {
        let next = if self.package == "churn_a" {
            "churn_b"
        } else {
            "churn_a"
        };
        write_manifest(&self.root, next)?;
        write_main(&self.root, next, operation)?;
        self.remove_cycle_files()?;
        self.watch
            .push(self.root.join(MANIFEST_FILENAME), FsChange::Modify);
        self.watch
            .push(self.root.join("src/main.aru"), FsChange::Modify);
        let summary = self.watch.commit(&mut self.db, true);
        if !summary.manifest_reloaded {
            return Err(format!("operation {operation}: manifest was not reloaded"));
        }
        self.package = next.to_owned();
        self.record_current_ids()
    }

    fn enable_cycle(&mut self, operation: usize) -> Result<(), String> {
        let a = self.root.join("src/cycle_a.aru");
        let b = self.root.join("src/cycle_b.aru");
        write_text(
            &a,
            &format!(
                "module {}\nimport {}.cycle_b as cycle_b\npublic func a(): int {{ return cycle_b.b() }}\n",
                self.package, self.package
            ),
        )?;
        write_text(
            &b,
            &format!(
                "module {}\nimport {}.cycle_a as cycle_a\npublic func b(): int {{ return cycle_a.a() }}\n",
                self.package, self.package
            ),
        )?;
        write_cycle_main(&self.root, &self.package, operation)?;
        self.watch.push(a, FsChange::Create);
        self.watch.push(b, FsChange::Create);
        self.watch
            .push(self.root.join("src/main.aru"), FsChange::Modify);
        let _ = self.watch.commit(&mut self.db, true);
        self.record_current_ids()
    }

    fn disable_cycle(&mut self, operation: usize) -> Result<(), String> {
        write_main(&self.root, &self.package, operation)?;
        self.remove_cycle_files()?;
        self.watch
            .push(self.root.join("src/main.aru"), FsChange::Modify);
        let _ = self.watch.commit(&mut self.db, true);
        self.record_current_ids()
    }

    fn remove_cycle_files(&mut self) -> Result<(), String> {
        for name in ["cycle_a.aru", "cycle_b.aru"] {
            let path = self.root.join("src").join(name);
            if path.exists() {
                fs::remove_file(&path).map_err(|error| error.to_string())?;
            }
            self.watch.push(path, FsChange::Remove);
        }
        Ok(())
    }

    fn record_current_ids(&mut self) -> Result<(), String> {
        let mut newly_seen = Vec::new();
        for relative in scan_aru_entries(&self.root.join("src")) {
            let key = format!("{}/{}", self.package, relative.replace('\\', "/"));
            let file = self
                .db
                .source_file_by_path(&key)
                .ok_or_else(|| format!("current module `{key}` is not registered"))?;
            let id = *file.file_id(self.db.as_source_db());
            if !self.seen_ids.contains(&id) {
                newly_seen.push(id);
            }
        }
        if let Some(minimum) = newly_seen.iter().min().copied() {
            if minimum <= self.max_id {
                return Err(format!(
                    "FileId {minimum} was allocated after {}; IDs must be monotonic",
                    self.max_id
                ));
            }
            self.max_id = newly_seen.iter().max().copied().unwrap_or(self.max_id);
            self.seen_ids.extend(newly_seen);
        }
        Ok(())
    }

    fn assert_clean_oracle(&mut self, operation: usize) -> Result<(), String> {
        // Conservative recovery after a lost/overflowed watcher event.
        self.watch.rescan_listing(&mut self.db);
        let incremental = observe_entry(&self.db, &self.package)?;
        let mut clean = DatabaseImpl::new();
        let clean_watch = open_watch_session(&mut clean, &self.root)?;
        let rebuilt = observe_entry(&clean, &clean_watch.package_name)?;
        if incremental != rebuilt {
            return Err(format!(
                "operation {operation}: incremental differs from clean rebuild\nincremental={incremental:?}\nclean={rebuilt:?}"
            ));
        }
        Ok(())
    }

    fn assert_stale_revision(&self, operation: usize) -> Result<(), String> {
        let old = AnalysisRevision::new(operation as u64 - 1);
        let current = AnalysisRevision::new(operation as u64);
        let symbol = LspSymbolId::new(SymbolId::new(0, 0), old);
        let worker = {
            let snapshot = AnalysisSnapshot::capture(&self.db, old);
            std::thread::spawn(move || symbol.resolve(&snapshot))
        };
        if worker
            .join()
            .map_err(|_| format!("operation {operation}: snapshot worker panicked"))?
            .is_none()
        {
            return Err("same-revision symbol unexpectedly failed to resolve".into());
        }
        let current_snapshot = AnalysisSnapshot::capture(&self.db, current);
        if symbol.resolve(&current_snapshot).is_some() {
            return Err(format!("operation {operation}: stale LspSymbolId resolved"));
        }
        Ok(())
    }
}

fn open_watch_session(db: &mut DatabaseImpl, root: &Path) -> Result<PackageWatchSession, String> {
    let manifest_path = root.join(MANIFEST_FILENAME);
    let manifest_bytes = fs::read(&manifest_path).map_err(|error| error.to_string())?;
    let data =
        parse_manifest_bytes(&manifest_path, &manifest_bytes).map_err(|error| error.to_string())?;
    let hash = hash_manifest_bytes(&manifest_bytes);
    let entry_abs = root.join(&data.entry);
    let package_src = entry_abs
        .parent()
        .ok_or_else(|| "manifest entry has no parent".to_owned())?
        .to_path_buf();
    let entries = scan_aru_entries(&package_src);
    let listing = DirectoryListing::new(db, Arc::new(package_src.clone()), Arc::new(entries));
    let roots = ModuleRoots::new(
        db,
        data.name.clone(),
        Arc::new(package_src.clone()),
        None,
        listing,
    );
    db.set_module_roots(roots);
    let manifest = register_manifest(db, manifest_path.clone(), data.clone(), hash);
    db.set_project_manifest(manifest);
    for relative in scan_aru_entries(&package_src) {
        let text = fs::read_to_string(package_src.join(&relative)).map_err(|e| e.to_string())?;
        db.new_file(format!("{}/{}", data.name, relative), text.clone());
        db.new_file(relative, text);
    }
    Ok(PackageWatchSession::new(
        db,
        PackageWatchConfig {
            package_root: root.to_path_buf(),
            package_src,
            package_name: data.name,
            entry_rel: data.entry,
            entry_abs,
            manifest_path,
            listing,
            module_roots: roots,
            manifest,
        },
    ))
}

#[derive(Debug, PartialEq, Eq)]
struct Observation {
    diagnostics: Vec<String>,
    amir: Option<String>,
}

fn observe_entry(db: &DatabaseImpl, package: &str) -> Result<Observation, String> {
    let key = format!("{package}/main.aru");
    let entry = db
        .source_file_by_path(&key)
        .ok_or_else(|| format!("entry `{key}` is not registered"))?;
    let checked = arandu_query::passes::type_check(db, entry);
    let mut diagnostics = checked
        .diagnostics
        .iter()
        .map(|diag| {
            format!(
                "{}|{:?}|{}|{}..{}",
                diag.code, diag.severity, diag.message, diag.span.start, diag.span.end
            )
        })
        .collect::<Vec<_>>();
    diagnostics.sort();
    let amir = if checked
        .diagnostics
        .iter()
        .any(|diag| diag.severity == Severity::Error)
    {
        None
    } else {
        let artifacts = arandu_query::passes::lower_amir(db, entry);
        Some(artifacts.amir.pretty_print(
            &artifacts.type_check.symbols,
            &artifacts.type_check.type_info.type_interner,
        ))
    };
    Ok(Observation { diagnostics, amir })
}

fn check_document_generations(db: &DatabaseImpl) -> Result<usize, String> {
    let source = db
        .source_file_by_path("main.aru")
        .ok_or_else(|| "relative entry key is missing".to_owned())?;
    let mut store = DocumentStore::new();
    let path = PathBuf::from("main.aru");
    let mut stale = Vec::with_capacity(OPERATIONS);
    for _ in 0..OPERATIONS {
        let id = store.open(path.clone(), source);
        stale.push(id);
        store.close(id);
    }
    let live = store.open(path, source);
    if stale.iter().any(|id| store.get(*id).is_some()) || store.get(live).is_none() {
        return Err("DocumentId generation reused a stale handle".into());
    }
    Ok(stale.len())
}

fn seed_project(root: &Path, package: &str) -> Result<(), String> {
    fs::create_dir_all(root.join("src")).map_err(|error| error.to_string())?;
    write_manifest(root, package)?;
    write_main(root, package, 0)?;
    write_text(
        &root.join("src/util.aru"),
        "public func answer(): int { return 42 }\n",
    )
}

fn write_manifest(root: &Path, package: &str) -> Result<(), String> {
    write_text(
        &root.join(MANIFEST_FILENAME),
        &format!("name = \"{package}\"\nversion = \"0.0.1\"\nentry = \"src/main.aru\"\n"),
    )
}

fn write_main(root: &Path, package: &str, revision: usize) -> Result<(), String> {
    write_text(
        &root.join("src/main.aru"),
        &format!(
            "module {package}\nimport {package}.util as util\nfunc main(): int {{\n    let revision: int = {revision}\n    return util.answer() + revision - revision\n}}\n"
        ),
    )
}

fn write_cycle_main(root: &Path, package: &str, revision: usize) -> Result<(), String> {
    write_text(
        &root.join("src/main.aru"),
        &format!(
            "module {package}\nimport {package}.cycle_a as cycle_a\nfunc main(): int {{\n    let revision: int = {revision}\n    return cycle_a.a() + revision - revision\n}}\n"
        ),
    )
}

fn write_text(path: &Path, text: &str) -> Result<(), String> {
    fs::write(path, text).map_err(|error| format!("failed to write {}: {error}", path.display()))
}

fn unique_temp_root(workspace_root: &Path) -> PathBuf {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "arandu-s2b-{}-{nonce}",
        workspace_root
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constants_define_the_gold_campaign() {
        assert_eq!(OPERATIONS, 10_000);
        assert!(OPERATIONS.is_multiple_of(CHECKPOINT_INTERVAL));
    }
}
