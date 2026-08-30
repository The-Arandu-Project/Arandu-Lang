//! Frontend-owned remote package graph materialization.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs;
use std::io::ErrorKind;
use std::path::Path;

use crate::cache::CacheStore;
use crate::manifest_io::load_manifest;
use arandu_query::{
    CacheDigest, CacheLayout, LOCK_FILENAME, Lockfile, MANIFEST_FILENAME, ManifestData,
    ManifestDependency, MaterializedGitPackage, PackageGraphLimits,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolutionPolicy {
    pub locked: bool,
    pub offline: bool,
}

pub fn materialize_remote_graph(
    root: &Path,
    manifest_path: &Path,
    root_data: &ManifestData,
    cache: &CacheLayout,
    policy: ResolutionPolicy,
) -> Result<BTreeMap<String, MaterializedGitPackage>, String> {
    let locked_digests = read_locked_git_digests(root, policy.locked)?;
    let store = CacheStore::new(cache.clone());
    let root = fs::canonicalize(root)
        .map_err(|error| format!("cannot canonicalize project root: {error}"))?;
    let mut queue =
        VecDeque::from([(manifest_path.to_path_buf(), root_data.clone(), root.clone())]);
    let mut seen_manifests = BTreeSet::new();
    let mut remotes = BTreeMap::new();
    let mut packages = 0usize;

    while let Some((manifest, data, boundary)) = queue.pop_front() {
        let manifest = fs::canonicalize(&manifest)
            .map_err(|error| format!("cannot canonicalize {}: {error}", manifest.display()))?;
        if !seen_manifests.insert(manifest.clone()) {
            continue;
        }
        packages = packages
            .checked_add(1)
            .ok_or_else(|| "package discovery counter overflow".to_string())?;
        if packages > PackageGraphLimits::default().max_packages {
            return Err("package discovery exceeded maximum package count".into());
        }
        let package_root = manifest
            .parent()
            .ok_or_else(|| format!("manifest {} has no parent", manifest.display()))?;
        for (alias, dependency) in &data.dependencies {
            match dependency {
                ManifestDependency::Path { path } => {
                    let dependency_root =
                        fs::canonicalize(package_root.join(path)).map_err(|error| {
                            format!("cannot resolve dependency `{alias}` at `{path}`: {error}")
                        })?;
                    if !dependency_root.starts_with(&boundary) {
                        return Err(format!(
                            "path dependency `{alias}` escapes package boundary {}",
                            boundary.display()
                        ));
                    }
                    let dependency_manifest = dependency_root.join(MANIFEST_FILENAME);
                    let (dependency_data, _, _) = load_manifest(&dependency_manifest)
                        .map_err(|error| format!("dependency `{alias}`: {error}"))?;
                    queue.push_back((dependency_manifest, dependency_data, boundary.clone()));
                }
                ManifestDependency::Git { origin, commit } => {
                    let identity = format!("git+{origin}#{commit}");
                    if remotes.contains_key(&identity) {
                        continue;
                    }
                    let expected = locked_digests.get(&identity).copied();
                    if policy.locked && expected.is_none() {
                        return Err(format!(
                            "{identity} is absent from arandu.lock and --locked forbids resolving it"
                        ));
                    }
                    let materialized = crate::remote_git::materialize(
                        &store,
                        dependency,
                        expected,
                        policy.offline,
                    )?;
                    let dependency_manifest = materialized.root.join(MANIFEST_FILENAME);
                    let (dependency_data, _, _) = load_manifest(&dependency_manifest)
                        .map_err(|error| format!("dependency `{alias}`: {error}"))?;
                    let remote_root = materialized.root.clone();
                    remotes.insert(
                        identity,
                        MaterializedGitPackage {
                            root: remote_root.clone(),
                            origin: materialized.origin,
                            commit: materialized.commit,
                            content_digest: materialized.content_digest.to_string(),
                        },
                    );
                    queue.push_back((dependency_manifest, dependency_data, remote_root));
                }
            }
        }
    }
    Ok(remotes)
}

fn read_locked_git_digests(
    root: &Path,
    strict: bool,
) -> Result<BTreeMap<String, CacheDigest>, String> {
    let path = root.join(LOCK_FILENAME);
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(error) => return Err(format!("cannot read {}: {error}", path.display())),
    };
    let lock = match Lockfile::parse(&path, &text) {
        Ok(lock) => lock,
        Err(_error) if !strict => return Ok(BTreeMap::new()),
        Err(error) => return Err(error.to_string()),
    };
    lock.packages
        .into_iter()
        .filter_map(|package| {
            package.content_digest.map(|digest| {
                digest
                    .parse::<CacheDigest>()
                    .map(|digest| (package.source, digest))
                    .map_err(|error| error.to_string())
            })
        })
        .collect()
}
