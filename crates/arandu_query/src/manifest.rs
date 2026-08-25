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
    let (name, version, entry) = if value.get("schema").is_some()
        || value.get("package").is_some()
        || value.get("targets").is_some()
    {
        let manifest: ManifestSchema = value.try_into().map_err(|error| ManifestError::Parse {
            path: path.to_path_buf(),
            message: error.to_string(),
        })?;
        validate_schema(path, &manifest)?;
        let target = manifest
            .targets
            .bin
            .or(manifest.targets.lib)
            .ok_or_else(|| ManifestError::MissingField {
                path: path.to_path_buf(),
                field: "targets.bin or targets.lib",
            })?;
        (manifest.package.name, manifest.package.version, target.root)
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
        (legacy.name, legacy.version, legacy.entry)
    };

    if name.is_empty() {
        return Err(ManifestError::Parse {
            path: path.to_path_buf(),
            message: "`name` must be non-empty".into(),
        });
    }
    if entry.is_empty() {
        return Err(ManifestError::Parse {
            path: path.to_path_buf(),
            message: "`entry` must be non-empty".into(),
        });
    }
    semver::Version::parse(&version).map_err(|error| ManifestError::Parse {
        path: path.to_path_buf(),
        message: format!("`package.version` is not valid SemVer: {error}"),
    })?;
    validate_relative_path(path, &entry, "target root", false)?;
    // PROMOTE-L2: package name must not collide with stdlib roots.
    if let Err(e) = crate::vfs::validate_package_name(&name) {
        return Err(ManifestError::ReservedName {
            path: path.to_path_buf(),
            message: e.to_string(),
        });
    }

    Ok(ManifestData {
        name,
        version,
        entry,
    })
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
    if manifest.targets.bin.is_some() && manifest.targets.lib.is_some() {
        return Err(ManifestError::Parse {
            path: path.to_path_buf(),
            message: "schema 1 accepts one target: choose `targets.bin` or `targets.lib`".into(),
        });
    }
    let target = manifest
        .targets
        .bin
        .as_ref()
        .or(manifest.targets.lib.as_ref());
    if let Some(target) = target {
        if target.name.is_empty() {
            return Err(ManifestError::Parse {
                path: path.to_path_buf(),
                message: "target `name` must be non-empty".into(),
            });
        }
    }
    for (alias, dependency) in &manifest.dependencies {
        crate::vfs::validate_package_name(alias).map_err(|error| ManifestError::ReservedName {
            path: path.to_path_buf(),
            message: format!("invalid dependency alias `{alias}`: {error}"),
        })?;
        validate_relative_path(path, &dependency.path, "dependency path", true)?;
    }
    let _ = &manifest.metadata;
    Ok(())
}

fn validate_relative_path(
    manifest_path: &Path,
    value: &str,
    field: &str,
    allow_parent: bool,
) -> Result<(), ManifestError> {
    let candidate = Path::new(value);
    let has_parent = candidate
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir));
    if value.is_empty()
        || candidate.is_absolute()
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
pub fn find_manifest(start: &Path) -> Option<PathBuf> {
    let mut current = if start.is_file() {
        start.parent()?.to_path_buf()
    } else {
        start.to_path_buf()
    };
    // Normalize to absolute when possible so relative starts still walk.
    if let Ok(abs) = std::fs::canonicalize(&current) {
        current = abs;
    }
    loop {
        let candidate = current.join(MANIFEST_FILENAME);
        if candidate.is_file() {
            return Some(candidate);
        }
        let legacy = current.join(LEGACY_MANIFEST_FILENAME);
        if legacy.is_file() {
            return Some(legacy);
        }
        if !current.pop() {
            break;
        }
    }
    None
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
}
