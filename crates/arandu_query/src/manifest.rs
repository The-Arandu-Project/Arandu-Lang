//! Project manifest (`arandu.toml`) — Salsa input from day 1.
//!
//! Gold bar (P2): the manifest is a `#[salsa::input]` whose **content hash**
//! participates in the invalidation key. Changing `entry` / `name` / `version`
//! (or any future field) must not leave a stale cache; registering the input
//! now avoids a painful migration when deps/workspace land.
//!
//! Parse errors are **never swallowed** (BUG-09 discipline): a malformed
//! `Arandu.toml` is a hard error with path + reason.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Deserialize;

/// Canonical on-disk filename for a project package.
pub const MANIFEST_FILENAME: &str = "arandu.toml";

/// Previous case-sensitive spelling, readable only during migration.
pub const LEGACY_MANIFEST_FILENAME: &str = "Arandu.toml";

/// Parsed package fields (MVP: name / version / entry).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestData {
    pub name: String,
    pub version: String,
    pub entry: String,
    pub schema: u32,
    pub edition: ManifestEdition,
    pub kind: PackageKind,
    pub toolchain_requirement: Option<String>,
    pub binary_target: Option<ManifestTarget>,
    pub library_target: Option<ManifestTarget>,
    pub capabilities: CapabilityPolicy,
    pub effect_policy: EffectPolicy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManifestEdition {
    Legacy,
    V2026,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackageKind {
    Binary,
    Library,
    Mixed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestTarget {
    pub name: String,
    pub root: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CapabilityPolicy {
    pub network: Vec<String>,
    pub filesystem_read: Vec<String>,
    pub filesystem_write: Vec<String>,
    pub environment: Vec<String>,
    pub process: Vec<String>,
    pub foreign: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectPolicy {
    pub deny_new_authority: bool,
    pub warn_new_resources: bool,
    pub deny: Vec<String>,
}

impl Default for EffectPolicy {
    fn default() -> Self {
        Self {
            deny_new_authority: true,
            warn_new_resources: true,
            deny: vec!["UnknownCapability".into()],
        }
    }
}

impl ManifestData {
    #[must_use]
    pub fn legacy(name: String, version: String, entry: String) -> Self {
        Self {
            name: name.clone(),
            version,
            entry: entry.clone(),
            schema: 0,
            edition: ManifestEdition::Legacy,
            kind: PackageKind::Binary,
            toolchain_requirement: None,
            binary_target: Some(ManifestTarget { name, root: entry }),
            library_target: None,
            capabilities: CapabilityPolicy::default(),
            effect_policy: EffectPolicy::default(),
        }
    }
}

/// Result of filesystem discovery before the manifest enters Salsa.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestDiscovery {
    pub path: PathBuf,
    pub spelling: ManifestSpelling,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManifestSpelling {
    Canonical,
    Legacy,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestSchema {
    schema: u32,
    package: PackageSection,
    #[serde(default)]
    toolchain: Option<ToolchainSection>,
    targets: TargetsSection,
    #[serde(default)]
    dependencies: std::collections::BTreeMap<String, PathDependency>,
    #[serde(default)]
    capabilities: RawCapabilityPolicy,
    #[serde(default)]
    policy: RawPolicy,
    #[serde(default)]
    metadata: toml::Table,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PackageSection {
    name: String,
    version: String,
    edition: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ToolchainSection {
    arandu: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TargetsSection {
    #[serde(default)]
    bin: Option<TargetSection>,
    #[serde(default)]
    lib: Option<TargetSection>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TargetSection {
    name: String,
    root: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PathDependency {
    path: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCapabilityPolicy {
    #[serde(default)]
    network: Vec<String>,
    #[serde(default)]
    filesystem_read: Vec<String>,
    #[serde(default)]
    filesystem_write: Vec<String>,
    #[serde(default)]
    environment: Vec<String>,
    #[serde(default)]
    process: Vec<String>,
    #[serde(default)]
    foreign: bool,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPolicy {
    #[serde(default)]
    effects: Option<RawEffectPolicy>,
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RawEffectPolicy {
    deny_new_authority: bool,
    warn_new_resources: bool,
    deny: Vec<String>,
}

impl Default for RawEffectPolicy {
    fn default() -> Self {
        let policy = EffectPolicy::default();
        Self {
            deny_new_authority: policy.deny_new_authority,
            warn_new_resources: policy.warn_new_resources,
            deny: policy.deny,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyManifest {
    name: String,
    version: String,
    entry: String,
}

/// Why reading or parsing `Arandu.toml` failed.
#[derive(Debug, Clone)]
pub enum ManifestError {
    Io {
        path: PathBuf,
        message: String,
    },
    Parse {
        path: PathBuf,
        message: String,
    },
    MissingField {
        path: PathBuf,
        field: &'static str,
    },
    /// Package `name` collides with a reserved stdlib/language root (PROMOTE-L2).
    ReservedName {
        path: PathBuf,
        message: String,
    },
    ConflictingFiles {
        canonical: PathBuf,
        legacy: PathBuf,
    },
    IncompatibleToolchain {
        path: PathBuf,
        required: String,
        current: String,
    },
}

impl fmt::Display for ManifestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ManifestError::Io { path, message } => {
                write!(f, "failed to read {}: {message}", path.display())
            }
            ManifestError::Parse { path, message } => {
                write!(f, "malformed {}: {message}", path.display())
            }
            ManifestError::MissingField { path, field } => {
                write!(
                    f,
                    "malformed {}: missing required field `{field}`",
                    path.display()
                )
            }
            ManifestError::ReservedName { path, message } => {
                write!(f, "invalid {}: {message}", path.display())
            }
            ManifestError::ConflictingFiles { canonical, legacy } => write!(
                f,
                "conflicting package manifests: both {} and {} exist; keep only `{MANIFEST_FILENAME}`",
                canonical.display(),
                legacy.display()
            ),
            ManifestError::IncompatibleToolchain {
                path,
                required,
                current,
            } => write!(
                f,
                "incompatible {}: requires Arandu `{required}`, current toolchain is `{current}`",
                path.display()
            ),
        }
    }
}

impl std::error::Error for ManifestError {}

/// Salsa input for the project manifest.
///
/// `content_hash` is the BLAKE3 of the raw file bytes (hex). Any change to the
/// file — including whitespace or comments — updates the hash and invalidates
/// dependents. Field values are also inputs so queries can depend on `entry`
/// without re-parsing.
#[salsa::input]
pub struct ProjectManifest {
    #[returns(ref)]
    pub name: String,
    #[returns(ref)]
    pub version: String,
    #[returns(ref)]
    pub entry: String,
    /// BLAKE3-256 of raw `Arandu.toml` bytes, lowercase hex (64 chars).
    #[returns(ref)]
    pub content_hash: String,
    pub path: Arc<PathBuf>,
}

/// BLAKE3 hex of `bytes` (stable invalidation fingerprint).
#[must_use]
pub fn hash_manifest_bytes(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

/// Parse `Arandu.toml` text. Does **not** read the filesystem.
///
/// Complete TOML syntax with a strict Arandu-owned schema. The legacy three-key
/// shape remains readable during the migration window.
pub fn parse_manifest_str(path: &Path, text: &str) -> Result<ManifestData, ManifestError> {
    let value: toml::Value = toml::from_str(text).map_err(|error| ManifestError::Parse {
        path: path.to_path_buf(),
        message: error.to_string(),
    })?;
    let data = if value.get("schema").is_some()
        || value.get("package").is_some()
        || value.get("targets").is_some()
    {
        let manifest: ManifestSchema = value.try_into().map_err(|error| ManifestError::Parse {
            path: path.to_path_buf(),
            message: error.to_string(),
        })?;
        validate_schema(path, &manifest)?;
        let binary_target = manifest.targets.bin.map(ManifestTarget::from);
        let library_target = manifest.targets.lib.map(ManifestTarget::from);
        let Some(primary_target) = binary_target.as_ref().or(library_target.as_ref()) else {
            return Err(ManifestError::MissingField {
                path: path.to_path_buf(),
                field: "targets.bin or targets.lib",
            });
        };
        let primary_root = primary_target.root.clone();
        let kind = match (binary_target.is_some(), library_target.is_some()) {
            (true, false) => PackageKind::Binary,
            (false, true) => PackageKind::Library,
            (true, true) => PackageKind::Mixed,
            (false, false) => {
                return Err(ManifestError::MissingField {
                    path: path.to_path_buf(),
                    field: "targets.bin or targets.lib",
                });
            }
        };
        ManifestData {
            name: manifest.package.name,
            version: manifest.package.version,
            entry: primary_root,
            schema: manifest.schema,
            edition: ManifestEdition::V2026,
            kind,
            toolchain_requirement: manifest.toolchain.map(|toolchain| toolchain.arandu),
            binary_target,
            library_target,
            capabilities: CapabilityPolicy {
                network: manifest.capabilities.network,
                filesystem_read: manifest.capabilities.filesystem_read,
                filesystem_write: manifest.capabilities.filesystem_write,
                environment: manifest.capabilities.environment,
                process: manifest.capabilities.process,
                foreign: manifest.capabilities.foreign,
            },
            effect_policy: manifest
                .policy
                .effects
                .map(|effects| EffectPolicy {
                    deny_new_authority: effects.deny_new_authority,
                    warn_new_resources: effects.warn_new_resources,
                    deny: effects.deny,
                })
                .unwrap_or_default(),
        }
    } else {
        for field in ["name", "version", "entry"] {
            if value.get(field).is_none() {
                return Err(ManifestError::MissingField {
                    path: path.to_path_buf(),
                    field,
                });
            }
        }
        let legacy: LegacyManifest = value.try_into().map_err(|error| ManifestError::Parse {
            path: path.to_path_buf(),
            message: error.to_string(),
        })?;
        ManifestData::legacy(legacy.name, legacy.version, legacy.entry)
    };

    if data.name.is_empty() {
        return Err(ManifestError::Parse {
            path: path.to_path_buf(),
            message: "`name` must be non-empty".into(),
        });
    }
    if data.entry.is_empty() {
        return Err(ManifestError::Parse {
            path: path.to_path_buf(),
            message: "`entry` must be non-empty".into(),
        });
    }
    semver::Version::parse(&data.version).map_err(|error| ManifestError::Parse {
        path: path.to_path_buf(),
        message: format!("`package.version` is not valid SemVer: {error}"),
    })?;
    validate_relative_path(path, &data.entry, "target root", false)?;
    // PROMOTE-L2: package name must not collide with stdlib roots.
    if let Err(e) = crate::vfs::validate_package_name(&data.name) {
        return Err(ManifestError::ReservedName {
            path: path.to_path_buf(),
            message: e.to_string(),
        });
    }

    Ok(data)
}

impl From<TargetSection> for ManifestTarget {
    fn from(target: TargetSection) -> Self {
        Self {
            name: target.name,
            root: target.root,
        }
    }
}

fn validate_schema(path: &Path, manifest: &ManifestSchema) -> Result<(), ManifestError> {
    if manifest.schema != 1 {
        return Err(ManifestError::Parse {
            path: path.to_path_buf(),
            message: format!("unsupported schema {}; expected 1", manifest.schema),
        });
    }
    if manifest.package.edition != "2026" {
        return Err(ManifestError::Parse {
            path: path.to_path_buf(),
            message: format!(
                "unsupported package edition `{}`; expected `2026`",
                manifest.package.edition
            ),
        });
    }
    if let Some(toolchain) = &manifest.toolchain {
        semver::VersionReq::parse(&toolchain.arandu).map_err(|error| ManifestError::Parse {
            path: path.to_path_buf(),
            message: format!("invalid `toolchain.arandu` requirement: {error}"),
        })?;
    }
    for target in [manifest.targets.bin.as_ref(), manifest.targets.lib.as_ref()]
        .into_iter()
        .flatten()
    {
        if target.name.is_empty() {
            return Err(ManifestError::Parse {
                path: path.to_path_buf(),
                message: "target `name` must be non-empty".into(),
            });
        }
        validate_relative_path(path, &target.root, "target root", false)?;
    }
    for (alias, dependency) in &manifest.dependencies {
        crate::vfs::validate_package_name(alias).map_err(|error| ManifestError::ReservedName {
            path: path.to_path_buf(),
            message: format!("invalid dependency alias `{alias}`: {error}"),
        })?;
        validate_relative_path(path, &dependency.path, "dependency path", true)?;
    }
    for (name, values) in [
        ("capabilities.network", &manifest.capabilities.network),
        (
            "capabilities.filesystem_read",
            &manifest.capabilities.filesystem_read,
        ),
        (
            "capabilities.filesystem_write",
            &manifest.capabilities.filesystem_write,
        ),
        (
            "capabilities.environment",
            &manifest.capabilities.environment,
        ),
        ("capabilities.process", &manifest.capabilities.process),
    ] {
        validate_policy_values(path, name, values)?;
    }
    if let Some(effects) = &manifest.policy.effects {
        validate_policy_values(path, "policy.effects.deny", &effects.deny)?;
        if effects.deny.iter().any(|effect| {
            !effect
                .chars()
                .next()
                .is_some_and(|first| first.is_ascii_uppercase())
        }) {
            return Err(ManifestError::Parse {
                path: path.to_path_buf(),
                message: "`policy.effects.deny` names must use PascalCase".into(),
            });
        }
    }
    let _ = &manifest.metadata;
    Ok(())
}

fn validate_policy_values(
    path: &Path,
    field: &str,
    values: &[String],
) -> Result<(), ManifestError> {
    let mut seen = std::collections::BTreeSet::new();
    for value in values {
        if value.is_empty() || !seen.insert(value) {
            return Err(ManifestError::Parse {
                path: path.to_path_buf(),
                message: format!("`{field}` contains an empty or duplicate value"),
            });
        }
    }
    Ok(())
}

pub fn ensure_toolchain_compatible(
    path: &Path,
    manifest: &ManifestData,
    current: &str,
) -> Result<(), ManifestError> {
    let Some(required) = &manifest.toolchain_requirement else {
        return Ok(());
    };
    let current_version =
        semver::Version::parse(current).map_err(|error| ManifestError::Parse {
            path: path.to_path_buf(),
            message: format!("compiler version `{current}` is not valid SemVer: {error}"),
        })?;
    let requirement =
        semver::VersionReq::parse(required).map_err(|error| ManifestError::Parse {
            path: path.to_path_buf(),
            message: format!("invalid `toolchain.arandu` requirement: {error}"),
        })?;
    if requirement.matches(&current_version) {
        Ok(())
    } else {
        Err(ManifestError::IncompatibleToolchain {
            path: path.to_path_buf(),
            required: required.clone(),
            current: current.to_owned(),
        })
    }
}

fn validate_relative_path(
    manifest_path: &Path,
    value: &str,
    field: &str,
    allow_parent: bool,
) -> Result<(), ManifestError> {
    let candidate = Path::new(value);
    let bytes = value.as_bytes();
    let windows_drive = bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':';
    let rooted = value.starts_with('/') || value.starts_with("//") || windows_drive;
    let has_parent = candidate
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir));
    if value.is_empty()
        || candidate.is_absolute()
        || rooted
        || (!allow_parent && has_parent)
        || value.contains('\\')
    {
        return Err(ManifestError::Parse {
            path: manifest_path.to_path_buf(),
            message: format!(
                "unsafe `{field}` `{value}`; use a non-empty relative path with `/` separators"
            ),
        });
    }
    Ok(())
}

/// Read and parse `Arandu.toml` at `path`. Propagates I/O and parse errors.
pub fn load_manifest(path: &Path) -> Result<(ManifestData, String, Vec<u8>), ManifestError> {
    let bytes = std::fs::read(path).map_err(|e| ManifestError::Io {
        path: path.to_path_buf(),
        message: e.to_string(),
    })?;
    let text = String::from_utf8(bytes.clone()).map_err(|e| ManifestError::Parse {
        path: path.to_path_buf(),
        message: format!("file is not valid UTF-8: {e}"),
    })?;
    let data = parse_manifest_str(path, &text)?;
    let hash = hash_manifest_bytes(&bytes);
    Ok((data, hash, bytes))
}

/// Register a loaded manifest as a Salsa input on `db`.
pub fn register_manifest(
    db: &dyn salsa::Database,
    path: PathBuf,
    data: ManifestData,
    content_hash: String,
) -> ProjectManifest {
    ProjectManifest::new(
        db,
        data.name,
        data.version,
        data.entry,
        content_hash,
        Arc::new(path),
    )
}

/// Walk parents of `start` looking for `arandu.toml` (project discovery).
///
/// This is **not** stdlib resolution — package roots may use cwd/path walk
/// (Cargo convention). Stdlib uses [`crate::stdlib::resolve_stdlib_root`].
pub fn find_manifest(start: &Path) -> Result<Option<ManifestDiscovery>, ManifestError> {
    let mut current = if start.is_file() {
        let Some(parent) = start.parent() else {
            return Ok(None);
        };
        parent.to_path_buf()
    } else {
        start.to_path_buf()
    };
    // Normalize to absolute when possible so relative starts still walk.
    if let Ok(abs) = std::fs::canonicalize(&current) {
        current = abs;
    }
    loop {
        if let Some(discovery) = discover_manifest_in(&current)? {
            return Ok(Some(discovery));
        }
        if !current.pop() {
            break;
        }
    }
    Ok(None)
}

fn discover_manifest_in(directory: &Path) -> Result<Option<ManifestDiscovery>, ManifestError> {
    let entries = std::fs::read_dir(directory).map_err(|error| ManifestError::Io {
        path: directory.to_path_buf(),
        message: error.to_string(),
    })?;
    let mut canonical = None;
    let mut legacy = None;
    for entry in entries {
        let entry = entry.map_err(|error| ManifestError::Io {
            path: directory.to_path_buf(),
            message: error.to_string(),
        })?;
        if entry.file_name() == MANIFEST_FILENAME && entry.path().is_file() {
            canonical = Some(entry.path());
        } else if entry.file_name() == LEGACY_MANIFEST_FILENAME && entry.path().is_file() {
            legacy = Some(entry.path());
        }
    }
    match (canonical, legacy) {
        (Some(canonical), Some(legacy)) => {
            let same_file = matches!(
                (
                    std::fs::canonicalize(&canonical),
                    std::fs::canonicalize(&legacy)
                ),
                (Ok(canonical_real), Ok(legacy_real)) if canonical_real == legacy_real
            );
            if same_file {
                Ok(Some(ManifestDiscovery {
                    path: canonical,
                    spelling: ManifestSpelling::Canonical,
                }))
            } else {
                Err(ManifestError::ConflictingFiles { canonical, legacy })
            }
        }
        (Some(path), None) => Ok(Some(ManifestDiscovery {
            path,
            spelling: ManifestSpelling::Canonical,
        })),
        (None, Some(path)) => Ok(Some(ManifestDiscovery {
            path,
            spelling: ManifestSpelling::Legacy,
        })),
        (None, None) => Ok(None),
    }
}

/// Tracked helper so dependents can pin work to the manifest fingerprint.
///
/// Exists primarily so the Salsa graph records the input edge from day 1
/// (even while the CLI still drives entry selection).
#[salsa::tracked]
pub fn manifest_fingerprint(db: &dyn crate::db::ArandCompilerDb, m: ProjectManifest) -> String {
    // Include fields + hash so any change shows up in explain-rebuild keys.
    format!(
        "{}@{}:{}#{}",
        m.name(db),
        m.version(db),
        m.entry(db),
        m.content_hash(db)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_happy_path() {
        let text = r#"
# comment
name = "hello"
version = "0.0.1"
entry = "src/main.aru"
"#;
        let data = parse_manifest_str(Path::new("Arandu.toml"), text).unwrap();
        assert_eq!(data.name, "hello");
        assert_eq!(data.version, "0.0.1");
        assert_eq!(data.entry, "src/main.aru");
    }

    #[test]
    fn parse_schema_one_with_path_dependency() {
        let text = r#"
schema = 1

[package]
name = "hello"
version = "1.2.3"
edition = "2026"

[toolchain]
arandu = ">=0.1.0-rc.4, <0.2.0"

[targets.bin]
name = "hello"
root = "src/main.aru"

[dependencies]
math = { path = "../math" }

[metadata.example]
note = "preserved for third-party tools"
"#;
        let data = parse_manifest_str(Path::new("arandu.toml"), text).unwrap();
        assert_eq!(data.name, "hello");
        assert_eq!(data.version, "1.2.3");
        assert_eq!(data.entry, "src/main.aru");
        assert_eq!(data.schema, 1);
        assert_eq!(data.edition, ManifestEdition::V2026);
        assert_eq!(data.kind, PackageKind::Binary);
        assert_eq!(
            data.toolchain_requirement.as_deref(),
            Some(">=0.1.0-rc.4, <0.2.0")
        );
    }

    #[test]
    fn mixed_package_prefers_binary_entry_and_preserves_library_target() {
        let text = r#"
schema = 1
[package]
name = "mixed"
version = "1.0.0"
edition = "2026"
[targets.lib]
name = "mixed"
root = "src/lib.aru"
[targets.bin]
name = "mixed-cli"
root = "src/main.aru"
"#;
        let data = parse_manifest_str(Path::new("arandu.toml"), text).unwrap();
        assert_eq!(data.kind, PackageKind::Mixed);
        assert_eq!(data.entry, "src/main.aru");
        assert_eq!(data.library_target.unwrap().root, "src/lib.aru");
    }

    #[test]
    fn toolchain_requirement_is_enforced_against_explicit_version() {
        let text = r#"
schema = 1
[package]
name = "future"
version = "1.0.0"
edition = "2026"
[toolchain]
arandu = ">=9.0.0"
[targets.lib]
name = "future"
root = "src/lib.aru"
"#;
        let data = parse_manifest_str(Path::new("arandu.toml"), text).unwrap();
        let error =
            ensure_toolchain_compatible(Path::new("arandu.toml"), &data, "0.1.0-rc.4").unwrap_err();
        assert!(matches!(error, ManifestError::IncompatibleToolchain { .. }));
        assert!(ensure_toolchain_compatible(Path::new("arandu.toml"), &data, "9.1.0").is_ok());
    }

    #[test]
    fn capability_and_effect_policy_are_typed_but_not_claimed_as_inferred() {
        let text = r#"
schema = 1
[package]
name = "server"
version = "1.0.0"
edition = "2026"
[targets.bin]
name = "server"
root = "src/main.aru"
[capabilities]
network = ["api.example.com:443"]
filesystem_read = ["assets/**"]
foreign = false
[policy.effects]
deny_new_authority = true
warn_new_resources = true
deny = ["UnknownCapability"]
"#;
        let data = parse_manifest_str(Path::new("arandu.toml"), text).unwrap();
        assert_eq!(data.capabilities.network, ["api.example.com:443"]);
        assert_eq!(data.capabilities.filesystem_read, ["assets/**"]);
        assert!(!data.capabilities.foreign);
        assert_eq!(data.effect_policy.deny, ["UnknownCapability"]);
    }

    #[test]
    fn parse_rejects_unknown_owned_field() {
        let text = r#"
schema = 1
[package]
name = "hello"
version = "1.2.3"
edition = "2026"
surprise = true
[targets.bin]
name = "hello"
root = "src/main.aru"
"#;
        let err = parse_manifest_str(Path::new("arandu.toml"), text).unwrap_err();
        assert!(err.to_string().contains("unknown field `surprise`"));
    }

    #[test]
    fn parse_rejects_duplicate_keys_and_host_foreign_absolute_paths() {
        let duplicate = r#"
schema = 1
schema = 1
[package]
name = "hello"
version = "1.0.0"
edition = "2026"
[targets.bin]
name = "hello"
root = "src/main.aru"
"#;
        assert!(matches!(
            parse_manifest_str(Path::new("arandu.toml"), duplicate),
            Err(ManifestError::Parse { .. })
        ));

        let windows_absolute = duplicate
            .replacen("schema = 1\nschema = 1", "schema = 1", 1)
            .replace("src/main.aru", "C:/project/main.aru");
        assert!(
            parse_manifest_str(Path::new("arandu.toml"), &windows_absolute)
                .unwrap_err()
                .to_string()
                .contains("unsafe `target root`")
        );
    }

    #[test]
    fn parse_rejects_invalid_semver_and_unsafe_target_path() {
        let invalid_version = r#"
schema = 1
[package]
name = "hello"
version = "latest"
edition = "2026"
[targets.bin]
name = "hello"
root = "src/main.aru"
"#;
        assert!(
            parse_manifest_str(Path::new("arandu.toml"), invalid_version)
                .unwrap_err()
                .to_string()
                .contains("not valid SemVer")
        );

        let escaping_root = invalid_version
            .replace("latest", "1.0.0")
            .replace("src/main.aru", "../main.aru");
        assert!(parse_manifest_str(Path::new("arandu.toml"), &escaping_root)
            .unwrap_err()
            .to_string()
            .contains("unsafe `target root`"));
    }

    #[test]
    fn parse_missing_entry_errors() {
        let text = r#"name = "x"
version = "1.0.0"
"#;
        let err = parse_manifest_str(Path::new("Arandu.toml"), text).unwrap_err();
        assert!(matches!(
            err,
            ManifestError::MissingField { field: "entry", .. }
        ));
    }

    #[test]
    fn parse_malformed_line_errors() {
        let text = "name = hello\n";
        let err = parse_manifest_str(Path::new("Arandu.toml"), text).unwrap_err();
        assert!(matches!(err, ManifestError::Parse { .. }));
    }

    #[test]
    fn parse_rejects_tables() {
        let text = r#"
name = "x"
version = "0.0.1"
entry = "src/main.aru"
[dependencies]
"#;
        let err = parse_manifest_str(Path::new("Arandu.toml"), text).unwrap_err();
        assert!(matches!(err, ManifestError::Parse { .. }));
    }

    #[test]
    fn content_hash_stable() {
        let a = hash_manifest_bytes(b"name = \"a\"\n");
        let b = hash_manifest_bytes(b"name = \"a\"\n");
        let c = hash_manifest_bytes(b"name = \"b\"\n");
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_eq!(a.len(), 64);
    }

    #[test]
    fn discovery_prefers_canonical_and_classifies_legacy() {
        let root = test_directory("manifest_discovery");
        std::fs::write(root.join(LEGACY_MANIFEST_FILENAME), "legacy").unwrap();
        let legacy = find_manifest(&root).unwrap().unwrap();
        assert_eq!(legacy.spelling, ManifestSpelling::Legacy);

        std::fs::write(root.join(MANIFEST_FILENAME), "canonical").unwrap();
        let exact_names = std::fs::read_dir(&root)
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.file_name())
            .collect::<Vec<_>>();
        let has_both = exact_names.iter().any(|name| name == MANIFEST_FILENAME)
            && exact_names
                .iter()
                .any(|name| name == LEGACY_MANIFEST_FILENAME);
        if has_both {
            assert!(matches!(
                find_manifest(&root),
                Err(ManifestError::ConflictingFiles { .. })
            ));
        } else {
            let discovery = find_manifest(&root).unwrap().unwrap();
            let expected = if exact_names.iter().any(|name| name == MANIFEST_FILENAME) {
                ManifestSpelling::Canonical
            } else {
                ManifestSpelling::Legacy
            };
            assert_eq!(discovery.spelling, expected);
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    fn test_directory(prefix: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "arandu-{prefix}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        root
    }
}
