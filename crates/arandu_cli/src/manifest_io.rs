//! Manifest filesystem discovery and reading for CLI and LSP frontends.
//!
//! Separated from `arandu_query` so Salsa queries remain purely in-memory
//! and deterministic without direct filesystem side-effects.

use arandu_query::{
    LEGACY_MANIFEST_FILENAME, MANIFEST_FILENAME, ManifestData, ManifestDiscovery, ManifestError,
    ManifestSpelling, hash_manifest_bytes, parse_manifest_bytes,
};
use std::fs;
use std::path::Path;

/// Read and parse `arandu.toml` at `path`. Propagates I/O and parse errors.
pub fn load_manifest(path: &Path) -> Result<(ManifestData, String, Vec<u8>), ManifestError> {
    let bytes = fs::read(path).map_err(|e| ManifestError::Io {
        path: path.to_path_buf(),
        message: e.to_string(),
    })?;
    let data = parse_manifest_bytes(path, &bytes)?;
    let hash = hash_manifest_bytes(&bytes);
    Ok((data, hash, bytes))
}

/// Walk parents of `start` looking for `arandu.toml` (project discovery).
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
    if let Ok(abs) = fs::canonicalize(&current) {
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
    let entries = fs::read_dir(directory).map_err(|error| ManifestError::Io {
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
                (fs::canonicalize(&canonical), fs::canonicalize(&legacy)),
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn discovery_prefers_canonical_and_classifies_legacy() {
        let root = test_directory("manifest_discovery");
        fs::write(
            root.join(LEGACY_MANIFEST_FILENAME),
            "name='a'\nversion='0.1.0'\nentry='src/main.aru'\n",
        )
        .unwrap();
        let legacy = find_manifest(&root).unwrap().unwrap();
        assert_eq!(legacy.spelling, ManifestSpelling::Legacy);

        fs::write(
            root.join(MANIFEST_FILENAME),
            "name='a'\nversion='0.1.0'\nentry='src/main.aru'\n",
        )
        .unwrap();
        let exact_names = fs::read_dir(&root)
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
        fs::remove_dir_all(root).unwrap();
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
        fs::create_dir_all(&root).unwrap();
        root
    }
}
