//! Watch buffer + package session tests (no notify OS loop).
//!
//! Validates Salsa invalidation API before wiring notify-debouncer-full:
//! rename atomicity, delete → M001, package_name cascade.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use arandu_middle::db::SourceDatabase;
use arandu_query::{
    hash_manifest_bytes, parse_manifest_bytes, register_manifest, scan_aru_entries, DatabaseImpl,
    DirectoryListing, FsChange, ModuleRoots, PackageWatchSession, WatchBuffer, MANIFEST_FILENAME,
};

fn temp_pkg(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "arandu_watch_{tag}_{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(dir.join("src")).unwrap();
    dir
}

fn write_manifest(root: &std::path::Path, name: &str) {
    fs::write(
        root.join(MANIFEST_FILENAME),
        format!(
            r#"name = "{name}"
version = "0.0.1"
entry = "src/main.aru"
"#
        ),
    )
    .unwrap();
}

fn seed_session(
    db: &mut DatabaseImpl,
    root: PathBuf,
    name: &str,
) -> (PackageWatchSession, arandu_query::SourceFile) {
    write_manifest(&root, name);
    let manifest_path = root.join(MANIFEST_FILENAME);
    let bytes = fs::read(&manifest_path).unwrap();
    let data = parse_manifest_bytes(&manifest_path, &bytes).unwrap();
    let hash = hash_manifest_bytes(&bytes);
    let entry_abs = root.join(&data.entry);
    let package_src = entry_abs.parent().unwrap().to_path_buf();

    fs::write(
        package_src.join("util.aru"),
        "public func answer(): int { return 42 }\n",
    )
    .unwrap();
    fs::write(
        package_src.join("main.aru"),
        format!(
            r#"
module {name}
import {name}.util as util
func main(): int {{
    return util.answer()
}}
"#
        ),
    )
    .unwrap();

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

    // Register package modules under import keys.
    for rel in scan_aru_entries(&package_src) {
        let text = fs::read_to_string(package_src.join(&rel)).unwrap();
        let key_pkg = format!("{}/{}", data.name, rel);
        db.new_file(key_pkg, text.clone());
        db.new_file(rel.clone(), text);
    }

    let main_key = format!("{}/main.aru", data.name);
    let main = db.source_file_by_path(&main_key).unwrap();

    let sess = PackageWatchSession::new(
        db,
        arandu_query::PackageWatchConfig {
            package_root: root,
            package_src,
            package_name: data.name,
            entry_rel: data.entry,
            entry_abs,
            manifest_path,
            listing,
            module_roots: roots,
            manifest,
        },
    );
    (sess, main)
}

#[test]
fn debounce_does_not_commit_inside_window() {
    let mut buf = WatchBuffer::with_debounce(Duration::from_millis(80));
    buf.push(PathBuf::from("/tmp/a.aru"), FsChange::Modify);
    assert!(buf.take_due().is_empty());
    assert_eq!(buf.pending_count(), 1);
}

