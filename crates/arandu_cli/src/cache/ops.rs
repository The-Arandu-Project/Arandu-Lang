//! Cache scanning, integrity verification, and transient pruning operations.

use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};

use arandu_query::{CacheDigest, CacheLayout};
use sha2::{Digest as _, Sha256};

use super::store::{is_lower_hex, is_plain_directory};
use super::types::{CacheScanLimits, CacheStoreError, validate_limits};

#[derive(Debug, Default)]
pub(crate) struct ArchiveScan {
    pub(crate) scanned: usize,
    pub(crate) valid: usize,
    pub(crate) corrupt: usize,
    pub(crate) invalid: usize,
    pub(crate) bytes: u64,
}

pub(crate) fn scan_archives(
    layout: &CacheLayout,
    limits: CacheScanLimits,
    verify: bool,
) -> Result<ArchiveScan, CacheStoreError> {
    validate_limits(limits)?;
    let root = layout.root().join("v1/archives/sha256");
    let mut scan = ArchiveScan::default();
    for fanout in sorted_entries(&root, limits.max_entries)? {
        consume_entry(&mut scan, limits)?;
        if !is_plain_directory(&fanout)? {
            scan.invalid = scan.invalid.saturating_add(1);
            continue;
        }
        let prefix = fanout
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("");
        if prefix.len() != 2 || !is_lower_hex(prefix) {
            scan.invalid = scan.invalid.saturating_add(1);
            continue;
        }
        let remaining = limits.max_entries.saturating_sub(scan.scanned);
        for archive in sorted_entries(&fanout, remaining)? {
            consume_entry(&mut scan, limits)?;
            let metadata = fs::symlink_metadata(&archive)
                .map_err(|error| CacheStoreError::io("inspect cache archive", &archive, error))?;
            if !metadata.file_type().is_file() {
                scan.invalid = scan.invalid.saturating_add(1);
                continue;
            }
            let Some(suffix) = archive.file_name().and_then(|name| name.to_str()) else {
                scan.invalid = scan.invalid.saturating_add(1);
                continue;
            };
            let Some(tail) = suffix.strip_suffix(".tar.zst") else {
                scan.invalid = scan.invalid.saturating_add(1);
                continue;
            };
            let encoded = format!("{prefix}{tail}");
            if encoded.len() != 64 || !is_lower_hex(&encoded) {
                scan.invalid = scan.invalid.saturating_add(1);
                continue;
            }
            let expected: CacheDigest = format!("sha256:{encoded}").parse().map_err(
                |error: arandu_query::CacheDigestError| {
                    CacheStoreError::MalformedCache(error.to_string())
                },
            )?;
            scan.bytes = scan.bytes.checked_add(metadata.len()).ok_or_else(|| {
                CacheStoreError::LimitExceeded("cache byte count overflowed".to_string())
            })?;
            if scan.bytes > limits.max_bytes {
                return Err(CacheStoreError::LimitExceeded(format!(
                    "cache scan exceeded {} bytes",
                    limits.max_bytes
                )));
            }
            scan.valid = scan.valid.saturating_add(1);
            if verify && hash_file_bounded(&archive, metadata.len())? != expected {
                scan.corrupt = scan.corrupt.saturating_add(1);
            }
        }
    }
    Ok(scan)
}

fn consume_entry(scan: &mut ArchiveScan, limits: CacheScanLimits) -> Result<(), CacheStoreError> {
    if scan.scanned >= limits.max_entries {
        return Err(CacheStoreError::LimitExceeded(format!(
            "cache scan exceeded {} entries",
            limits.max_entries
        )));
    }
    scan.scanned = scan.scanned.saturating_add(1);
    Ok(())
}

pub(crate) fn scan_transient(
    root: &Path,
    limits: CacheScanLimits,
    remove: bool,
) -> Result<(usize, u64), CacheStoreError> {
    validate_limits(limits)?;
    let mut files = 0_usize;
    let mut bytes = 0_u64;
    let mut seen = 0_usize;
    for path in sorted_entries(root, limits.max_entries)? {
        seen = seen.saturating_add(1);
        if seen > limits.max_entries {
            return Err(CacheStoreError::LimitExceeded(
                "cache transient entry limit exceeded".to_string(),
            ));
        }
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| CacheStoreError::io("inspect transient cache entry", &path, error))?;
        if !metadata.file_type().is_file() {
            continue;
        }
        bytes = bytes.checked_add(metadata.len()).ok_or_else(|| {
            CacheStoreError::LimitExceeded("cache prune byte count overflowed".to_string())
        })?;
        if bytes > limits.max_bytes {
            return Err(CacheStoreError::LimitExceeded(format!(
                "cache prune exceeded {} bytes",
                limits.max_bytes
            )));
        }
        files = files.saturating_add(1);
        if remove {
            fs::remove_file(&path).map_err(|error| {
                CacheStoreError::io("prune transient cache entry", &path, error)
            })?;
        }
    }
    Ok((files, bytes))
}

pub(crate) fn sorted_entries(
    root: &Path,
    max_entries: usize,
) -> Result<Vec<PathBuf>, CacheStoreError> {
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(CacheStoreError::io("read cache directory", root, error)),
    };
    let mut paths = Vec::new();
    for entry in entries {
        if paths.len() >= max_entries {
            return Err(CacheStoreError::LimitExceeded(format!(
                "cache directory {} exceeded {max_entries} entries",
                root.display()
            )));
        }
        paths.push(
            entry
                .map(|entry| entry.path())
                .map_err(|error| CacheStoreError::io("read cache directory entry", root, error))?,
        );
    }
    paths.sort();
    Ok(paths)
}

fn hash_file_bounded(path: &Path, expected_len: u64) -> Result<CacheDigest, CacheStoreError> {
    let mut file = File::open(path)
        .map_err(|error| CacheStoreError::io("open cached archive", path, error))?;
    let mut hasher = Sha256::new();
    let mut remaining = expected_len;
    let mut buffer = [0_u8; 32 * 1024];
    while remaining > 0 {
        let requested = usize::try_from(remaining)
            .unwrap_or(buffer.len())
            .min(buffer.len());
        let read = file
            .read(&mut buffer[..requested])
            .map_err(|error| CacheStoreError::io("hash cached archive", path, error))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        remaining = remaining.saturating_sub(u64::try_from(read).unwrap_or(0));
    }
    let mut extra = [0_u8; 1];
    let has_extra = file
        .read(&mut extra)
        .map_err(|error| CacheStoreError::io("hash cached archive", path, error))?
        != 0;
    if remaining != 0 || has_extra {
        return Err(CacheStoreError::MalformedCache(format!(
            "archive changed size while verifying {}",
            path.display()
        )));
    }
    Ok(CacheDigest::from_bytes(hasher.finalize().into()))
}
