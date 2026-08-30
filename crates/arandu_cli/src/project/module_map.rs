//! Incremental package module map construction and registration.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use arandu_query::LocalPackageGraph;

pub fn install_package_module_map(
    db: &mut arandu_query::DatabaseImpl,
    graph: &LocalPackageGraph,
    entry_path: &Path,
) -> Result<(), String> {
    let Some(plan) = graph.module_plan(entry_path)? else {
        return Ok(());
    };
    let mut bindings = BTreeMap::new();
    let mut files = BTreeMap::new();
    for planned in plan.bindings {
        let file = if let Some(file) = files.get(&planned.physical) {
            *file
        } else if let Some(file) = db.source_file_by_path(&planned.physical.to_string_lossy()) {
            files.insert(planned.physical.clone(), file);
            file
        } else {
            let text = fs::read_to_string(&planned.physical).map_err(|error| {
                format!("cannot read module {}: {error}", planned.physical.display())
            })?;
            let file = db.new_file(planned.physical.to_string_lossy().into_owned(), text);
            files.insert(planned.physical.clone(), file);
            file
        };
        bindings.insert(
            planned.logical,
            arandu_query::ModuleBinding {
                package: planned.package,
                target: planned.target,
                module: planned.module,
                file,
            },
        );
    }

    let map = arandu_query::PackageModuleMap::new(
        db,
        plan.current_package,
        plan.current_target,
        std::sync::Arc::new(bindings.into_iter().collect()),
    );
    db.set_package_module_map(map);
    Ok(())
}