#[test]
fn rename_single_commit_no_orphan_remove() {
    let root = temp_pkg("rename");
    let mut db = DatabaseImpl::new();
    let (mut sess, main) = seed_session(&mut db, root.clone(), "my_app");

    // Baseline green.
    let diags = sess.check_entry(&db, main);
    assert!(
        diags
            .iter()
            .all(|d| d.severity != arandu_middle::Severity::Error),
        "baseline errors: {diags:?}"
    );

    let from = sess.package_src.join("util.aru");
    let to = sess.package_src.join("util2.aru");
    fs::rename(&from, &to).unwrap();

    // One correlated rename — not Remove then Create as separate commits.
    sess.push_rename(from, to);
    let summary = sess.commit(&mut db, true);
    assert_eq!(summary.renamed, 1);
    assert!(
        summary.removed >= 1 && summary.created >= 1,
        "rename expands to remove+create inside one commit: {summary:?}"
    );

    // Mid-batch never ran typeck; after commit, update main to import util2
    // so the project is consistent (user would rename imports too).
    // For this test we only assert util.aru key is gone and util2 exists —
    // no intermediate M001 was observed because we only typecheck after commit.
    assert!(db.resolve_module_path("my_app/util.aru").is_none());
    assert!(db.resolve_module_path("my_app/util2.aru").is_some());

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn delete_util_emits_explicit_m001_not_silence() {
    let root = temp_pkg("delete");
    let mut db = DatabaseImpl::new();
    let (mut sess, main) = seed_session(&mut db, root.clone(), "my_app");

    let util = sess.package_src.join("util.aru");
    fs::remove_file(&util).unwrap();
    sess.push(util, FsChange::Remove);
    let summary = sess.commit(&mut db, true);
    assert!(summary.removed >= 1);
    assert!(
        !summary.unregistered_keys.is_empty(),
        "must unregister import keys: {summary:?}"
    );

    let diags = sess.check_entry(&db, main);
    let has_m001 = diags.iter().any(|d| {
        matches!(d.code, arandu_middle::DiagCode::M001UnresolvedImport)
            || d.message.contains("unresolved import")
            || d.message.contains("my_app.util")
            || d.message.contains("util")
    });
    assert!(
        has_m001,
        "delete of imported module must surface M001, got: {diags:?}"
    );

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn package_name_change_invalidates_local_imports() {
    let root = temp_pkg("rename_pkg");
    let mut db = DatabaseImpl::new();
    let (mut sess, main) = seed_session(&mut db, root.clone(), "my_app");

    // Rename package my_app → core_lib in Arandu.toml only (source still says my_app.util).
    write_manifest(&root, "core_lib");
    sess.push(root.join(MANIFEST_FILENAME), FsChange::Modify);
    let summary = sess.commit(&mut db, true);
    assert!(
        summary.manifest_reloaded,
        "manifest change must reload: {summary:?}"
    );

    // Old import keys must not resolve under old package name.
    assert!(
        db.resolve_module_path("my_app/util.aru").is_none(),
        "old package prefix must be gone after rename"
    );
    // New prefix should resolve (files re-registered under core_lib/…).
    assert!(
        db.resolve_module_path("core_lib/util.aru").is_some(),
        "new package prefix must resolve"
    );

    // Entry still imports my_app.util → broken import diagnostic.
    // Re-open main under new key if needed.
    let main = db
        .source_file_by_path("core_lib/main.aru")
        .or_else(|| db.source_file_by_path("main.aru"))
        .unwrap_or(main);
    // Refresh main text still has import my_app.util
    let diags = sess.check_entry(&db, main);
    let broken = diags.iter().any(|d| {
        matches!(d.code, arandu_middle::DiagCode::M001UnresolvedImport)
            || d.message.contains("my_app")
            || d.message.contains("unresolved")
    });
    assert!(
        broken,
        "stale import my_app.util after package rename must fail: {diags:?}"
    );

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn modify_util_body_updates_via_set_text() {
    let root = temp_pkg("modify");
    let mut db = DatabaseImpl::new();
    let (mut sess, main) = seed_session(&mut db, root.clone(), "my_app");

    let util = sess.package_src.join("util.aru");
    fs::write(&util, "public func answer(): int { return 7 }\n").unwrap();
    sess.push(util, FsChange::Modify);
    let summary = sess.commit(&mut db, true);
    assert!(summary.modified >= 1);

    // typeck still clean (signature unchanged).
    let diags = sess.check_entry(&db, main);
    assert!(
        diags
            .iter()
            .all(|d| d.severity != arandu_middle::Severity::Error),
        "{diags:?}"
    );

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn create_registers_package_and_relative_keys_after_path_canonicalization() {
    let root = temp_pkg("create_keys");
    let mut db = DatabaseImpl::new();
    let (mut sess, _) = seed_session(&mut db, root.clone(), "my_app");

    let added = sess.package_src.join("added.aru");
    fs::write(&added, "public func added(): int { return 1 }\n").unwrap();
    sess.push(added, FsChange::Create);
    let summary = sess.commit(&mut db, true);

    assert_eq!(summary.created, 1);
    assert!(db.source_file_by_path("my_app/added.aru").is_some());
    assert!(db.source_file_by_path("added.aru").is_some());
    let _ = fs::remove_dir_all(&root);
}
