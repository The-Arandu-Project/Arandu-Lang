//! Background workspace discovery and file registration.
//!
//! Runs after the initialize handshake (AGENTS.md): the main thread registers
//! discovered files into [`ServerState`] while local resources stay available.
//! Discovery is bounded by the worker pool backlog; it never blocks `initialize`.

use crate::pool::{Priority, WorkerPool};
use crate::state::{discover_aru_files, ServerState};
use crate::uri_util::uri_from_path;
use arandu_query::{
    find_manifest, load_manifest, resolve_stdlib_root, scan_aru_entries, ManifestData,
    StdlibResolveOpts,
};
use crossbeam_channel::{bounded, Receiver};
use std::path::PathBuf;

pub(crate) struct WorkspaceFile {
    pub(crate) path: PathBuf,
    pub(crate) text: String,
}

pub(crate) struct WorkspaceProject {
    pub(crate) manifest_path: PathBuf,
    pub(crate) manifest_data: ManifestData,
    pub(crate) manifest_hash: String,
    pub(crate) package_src: PathBuf,
    pub(crate) entries: Vec<String>,
    pub(crate) stdlib_root: Option<PathBuf>,
}

pub(crate) enum WorkspaceEvent {
    Project(WorkspaceProject),
    File(WorkspaceFile),
    Done,
}

pub(crate) fn spawn_workspace_discovery(
    pool: &WorkerPool,
    roots: Vec<PathBuf>,
) -> Receiver<WorkspaceEvent> {
    const DISCOVERY_BACKLOG: usize = 8;
    let (tx, rx) = bounded(DISCOVERY_BACKLOG);
    if roots.is_empty() {
        let _ = tx.send(WorkspaceEvent::Done);
        return rx;
    }
    let _ = pool.spawn(Priority::Background, None, move |cancellation| {
        // The compiler DB currently owns one ModuleRoots input. Select the
        // first workspace package deterministically; multi-root ownership is a
        // separate protocol capability, not an order-dependent overwrite.
        if let Some(project) = discover_workspace_project(&roots) {
            if tx.send(WorkspaceEvent::Project(project)).is_err() {
                return;
            }
        }
        for (path, text) in discover_aru_files(&roots) {
            if cancellation.is_cancelled() {
                break;
            }
            if tx
                .send(WorkspaceEvent::File(WorkspaceFile { path, text }))
                .is_err()
            {
                break;
            }
        }
        let _ = tx.send(WorkspaceEvent::Done);
    });
    rx
}

pub(crate) fn discover_workspace_project(roots: &[PathBuf]) -> Option<WorkspaceProject> {
    for root in roots {
        let Some(manifest_path) = find_manifest(root) else {
            continue;
        };
        let Ok((manifest_data, manifest_hash, _)) = load_manifest(&manifest_path) else {
            continue;
        };
        let package_root = manifest_path
            .parent()
            .map(std::path::Path::to_path_buf)
            .unwrap_or_else(|| root.clone());
        let entry_path = package_root.join(&manifest_data.entry);
        let package_src = entry_path
            .parent()
            .map(std::path::Path::to_path_buf)
            .unwrap_or(package_root);
        let entries = scan_aru_entries(&package_src);
        let stdlib_root = resolve_stdlib_root(StdlibResolveOpts::default())
            .ok()
            .map(|stdlib| stdlib.path);
        return Some(WorkspaceProject {
            manifest_path,
            manifest_data,
            manifest_hash,
            package_src,
            entries,
            stdlib_root,
        });
    }
    None
}

/// Registers a discovered file unless an editor buffer already owns the URI —
/// never replace a newer editor buffer with a stale disk snapshot.
pub(crate) fn register_workspace_file(state: &mut ServerState, file: WorkspaceFile) {
    let Some(uri) = uri_from_path(&file.path) else {
        return;
    };
    if !state.by_uri.contains_key(uri.as_str()) {
        state.open_or_commit(&uri, file.text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_project_discovery_loads_manifest_and_listing() {
        let root = std::env::temp_dir().join(format!(
            "arandu-lsp-project-discovery-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock after epoch")
                .as_nanos()
        ));
        std::fs::create_dir_all(root.join("src")).expect("create fixture");
        std::fs::write(
            root.join("Arandu.toml"),
            "name = \"editor_gold\"\nversion = \"0.1.0\"\nentry = \"src/main.aru\"\n",
        )
        .expect("write manifest");
        std::fs::write(root.join("src/main.aru"), "func main() {}\n").expect("write entry");

        let project = discover_workspace_project(std::slice::from_ref(&root))
            .expect("discover package metadata");
        assert_eq!(project.manifest_data.name, "editor_gold");
        assert_eq!(
            project.package_src,
            std::fs::canonicalize(root.join("src")).expect("canonical fixture source")
        );
        assert_eq!(project.entries, vec!["main.aru"]);

        std::fs::remove_dir_all(root).expect("remove fixture");
    }
}
