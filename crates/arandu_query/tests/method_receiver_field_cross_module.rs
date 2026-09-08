//! Regression: single-line struct bodies and a method call whose receiver is a
//! *field* of a cross-module type (`self.total.add(...)` → `Stats.add`) must
//! parse and link to a static `FunctionRef`, not degrade to an indirect call.
//!
//! Two regressions are pinned here:
//! - `parse_struct_decl` used to reject comma-separated single-line struct
//!   bodies (`public struct R { x: int, y: int }`) with P001, silently
//!   dropping the module's exports.
//! - `lower_hir/link.rs` used to skip `associated_members`, so
//!   `resolve_method_target` failed at AMIR lowering and the backend ICE'd with
//!   ICE-GEN-001 (indirect calls are unimplemented) — a path users worked
//!   around in stdlib by replacing method calls with free functions.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use arandu_middle::amir::{AmirOperand, AmirStmt};
use arandu_middle::Severity;
use arandu_query::passes::lower_amir;
use arandu_query::{scan_aru_entries, DatabaseImpl, DirectoryListing, ModuleRoots};

fn temp_pkg(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "arandu_method_recv_{name}_{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(dir.join("src")).unwrap();
    dir
}

fn write_pkg_file(root: &Path, name: &str, contents: &str) {
    fs::write(root.join("src").join(name), contents).unwrap();
}

#[test]
fn single_line_struct_fields_and_field_receiver_method_link_to_direct_call() {
    let root = temp_pkg("scan");
    write_pkg_file(
        &root,
        "counter.aru",
        r#"module counter
public struct Stats { code: uint, comment: uint, blank: uint }
public func Stats.add(self: mut ref Stats, other: ref Stats): void {
    self.code = self.code + other.code
    self.comment = self.comment + other.comment
    self.blank = self.blank + other.blank
}
public func zeroStats(): Stats { return Stats { code: 0, comment: 0, blank: 0 } }
"#,
    );
    write_pkg_file(
        &root,
        "walker.aru",
        r#"module walker
import self.counter as counter
public struct ScanResult { total: counter.Stats, file_count: uint }
public func zeroResult(): ScanResult { return ScanResult { total: counter.zeroStats(), file_count: 0 } }
public func ScanResult.addFile(self: mut ref ScanResult, stats: ref counter.Stats): void {
    self.total.add(stats)
    self.file_count = self.file_count + 1
}
public func runOnce(): uint {
    let mut result = zeroResult()
    let mut stats = counter.zeroStats()
    result.total.add(stats)
    result.addFile(stats)
    return result.file_count
}
"#,
    );
    write_pkg_file(
        &root,
        "main.aru",
        r#"module pypor
import self.walker as walker
func main(): int {
    let _ = walker.runOnce()
    return 0
}
"#,
    );

    let mut db = DatabaseImpl::new();
    let src = root.join("src");
    let entries = scan_aru_entries(&src);
    for relative in &entries {
        let path = src.join(relative);
        db.new_file(
            path.to_string_lossy().into_owned(),
            fs::read_to_string(&path).unwrap(),
        );
    }
    let listing = DirectoryListing::new(&db, Arc::new(src.clone()), Arc::new(entries));
    let roots = ModuleRoots::new(&db, "pypor".to_string(), Arc::new(src), None, listing);
    db.set_module_roots(roots);
    let main_path = root.join("src").join("main.aru");
    let root_file = db.new_file(
        main_path.to_string_lossy().into_owned(),
        fs::read_to_string(&main_path).unwrap(),
    );

    let artifacts = lower_amir(&db, root_file);
    let accumulated =
        lower_amir::accumulated::<arandu_middle::db::DiagnosticsAccumulator>(&db, root_file);
    let diagnostics: Vec<_> = accumulated.iter().map(|diagnostic| &diagnostic.0).collect();
    assert!(
        !diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == Severity::Error),
        "cross-module method lowering failed: {diagnostics:?}"
    );

    let mut add_file_sym = None;
    let mut add_sym = None;
    let mut indirect_calls = 0;
    let mut host_names = Vec::new();
    for function in &artifacts.amir.funcs {
        let symbol = artifacts.type_check.symbols.get(function.symbol);
        let name = symbol.name.clone();
        host_names.push(artifacts.type_check.symbols.host_func_name(symbol));
        if name == "ScanResult.addFile" {
            add_file_sym = Some(function.symbol);
            for statement in function.stmts.payloads.raw.iter() {
                if let AmirStmt::Call { callee, .. } = statement {
                    if !matches!(callee, AmirOperand::FunctionRef(_)) {
                        indirect_calls += 1;
                    }
                }
            }
        }
        if name == "Stats.add" {
            add_sym = Some(function.symbol);
        }
    }
    assert!(
        add_sym.is_some(),
        "Stats.add must be present in the linked AMIR definitions"
    );
    assert!(
        add_file_sym.is_some(),
        "ScanResult.addFile must be present in the linked AMIR definitions"
    );
    assert_eq!(
        indirect_calls, 0,
        "`self.total.add(stats)` must lower to a direct FunctionRef call"
    );
    assert!(
        host_names.contains(&"counter.Stats.add"),
        "canonical backend name missing: {host_names:?}"
    );

    let _ = fs::remove_dir_all(&root);
}
