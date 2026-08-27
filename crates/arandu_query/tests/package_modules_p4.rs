//! P4: logical package namespaces are explicit Salsa inputs, not filesystem probes.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use arandu_middle::{ModuleId, PackageId, TargetId};
use arandu_query::{
    register_manifest, DatabaseImpl, ManifestData, ModuleBinding, PackageModuleMap,
};
use salsa::Setter;

fn ids() -> (PackageId, TargetId, ModuleId) {
    (
        PackageId::try_from_usize(1).unwrap(),
        TargetId::try_from_usize(1).unwrap(),
        ModuleId::try_from_usize(1).unwrap(),
    )
}

fn enable_package_mode(db: &DatabaseImpl) {
    let manifest = register_manifest(
        db,
        "arandu.toml".into(),
        ManifestData::legacy("app".into(), "0.1.0".into(), "src/main.aru".into()),
        "test-manifest".into(),
    );
    db.set_project_manifest(manifest);
}

#[test]
fn map_allows_direct_export_and_rejects_private_and_transitive_namespaces() {
    let mut db = DatabaseImpl::new();
    let geometry = db.new_file(
        "dependency/src/geometry.aru".into(),
        "public func answer(): int { return 42 }".into(),
    );
    let private = db.new_file(
        "dependency/src/internal.aru".into(),
        "public func secret(): int { return 99 }".into(),
    );
    let (package, target, module) = ids();
    let map = PackageModuleMap::new(
        &db,
        PackageId::try_from_usize(0).unwrap(),
        TargetId::try_from_usize(0).unwrap(),
        Arc::new(vec![(
            "math/geometry.aru".into(),
            ModuleBinding {
                package,
                target,
                module,
                file: geometry,
            },
        )]),
    );
    db.set_package_module_map(map);

    assert!(db.source_file_by_path("dependency/src/internal.aru") == Some(private));
    assert!(
        arandu_middle::db::SourceDatabase::resolve_module_path(&db, "math/geometry.aru")
            == Some(geometry)
    );
    assert!(
        arandu_middle::db::SourceDatabase::resolve_module_path(&db, "math/internal.aru").is_none()
    );
    assert!(
        arandu_middle::db::SourceDatabase::resolve_module_path(&db, "transitive/thing.aru")
            .is_none()
    );
}

#[test]
fn package_map_change_invalidates_import_without_filesystem_discovery() {
    let mut db = DatabaseImpl::new();
    let geometry = db.new_file(
        "dependency/src/geometry.aru".into(),
        "public func answer(): int { return 42 }".into(),
    );
    let importer = db.new_file(
        "app/src/main.aru".into(),
        "import math.geometry as geometry\nfunc main(): int { return geometry.answer() }".into(),
    );
    let (package, target, module) = ids();
    let map = PackageModuleMap::new(
        &db,
        PackageId::try_from_usize(0).unwrap(),
        TargetId::try_from_usize(0).unwrap(),
        Arc::new(vec![(
            "math/geometry.aru".into(),
            ModuleBinding {
                package,
                target,
                module,
                file: geometry,
            },
        )]),
    );
    db.set_package_module_map(map);
    assert!(arandu_query::passes::type_check(&db, importer)
        .diagnostics
        .is_empty());

    map.set_bindings(&mut db).to(Arc::new(Vec::new()));
    assert!(arandu_query::passes::type_check(&db, importer)
        .diagnostics
        .iter()
        .any(|diagnostic| matches!(
            diagnostic.code,
            arandu_middle::DiagCode::M001UnresolvedImport
        )));
}

#[test]
fn dependency_body_edit_keeps_the_same_export_contract() {
    let mut db = DatabaseImpl::new();
    let dependency = db.new_file(
        "dependency/src/lib.aru".into(),
        "public func answer(): int { return 41 }".into(),
    );
    let importer = db.new_file(
        "app/src/main.aru".into(),
        "import math as math\nfunc main(): int { return math.answer() }".into(),
    );
    let (package, target, module) = ids();
    let map = PackageModuleMap::new(
        &db,
        PackageId::try_from_usize(0).unwrap(),
        TargetId::try_from_usize(0).unwrap(),
        Arc::new(vec![(
            "math.aru".into(),
            ModuleBinding {
                package,
                target,
                module,
                file: dependency,
            },
        )]),
    );
    db.set_package_module_map(map);
    assert!(arandu_query::passes::type_check(&db, importer)
        .diagnostics
        .is_empty());

    dependency
        .set_text(&mut db)
        .to(Arc::from("public func answer(): int { return 42 }"));
    assert!(arandu_query::passes::type_check(&db, importer)
        .diagnostics
        .is_empty());
}

#[test]
fn package_mode_migrates_implicit_local_import_with_structured_replacement() {
    let mut db = DatabaseImpl::new();
    enable_package_mode(&db);
    db.new_file(
        "util.aru".into(),
        "public func answer(): int { return 42 }".into(),
    );
    let importer = db.new_file(
        "src/main.aru".into(),
        "import util as util\nfunc main(): int { return util.answer() }".into(),
    );

    let result = arandu_query::passes::type_check(&db, importer);
    let diagnostic = result
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.code == arandu_middle::DiagCode::M004LegacyLocalImport)
        .expect("legacy local import must have a migration diagnostic");
    let replacement = diagnostic
        .hints
        .iter()
        .find_map(|hint| hint.replacement.as_ref())
        .expect("migration diagnostic must carry a structured replacement");
    assert_eq!(replacement.new_text, "import self.util as util");
}

#[test]
fn package_mode_rejects_quoted_filesystem_import_before_alias_collection() {
    let mut db = DatabaseImpl::new();
    enable_package_mode(&db);
    db.new_file(
        "vendor/util.aru".into(),
        "public func answer(): int { return 42 }".into(),
    );
    let importer = db.new_file(
        "src/main.aru".into(),
        "import \"vendor/util.aru\" as vendor\nfunc main(): int { return 0 }".into(),
    );

    let result = arandu_query::passes::type_check(&db, importer);
    assert!(result.diagnostics.iter().any(|diagnostic| {
        diagnostic.code == arandu_middle::DiagCode::M005FilesystemImportForbidden
    }));
    assert!(result
        .symbols
        .lookup_module(result.symbols.global_scope(), "vendor")
        .is_none());
}
