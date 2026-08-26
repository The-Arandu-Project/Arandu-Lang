//! Deterministic discovery model for workspace-contained path packages.
//!
//! This module performs filesystem discovery only when called by CLI/LSP
//! orchestration. The resulting graph is ordinary immutable data; queries
//! receive only the derived [`crate::PackageModuleMap`] Salsa input.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use crate::MANIFEST_FILENAME;
use crate::{
    load_manifest, semantic_manifest_fingerprint, LockedPackage, Lockfile, ManifestData,
    ManifestDependency,
};
use arandu_middle::{ModuleId, PackageId, TargetId};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalPackage {
    pub root: PathBuf,
    pub source: String,
    pub data: ManifestData,
    pub dependencies: BTreeMap<String, String>,
    pub origin: Option<String>,
    pub commit: Option<String>,
    pub content_digest: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalPackageGraph {
    pub workspace_root: PathBuf,
    pub packages: Vec<LocalPackage>,
}

/// A Git package whose bytes were fetched and verified by CLI/LSP orchestration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MaterializedGitPackage {
    pub root: PathBuf,
    pub origin: String,
    pub commit: String,
    pub content_digest: String,
}

/// Resource limits applied while discovering local packages.
///
/// These limits are deliberately independent from the identity-space limits:
/// they protect CLI/LSP callers from pathological manifests before the graph
/// can consume unbounded recursion or memory.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PackageGraphLimits {
    pub max_packages: usize,
    pub max_dependency_edges: usize,
    pub max_depth: usize,
}

