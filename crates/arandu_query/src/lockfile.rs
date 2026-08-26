//! Pure, deterministic `arandu.lock` model. Filesystem policy belongs to the CLI.

use std::collections::BTreeSet;
use std::fmt;
use std::path::Path;

use serde::Deserialize;

use crate::manifest::{ManifestData, ManifestEdition, PackageKind};

pub const LOCK_FILENAME: &str = "arandu.lock";
pub const LOCK_VERSION: u32 = 1;
const DIGEST_PREFIX: &str = "blake3:";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lockfile {
    pub manifest_fingerprint: String,
    pub packages: Vec<LockedPackage>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockedPackage {
    pub name: String,
    pub version: String,
    pub source: String,
    pub manifest_fingerprint: String,
    pub dependencies: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockfileError(String);

impl fmt::Display for LockfileError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for LockfileError {}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawLockfile {
    version: u32,
    manifest_fingerprint: String,
    package: Vec<RawPackage>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPackage {
    name: String,
    version: String,
    source: String,
    #[serde(default)]
    manifest_fingerprint: String,
    dependencies: Vec<String>,
}

impl Lockfile {
    #[must_use]
    pub fn for_manifest(manifest: &ManifestData) -> Self {
        Self {
            manifest_fingerprint: semantic_manifest_fingerprint(manifest),
            packages: vec![LockedPackage {
                name: manifest.name.clone(),
                version: manifest.version.clone(),
                source: "root".into(),
                manifest_fingerprint: semantic_manifest_fingerprint(manifest),
                // P3 freezes the single root. P4 replaces these aliases with
                // resolved package identities while retaining format v1.
                dependencies: manifest.dependencies.keys().cloned().collect(),
            }],
        }
    }

    #[must_use]
    pub fn for_packages(manifest: &ManifestData, mut packages: Vec<LockedPackage>) -> Self {
        packages.sort_by(|left, right| {
            (&left.source, &left.name, &left.version).cmp(&(
                &right.source,
                &right.name,
                &right.version,
            ))
        });
        let mut graph = semantic_manifest_fingerprint(manifest);
        for package in &packages {
            push_component(&mut graph, "package.source", &package.source);
            push_component(&mut graph, "package.name", &package.name);
            push_component(&mut graph, "package.version", &package.version);
            push_component(
                &mut graph,
                "package.manifest",
                &package.manifest_fingerprint,
            );
            let mut dependencies = package.dependencies.clone();
            dependencies.sort();
            for dependency in dependencies {
                push_component(&mut graph, "package.dependency", &dependency);
            }
        }
        Self {
            manifest_fingerprint: format!(
                "{DIGEST_PREFIX}{}",
                blake3::hash(graph.as_bytes()).to_hex()
            ),
            packages,
        }
    }

    pub fn parse(path: &Path, text: &str) -> Result<Self, LockfileError> {
        let raw: RawLockfile = toml::from_str(text)
            .map_err(|error| LockfileError(format!("invalid {}: {error}", path.display())))?;
        if raw.version != LOCK_VERSION {
            return Err(LockfileError(format!(
                "unsupported lockfile version {}; expected {LOCK_VERSION}",
                raw.version
            )));
        }
        validate_digest(&raw.manifest_fingerprint)?;
        if raw.package.is_empty() {
            return Err(LockfileError("lockfile package graph is empty".into()));
        }
        let mut identities = BTreeSet::new();
        let mut packages = Vec::with_capacity(raw.package.len());
        for package in raw.package {
            if package.name.is_empty() || package.source.is_empty() {
                return Err(LockfileError(
                    "lockfile package name and source must be non-empty".into(),
                ));
            }
            semver::Version::parse(&package.version).map_err(|error| {
                LockfileError(format!(
                    "invalid locked version for `{}`: {error}",
                    package.name
                ))
            })?;
            validate_portable_source(&package.source)?;
            if !package.manifest_fingerprint.is_empty() {
                validate_digest(&package.manifest_fingerprint)?;
            }
            let identity = (package.source.clone(), package.name.clone());
            if !identities.insert(identity) {
                return Err(LockfileError(format!(
                    "duplicate locked package `{}` from `{}`",
                    package.name, package.source
                )));
            }
            let mut dependencies = package.dependencies;
            if dependencies.iter().any(|value| value.is_empty()) {
                return Err(LockfileError(
                    "empty dependency identity in lockfile".into(),
                ));
            }
            let original = dependencies.clone();
            dependencies.sort();
            dependencies.dedup();
            if dependencies != original {
                return Err(LockfileError(
                    "lockfile dependencies must be sorted and unique".into(),
                ));
            }
            packages.push(LockedPackage {
                name: package.name,
                version: package.version,
                source: package.source,
                manifest_fingerprint: package.manifest_fingerprint,
                dependencies,
            });
        }
        let original = packages.clone();
        packages.sort_by(|left, right| {
            (&left.source, &left.name, &left.version).cmp(&(
                &right.source,
                &right.name,
                &right.version,
            ))
        });
        if packages != original {
            return Err(LockfileError(
                "lockfile packages must use canonical identity order".into(),
            ));
        }
        Ok(Self {
            manifest_fingerprint: raw.manifest_fingerprint,
            packages,
        })
    }

    #[must_use]
    pub fn to_canonical_bytes(&self) -> Vec<u8> {
        let mut output =
            String::from("# This file is generated by Arandu. Do not edit.\nversion = 1\n");
        push_string_field(
            &mut output,
            "manifest_fingerprint",
            &self.manifest_fingerprint,
        );
        let mut packages = self.packages.clone();
        packages.sort_by(|left, right| {
            (&left.source, &left.name, &left.version).cmp(&(
                &right.source,
                &right.name,
                &right.version,
            ))
        });
        for package in &packages {
            output.push_str("\n[[package]]\n");
            push_string_field(&mut output, "name", &package.name);
            push_string_field(&mut output, "version", &package.version);
            push_string_field(&mut output, "source", &package.source);
            push_string_field(
                &mut output,
                "manifest_fingerprint",
                &package.manifest_fingerprint,
            );
            output.push_str("dependencies = [");
            let mut dependencies = package.dependencies.clone();
            dependencies.sort();
            dependencies.dedup();
            for (index, dependency) in dependencies.iter().enumerate() {
                if index != 0 {
                    output.push_str(", ");
                }
                output.push_str(&toml_string(dependency));
            }
            output.push_str("]\n");
        }
        output.into_bytes()
    }
}

#[must_use]
pub fn semantic_manifest_fingerprint(manifest: &ManifestData) -> String {
    let mut canonical = String::new();
    push_component(&mut canonical, "schema", &manifest.schema.to_string());
    push_component(&mut canonical, "name", &manifest.name);
    push_component(&mut canonical, "version", &manifest.version);
    push_component(
        &mut canonical,
        "edition",
        match manifest.edition {
            ManifestEdition::Legacy => "legacy",
            ManifestEdition::V2026 => "2026",
        },
    );
    push_component(
        &mut canonical,
        "kind",
        match manifest.kind {
            PackageKind::Binary => "binary",
            PackageKind::Library => "library",
            PackageKind::Mixed => "mixed",
        },
    );
    push_component(
        &mut canonical,
        "toolchain",
        manifest.toolchain_requirement.as_deref().unwrap_or(""),
    );
    for (kind, target) in [
        ("bin", manifest.binary_target.as_ref()),
        ("lib", manifest.library_target.as_ref()),
    ] {
        if let Some(target) = target {
            push_component(&mut canonical, &format!("target.{kind}.name"), &target.name);
            push_component(&mut canonical, &format!("target.{kind}.root"), &target.root);
            for (public_name, source) in &target.exports {
                push_component(
                    &mut canonical,
                    &format!("target.{kind}.export.{public_name}"),
                    source,
                );
            }
        }
    }
    for (alias, dependency) in &manifest.dependencies {
        push_component(
            &mut canonical,
            &format!("dependency.{alias}"),
            &dependency.path,
        );
    }
    if let Some(workspace) = &manifest.workspace {
        for member in &workspace.members {
            push_component(&mut canonical, "workspace.member", member);
        }
    }
    format!(
        "{DIGEST_PREFIX}{}",
        blake3::hash(canonical.as_bytes()).to_hex()
    )
}

fn push_component(output: &mut String, key: &str, value: &str) {
    output.push_str(&key.len().to_string());
    output.push(':');
    output.push_str(key);
    output.push(':');
    output.push_str(&value.len().to_string());
    output.push(':');
    output.push_str(value);
    output.push('\n');
}

fn validate_digest(value: &str) -> Result<(), LockfileError> {
    let Some(hex) = value.strip_prefix(DIGEST_PREFIX) else {
        return Err(LockfileError(
            "manifest fingerprint must use `blake3:<64 lowercase hex>`".into(),
        ));
    };
    if hex.len() != 64
        || !hex
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(LockfileError(
            "manifest fingerprint must use `blake3:<64 lowercase hex>`".into(),
        ));
    }
    Ok(())
}

fn validate_portable_source(source: &str) -> Result<(), LockfileError> {
    let portable_path = source.strip_prefix("path+").is_some_and(|path| {
        !path.is_empty()
            && !path.contains('\\')
            && !path.starts_with('/')
            && !path
                .split('/')
                .any(|segment| segment.is_empty() || segment == "." || segment == "..")
    });
    if source != "root" && !portable_path {
        return Err(LockfileError(format!(
            "nonportable package source `{source}` in lockfile"
        )));
    }
    Ok(())
}

fn push_string_field(output: &mut String, key: &str, value: &str) {
    output.push_str(key);
    output.push_str(" = ");
    output.push_str(&toml_string(value));
    output.push('\n');
}

fn toml_string(value: &str) -> String {
    let mut output = String::with_capacity(value.len() + 2);
    output.push('"');
    for character in value.chars() {
        match character {
            '\\' => output.push_str("\\\\"),
            '"' => output.push_str("\\\""),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            character if character.is_control() => {
                use std::fmt::Write as _;
                let _ = write!(output, "\\u{:04X}", u32::from(character));
            }
            character => output.push(character),
        }
    }
    output.push('"');
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::parse_manifest_str;

    fn manifest(text: &str) -> ManifestData {
        parse_manifest_str(Path::new("arandu.toml"), text).unwrap()
    }

    #[test]
    fn canonical_lock_is_byte_stable_and_lf_only() {
        let data = manifest(
            "schema=1\n[package]\nname='app'\nversion='1.0.0'\nedition='2026'\n[targets.bin]\nname='app'\nroot='src/main.aru'\n",
        );
        let lock = Lockfile::for_manifest(&data);
        let bytes = lock.to_canonical_bytes();
        assert!(!bytes.contains(&b'\r'));
        assert_eq!(
            Lockfile::parse(
                Path::new("arandu.lock"),
                std::str::from_utf8(&bytes).unwrap()
            )
            .unwrap(),
            lock
        );
    }

    #[test]
    fn semantic_fingerprint_ignores_toml_layout_but_not_resolution_inputs() {
        let compact = manifest("name='app'\nversion='1.0.0'\nentry='src/main.aru'\n");
        let spaced =
            manifest("# comment\nname = 'app'\nversion = '1.0.0'\nentry = 'src/main.aru'\n");
        assert_eq!(
            semantic_manifest_fingerprint(&compact),
            semantic_manifest_fingerprint(&spaced)
        );
        let changed = manifest("name='app'\nversion='1.0.1'\nentry='src/main.aru'\n");
        assert_ne!(
            semantic_manifest_fingerprint(&compact),
            semantic_manifest_fingerprint(&changed)
        );
    }

    #[test]
    fn parser_rejects_unknown_corrupt_duplicate_and_nonportable_data() {
        let digest = format!("blake3:{}", "0".repeat(64));
        let base = format!("version=1\nmanifest_fingerprint='{digest}'\n[[package]]\nname='a'\nversion='1.0.0'\nsource='root'\ndependencies=[]\n");
        assert!(Lockfile::parse(
            Path::new("arandu.lock"),
            &(base.clone() + "surprise=true\n")
        )
        .is_err());
        assert!(Lockfile::parse(
            Path::new("arandu.lock"),
            &base.replace(&digest, "blake3:nope")
        )
        .is_err());
        assert!(Lockfile::parse(
            Path::new("arandu.lock"),
            &base.replace("source='root'", "source='C:\\\\repo'")
        )
        .is_err());
        assert!(Lockfile::parse(
            Path::new("arandu.lock"),
            &(base.clone() + &base[base.find("[[package]]").unwrap()..])
        )
        .is_err());
    }
}
