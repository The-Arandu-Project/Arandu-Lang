//! Deterministic discovery model for workspace-contained path packages.
//!
//! This module performs filesystem discovery only when called by CLI/LSP
//! orchestration. The resulting graph is ordinary immutable data; queries
//! receive only the derived [`crate::PackageModuleMap`] Salsa input.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use crate::MANIFEST_FILENAME;
use crate::{load_manifest, semantic_manifest_fingerprint, LockedPackage, Lockfile, ManifestData};
use arandu_middle::{ModuleId, PackageId, TargetId};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalPackage {
    pub root: PathBuf,
    pub source: String,
    pub data: ManifestData,
    pub dependencies: BTreeMap<String, String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalPackageGraph {
    pub workspace_root: PathBuf,
    pub packages: Vec<LocalPackage>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlannedModuleBinding {
    pub logical: String,
    pub physical: PathBuf,
    pub package: PackageId,
    pub target: TargetId,
    pub module: ModuleId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackageModulePlan {
    pub current_package: PackageId,
    pub current_target: TargetId,
    pub bindings: Vec<PlannedModuleBinding>,
}

impl LocalPackageGraph {
    pub fn discover(
        workspace_root: &Path,
        root_manifest: &Path,
        root_data: &ManifestData,
    ) -> Result<Self, String> {
        let workspace_root = fs::canonicalize(workspace_root).map_err(|error| {
            format!(
                "cannot canonicalize workspace root {}: {error}",
                workspace_root.display()
            )
        })?;
        let allowed_members = root_data.workspace.as_ref().map(|workspace| {
            workspace
                .members
                .iter()
                .map(|member| member.trim_end_matches('/').to_string())
                .collect::<BTreeSet<_>>()
        });
        let mut discovered = BTreeMap::new();
        let mut visiting = Vec::new();
        discover_package(
            &workspace_root,
            root_manifest,
            root_data.clone(),
            true,
            allowed_members.as_ref(),
            &mut visiting,
            &mut discovered,
        )?;
        let packages = discovered.into_values().collect::<Vec<_>>();
        if packages.len() > u32::MAX as usize {
            return Err("package graph exceeds the supported identity space".into());
        }
        Ok(Self {
            workspace_root,
            packages,
        })
    }

    #[must_use]
    pub fn lockfile(&self, root: &ManifestData) -> Lockfile {
        let packages = self
            .packages
            .iter()
            .map(|package| LockedPackage {
                name: package.data.name.clone(),
                version: package.data.version.clone(),
                source: package.source.clone(),
                manifest_fingerprint: semantic_manifest_fingerprint(&package.data),
                dependencies: package
                    .dependencies
                    .iter()
                    .map(|(alias, source)| format!("{alias}={source}"))
                    .collect(),
            })
            .collect();
        Lockfile::for_packages(root, packages)
    }

    pub fn module_plan(&self, entry_path: &Path) -> Result<Option<PackageModulePlan>, String> {
        let package_ids = self
            .packages
            .iter()
            .enumerate()
            .map(|(index, package)| {
                PackageId::try_from_usize(index)
                    .map(|id| (package.source.clone(), id))
                    .ok_or_else(|| "package graph identity overflow".to_string())
            })
            .collect::<Result<BTreeMap<_, _>, _>>()?;
        let mut target_ids = BTreeMap::new();
        let mut next_target = 0usize;
        for package in &self.packages {
            for kind in ["lib", "bin"] {
                let present = match kind {
                    "lib" => package.data.library_target.is_some(),
                    "bin" => package.data.binary_target.is_some(),
                    _ => false,
                };
                if present {
                    let id = TargetId::try_from_usize(next_target)
                        .ok_or_else(|| "target identity overflow".to_string())?;
                    next_target = next_target
                        .checked_add(1)
                        .ok_or_else(|| "target identity overflow".to_string())?;
                    target_ids.insert((package.source.clone(), kind), id);
                }
            }
        }
        let root = self
            .packages
            .iter()
            .find(|package| package.source == "root")
            .ok_or_else(|| "resolved graph has no root package".to_string())?;
        let current_package = package_ids["root"];
        let root_kind = if root.data.binary_target.is_some() {
            "bin"
        } else {
            "lib"
        };
        let current_target = target_ids[&("root".to_string(), root_kind)];
        if root.dependencies.is_empty() {
            return Ok(None);
        }

        let package_src = entry_path
            .parent()
            .ok_or_else(|| format!("entry {} has no source directory", entry_path.display()))?;
        let mut bindings = BTreeMap::new();
        let mut folded = BTreeSet::new();
        let mut modules = BTreeMap::new();
        let mut next_module = 0usize;
        for relative in crate::scan_aru_entries(package_src) {
            let physical = package_src.join(&relative);
            if physical == entry_path {
                continue;
            }
            let module = module_id(&physical, &mut modules, &mut next_module)?;
            let module_path = relative.trim_end_matches(".aru");
            for logical in [
                format!("self/{module_path}.aru"),
                format!("{module_path}.aru"),
                format!("{}/{module_path}.aru", root.data.name),
            ] {
                insert_planned_binding(
                    &mut bindings,
                    &mut folded,
                    PlannedModuleBinding {
                        logical,
                        physical: physical.clone(),
                        package: current_package,
                        target: current_target,
                        module,
                    },
                )?;
            }
        }
        for (alias, dependency_source) in &root.dependencies {
            let dependency = self
                .packages
                .iter()
                .find(|package| &package.source == dependency_source)
                .ok_or_else(|| format!("missing resolved dependency `{dependency_source}`"))?;
            let library =
                dependency.data.library_target.as_ref().ok_or_else(|| {
                    format!("dependency `{alias}` does not provide a library target")
                })?;
            if library.exports.is_empty() {
                return Err(format!(
                    "dependency `{alias}` must declare `[targets.lib.exports]`; deep imports are not inferred"
                ));
            }
            let package = package_ids[dependency_source];
            let target = target_ids[&(dependency_source.clone(), "lib")];
            for (public_name, relative) in &library.exports {
                let physical = fs::canonicalize(dependency.root.join(relative)).map_err(|error| {
                    format!("cannot resolve export `{alias}.{public_name}` at `{relative}`: {error}")
                })?;
                if !physical.starts_with(&dependency.root) || !physical.is_file() {
                    return Err(format!(
                        "export `{alias}.{public_name}` escapes its package or is not a file"
                    ));
                }
                let module = module_id(&physical, &mut modules, &mut next_module)?;
                let logical = if public_name == "." {
                    format!("{alias}.aru")
                } else {
                    format!("{}/{}.aru", alias, public_name.replace('.', "/"))
                };
                insert_planned_binding(
                    &mut bindings,
                    &mut folded,
                    PlannedModuleBinding {
                        logical,
                        physical,
                        package,
                        target,
                        module,
                    },
                )?;
            }
        }
        Ok(Some(PackageModulePlan {
            current_package,
            current_target,
            bindings: bindings.into_values().collect(),
        }))
    }
}

fn module_id(
    physical: &Path,
    modules: &mut BTreeMap<PathBuf, ModuleId>,
    next_module: &mut usize,
) -> Result<ModuleId, String> {
    if let Some(id) = modules.get(physical) {
        return Ok(*id);
    }
    let id = ModuleId::try_from_usize(*next_module)
        .ok_or_else(|| "module identity overflow".to_string())?;
    *next_module = next_module
        .checked_add(1)
        .ok_or_else(|| "module identity overflow".to_string())?;
    modules.insert(physical.to_path_buf(), id);
    Ok(id)
}

fn insert_planned_binding(
    bindings: &mut BTreeMap<String, PlannedModuleBinding>,
    folded: &mut BTreeSet<String>,
    binding: PlannedModuleBinding,
) -> Result<(), String> {
    if !folded.insert(binding.logical.to_ascii_lowercase()) {
        return Err(format!(
            "case-fold collision or duplicate logical module `{}`",
            binding.logical
        ));
    }
    bindings.insert(binding.logical.clone(), binding);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn discover_package(
    workspace_root: &Path,
    manifest_path: &Path,
    data: ManifestData,
    is_root: bool,
    allowed_members: Option<&BTreeSet<String>>,
    visiting: &mut Vec<String>,
    discovered: &mut BTreeMap<String, LocalPackage>,
) -> Result<String, String> {
    let package_root = manifest_path
        .parent()
        .ok_or_else(|| format!("manifest {} has no parent", manifest_path.display()))?;
    let package_root = fs::canonicalize(package_root)
        .map_err(|error| format!("cannot canonicalize {}: {error}", package_root.display()))?;
    if !package_root.starts_with(workspace_root) {
        return Err(format!(
            "path dependency {} escapes workspace root {}",
            package_root.display(),
            workspace_root.display()
        ));
    }
    let relative = package_root
        .strip_prefix(workspace_root)
        .map_err(|error| error.to_string())?
        .to_string_lossy()
        .replace('\\', "/");
    let source = if is_root || package_root == workspace_root {
        "root".to_string()
    } else {
        format!("path+{relative}")
    };
    if source != "root" {
        if let Some(members) = allowed_members {
            if !members.contains(&relative) {
                return Err(format!(
                    "path dependency `{relative}` is not declared in `[workspace].members`"
                ));
            }
        }
        if data.library_target.is_none() {
            return Err(format!(
                "dependency package `{}` has no `[targets.lib]`",
                data.name
            ));
        }
    }
    if let Some(position) = visiting.iter().position(|item| item == &source) {
        let mut cycle = visiting[position..].to_vec();
        cycle.push(source.clone());
        return Err(format!("cyclic package dependency: {}", cycle.join(" -> ")));
    }
    if discovered.contains_key(&source) {
        return Ok(source);
    }

    visiting.push(source.clone());
    let mut edges = BTreeMap::new();
    let mut identities = BTreeSet::new();
    for (alias, dependency) in &data.dependencies {
        let dependency_root =
            fs::canonicalize(package_root.join(&dependency.path)).map_err(|error| {
                format!(
                    "cannot resolve dependency `{alias}` at `{}`: {error}",
                    dependency.path
                )
            })?;
        let dependency_manifest = dependency_root.join(MANIFEST_FILENAME);
        let (dependency_data, _, _) = load_manifest(&dependency_manifest)
            .map_err(|error| format!("dependency `{alias}`: {error}"))?;
        let dependency_source = discover_package(
            workspace_root,
            &dependency_manifest,
            dependency_data,
            false,
            allowed_members,
            visiting,
            discovered,
        )?;
        if !identities.insert(dependency_source.clone()) {
            return Err(format!(
                "package `{}` binds the same dependency identity more than once",
                data.name
            ));
        }
        edges.insert(alias.clone(), dependency_source);
    }
    visiting.pop();
    discovered.insert(
        source.clone(),
        LocalPackage {
            root: package_root,
            source: source.clone(),
            data,
            dependencies: edges,
        },
    );
    Ok(source)
}