impl Default for PackageGraphLimits {
    fn default() -> Self {
        Self {
            max_packages: 1024,
            max_dependency_edges: 8192,
            max_depth: 64,
        }
    }
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
        Self::discover_materialized(workspace_root, root_manifest, root_data, &BTreeMap::new())
    }

    pub fn discover_materialized(
        workspace_root: &Path,
        root_manifest: &Path,
        root_data: &ManifestData,
        remote_packages: &BTreeMap<String, MaterializedGitPackage>,
    ) -> Result<Self, String> {
        Self::discover_materialized_with_limits(
            workspace_root,
            root_manifest,
            root_data,
            remote_packages,
            PackageGraphLimits::default(),
        )
    }

    pub fn discover_with_limits(
        workspace_root: &Path,
        root_manifest: &Path,
        root_data: &ManifestData,
        limits: PackageGraphLimits,
    ) -> Result<Self, String> {
        Self::discover_materialized_with_limits(
            workspace_root,
            root_manifest,
            root_data,
            &BTreeMap::new(),
            limits,
        )
    }

    pub fn discover_materialized_with_limits(
        workspace_root: &Path,
        root_manifest: &Path,
        root_data: &ManifestData,
        remote_packages: &BTreeMap<String, MaterializedGitPackage>,
        limits: PackageGraphLimits,
    ) -> Result<Self, String> {
        if limits.max_packages == 0 || limits.max_dependency_edges == 0 || limits.max_depth == 0 {
            return Err("package graph limits must be greater than zero".into());
        }
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
        let mut budget = GraphBudget::new(limits);
        discover_package(
            &workspace_root,
            root_manifest,
            root_data.clone(),
            true,
            allowed_members.as_ref(),
            None,
            None,
            remote_packages,
            &mut visiting,
            &mut discovered,
            &mut budget,
        )?;
        let packages = discovered.into_values().collect::<Vec<_>>();
        if packages.len() > usize::try_from(u32::MAX).unwrap_or(usize::MAX) {
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
                origin: package.origin.clone(),
                commit: package.commit.clone(),
                content_digest: package.content_digest.clone(),
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

struct GraphBudget {
    limits: PackageGraphLimits,
    packages: usize,
    dependency_edges: usize,
}

impl GraphBudget {
    fn new(limits: PackageGraphLimits) -> Self {
        Self {
            limits,
            packages: 0,
            dependency_edges: 0,
        }
    }

    fn enter_package(&mut self, source: &str, depth: usize) -> Result<(), String> {
        if depth > self.limits.max_depth {
            return Err(format!(
                "package graph exceeds maximum depth {} at `{source}`",
                self.limits.max_depth
            ));
        }
        self.packages = self
            .packages
            .checked_add(1)
            .ok_or_else(|| "package graph package counter overflow".to_string())?;
        if self.packages > self.limits.max_packages {
            return Err(format!(
                "package graph exceeds maximum package count {}",
                self.limits.max_packages
            ));
        }
        Ok(())
    }

    fn add_edge(&mut self, package: &str, alias: &str) -> Result<(), String> {
        self.dependency_edges = self
            .dependency_edges
            .checked_add(1)
            .ok_or_else(|| "package graph dependency counter overflow".to_string())?;
        if self.dependency_edges > self.limits.max_dependency_edges {
            return Err(format!(
                "package graph exceeds maximum dependency edge count {} at `{package}.{alias}`",
                self.limits.max_dependency_edges
            ));
        }
        Ok(())
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
    remote_boundary: Option<&Path>,
    source_override: Option<&str>,
    remote_packages: &BTreeMap<String, MaterializedGitPackage>,
    visiting: &mut Vec<String>,
    discovered: &mut BTreeMap<String, LocalPackage>,
    budget: &mut GraphBudget,
) -> Result<String, String> {
    let package_root = manifest_path
        .parent()
        .ok_or_else(|| format!("manifest {} has no parent", manifest_path.display()))?;
    let package_root = fs::canonicalize(package_root)
        .map_err(|error| format!("cannot canonicalize {}: {error}", package_root.display()))?;
    let boundary = remote_boundary.unwrap_or(workspace_root);
    if !package_root.starts_with(boundary) {
        return Err(format!(
            "path dependency {} escapes package boundary {}",
            package_root.display(),
            boundary.display()
        ));
    }
    let relative = package_root
        .strip_prefix(boundary)
        .map_err(|error| error.to_string())?
        .to_string_lossy()
        .replace('\\', "/");
    let source = if let Some(source) = source_override {
        source.to_string()
    } else if let Some(remote) = remote_boundary {
        if package_root == remote {
            visiting
                .last()
                .cloned()
                .unwrap_or_else(|| "remote".to_string())
        } else {
            format!(
                "{}+path:{relative}",
                visiting
                    .last()
                    .cloned()
                    .unwrap_or_else(|| "remote".to_string())
            )
        }
    } else if is_root || package_root == workspace_root {
        "root".to_string()
    } else {
        format!("path+{relative}")
    };
    if source != "root" {
        if remote_boundary.is_none() {
            if let Some(members) = allowed_members {
                if !members.contains(&relative) {
                    return Err(format!(
                        "path dependency `{relative}` is not declared in `[workspace].members`"
                    ));
                }
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

    budget.enter_package(&source, visiting.len() + 1)?;

    visiting.push(source.clone());
    let mut edges = BTreeMap::new();
    let mut identities = BTreeSet::new();
    for (alias, dependency) in &data.dependencies {
        budget.add_edge(&source, alias)?;
        let (dependency_root, dependency_remote_boundary, dependency_source_hint) = match dependency
        {
            ManifestDependency::Path { path } => (
                fs::canonicalize(package_root.join(path)).map_err(|error| {
                    format!("cannot resolve dependency `{alias}` at `{path}`: {error}")
                })?,
                remote_boundary,
                None,
            ),
            ManifestDependency::Git { origin, commit } => {
                let identity = format!("git+{origin}#{commit}");
                let remote = remote_packages.get(&identity).ok_or_else(|| {
                    format!("remote Git dependency `{alias}` was not materialized and verified")
                })?;
                (
                    remote.root.clone(),
                    Some(remote.root.as_path()),
                    Some(identity),
                )
            }
        };
        let dependency_manifest = dependency_root.join(MANIFEST_FILENAME);
        let (dependency_data, _, _) = load_manifest(&dependency_manifest)
            .map_err(|error| format!("dependency `{alias}`: {error}"))?;
        let dependency_source = discover_package(
            workspace_root,
            &dependency_manifest,
            dependency_data,
            false,
            allowed_members,
            dependency_remote_boundary,
            dependency_source_hint.as_deref(),
            remote_packages,
            visiting,
            discovered,
            budget,
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
            origin: remote_packages
                .get(&source)
                .map(|remote| remote.origin.clone()),
            commit: remote_packages
                .get(&source)
                .map(|remote| remote.commit.clone()),
            content_digest: remote_packages
                .get(&source)
                .map(|remote| remote.content_digest.clone()),
        },
    );
    Ok(source)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn fixture_root(name: &str) -> PathBuf {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock must be after epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("arandu-package-graph-{name}-{suffix}"));
        fs::create_dir_all(root.join("src")).expect("create fixture");
        fs::write(
            root.join(MANIFEST_FILENAME),
            "schema=1\n[package]\nname='root'\nversion='0.1.0'\nedition='2026'\n[targets.bin]\nname='root'\nroot='src/main.aru'\n[dependencies]\nmath={path='packages/math'}\n[workspace]\nmembers=['packages/math']\n",
        )
        .expect("write root manifest");
        fs::write(root.join("src/main.aru"), "func main(): int { return 0 }\n")
            .expect("write root source");
        fs::create_dir_all(root.join("packages/math/src")).expect("create dependency");
        fs::write(
            root.join("packages/math/arandu.toml"),
            "schema=1\n[package]\nname='math'\nversion='1.0.0'\nedition='2026'\n[targets.lib]\nname='math'\nroot='src/lib.aru'\n[targets.lib.exports]\n'.'='src/lib.aru'\n",
        )
        .expect("write dependency manifest");
        fs::write(
            root.join("packages/math/src/lib.aru"),
            "public func answer(): int { return 42 }\n",
        )
        .expect("write dependency source");
        root
    }

    #[test]
    fn discovery_rejects_package_count_depth_and_edge_budgets() {
        let root = fixture_root("limits");
        let manifest = root.join(MANIFEST_FILENAME);
        let (data, _, _) = load_manifest(&manifest).expect("fixture manifest");

        let limits = PackageGraphLimits {
            max_packages: 1,
            ..PackageGraphLimits::default()
        };
        let error = LocalPackageGraph::discover_with_limits(&root, &manifest, &data, limits)
            .expect_err("package count must be bounded");
        assert!(error.contains("maximum package count"), "{error}");

        let limits = PackageGraphLimits {
            max_dependency_edges: 0,
            ..PackageGraphLimits::default()
        };
        let error = LocalPackageGraph::discover_with_limits(&root, &manifest, &data, limits)
            .expect_err("zero edge limit must be rejected");
        assert!(
            error.contains("limits must be greater than zero"),
            "{error}"
        );

        let limits = PackageGraphLimits {
            max_depth: 1,
            ..PackageGraphLimits::default()
        };
        let error = LocalPackageGraph::discover_with_limits(&root, &manifest, &data, limits)
            .expect_err("depth must be bounded");
        assert!(error.contains("maximum depth"), "{error}");

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn materialized_git_package_enters_graph_and_lock_with_verified_provenance() {
        let root = fixture_root("remote");
        fs::write(
            root.join(MANIFEST_FILENAME),
            "schema=1\n[package]\nname='root'\nversion='0.1.0'\nedition='2026'\n[targets.bin]\nname='root'\nroot='src/main.aru'\n[dependencies]\nmath={git='https://example.com/math.git',rev='0123456789abcdef0123456789abcdef01234567'}\n",
        )
        .unwrap();
        let remote = root.with_extension("remote-cache");
        fs::create_dir_all(remote.join("src")).unwrap();
        fs::write(
            remote.join(MANIFEST_FILENAME),
            "schema=1\n[package]\nname='math'\nversion='1.0.0'\nedition='2026'\n[targets.lib]\nname='math'\nroot='src/lib.aru'\n[targets.lib.exports]\n'.'='src/lib.aru'\n",
        )
        .unwrap();
        fs::write(
            remote.join("src/lib.aru"),
            "public func answer(): int { return 42 }\n",
        )
        .unwrap();

        let manifest = root.join(MANIFEST_FILENAME);
        let (data, _, _) = load_manifest(&manifest).unwrap();
        let source = "git+https://example.com/math.git#0123456789abcdef0123456789abcdef01234567";
        let remotes = BTreeMap::from([(
            source.to_string(),
            MaterializedGitPackage {
                root: fs::canonicalize(&remote).unwrap(),
                origin: "https://example.com/math.git".into(),
                commit: "0123456789abcdef0123456789abcdef01234567".into(),
                content_digest:
                    "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
            },
        )]);
        let graph = LocalPackageGraph::discover_materialized(&root, &manifest, &data, &remotes)
            .expect("verified remote graph");
        let locked = graph.lockfile(&data);
        let remote_package = locked
            .packages
            .iter()
            .find(|package| package.source == source)
            .expect("remote lock entry");
        assert_eq!(
            remote_package.origin.as_deref(),
            Some("https://example.com/math.git")
        );
        assert_eq!(
            remote_package.content_digest.as_deref(),
            Some("sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
        );

        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(remote);
    }
}
