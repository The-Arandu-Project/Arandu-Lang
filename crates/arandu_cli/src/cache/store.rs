//! Immutable package cache storage, publishing, and quarantine under entry locks.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use arandu_query::{CacheDigest, CacheLayout};

use super::ops::{scan_archives, scan_transient};
use super::tree::hash_tree;
use super::types::{
    CacheInspect, CachePrune, CachePublish, CacheScanLimits, CacheStoreError, CacheVerify,
    TreeLimits, TreeVerification,
};

/// Filesystem owner for immutable package cache objects.
#[derive(Debug, Clone)]
pub struct CacheStore {
    layout: CacheLayout,
}

impl CacheStore {
    pub fn new(layout: CacheLayout) -> Self {
        Self { layout }
    }

    /// Publish an archive only when its bytes match the expected identity.
    pub fn publish_archive(
        &self,
        expected: CacheDigest,
        bytes: &[u8],
    ) -> Result<CachePublish, CacheStoreError> {
        let actual = CacheDigest::sha256(bytes);
        if actual != expected {
            return Err(CacheStoreError::DigestMismatch { expected, actual });
        }

        let destination = self.layout.archive(expected);
        let lock_path = self.layout.entry_lock(expected);
        create_parent(&lock_path)?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .map_err(|error| CacheStoreError::io("open cache entry lock", &lock_path, error))?;
        lock.lock()
            .map_err(|error| CacheStoreError::io("lock cache entry", &lock_path, error))?;

        let repaired = match fs::symlink_metadata(&destination) {
            Ok(metadata) if is_link_like(&metadata) || !metadata.file_type().is_file() => {
                self.quarantine_corrupt(&destination, expected)?;
                true
            }
            Ok(_) => match fs::read(&destination) {
                Ok(existing) if CacheDigest::sha256(&existing) == expected => {
                    return Ok(CachePublish::AlreadyPresent);
                }
                Ok(_) => {
                    self.quarantine_corrupt(&destination, expected)?;
                    true
                }
                Err(error) => {
                    return Err(CacheStoreError::io(
                        "read cached archive",
                        &destination,
                        error,
                    ));
                }
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(error) => {
                return Err(CacheStoreError::io(
                    "read cached archive",
                    &destination,
                    error,
                ));
            }
        };

        create_parent(&destination)?;
        let staging_root = self.layout.staging();
        fs::create_dir_all(&staging_root).map_err(|error| {
            CacheStoreError::io("create cache staging directory", &staging_root, error)
        })?;
        let staging = self.unique_staging_path(expected);
        write_new_synced(&staging, bytes)?;
        if let Err(error) = fs::rename(&staging, &destination) {
            let _ = fs::remove_file(&staging);
            return Err(CacheStoreError::io(
                "publish cached archive",
                &destination,
                error,
            ));
        }

        Ok(if repaired {
            CachePublish::Repaired
        } else {
            CachePublish::Added
        })
    }

    /// Inspect recognized cache namespaces without hashing archive contents.
    pub fn inspect(&self, limits: CacheScanLimits) -> Result<CacheInspect, CacheStoreError> {
        let archives = scan_archives(&self.layout, limits, false)?;
        let (staging_files, _) = scan_transient(&self.layout.staging(), limits, false)?;
        let (quarantine_files, _) = scan_transient(&self.layout.quarantine(), limits, false)?;
        Ok(CacheInspect {
            archives: archives.valid,
            archive_bytes: archives.bytes,
            invalid_entries: archives.invalid,
            staging_files,
            quarantine_files,
        })
    }

    /// Re-hash every recognized archive within the caller-provided limits.
    pub fn verify(&self, limits: CacheScanLimits) -> Result<CacheVerify, CacheStoreError> {
        let archives = scan_archives(&self.layout, limits, true)?;
        Ok(CacheVerify {
            verified: archives.valid.saturating_sub(archives.corrupt),
            verified_bytes: archives.bytes,
            corrupt: archives.corrupt,
            invalid_entries: archives.invalid,
        })
    }

    /// Remove only transient staging/quarantine files, never valid archives.
    pub fn prune(
        &self,
        limits: CacheScanLimits,
        dry_run: bool,
    ) -> Result<CachePrune, CacheStoreError> {
        let (staging_files, staging_bytes) =
            scan_transient(&self.layout.staging(), limits, !dry_run)?;
        let remaining_entries = limits.max_entries.saturating_sub(staging_files);
        let remaining_bytes = limits.max_bytes.saturating_sub(staging_bytes);
        let (quarantine_files, quarantine_bytes) = scan_transient(
            &self.layout.quarantine(),
            CacheScanLimits {
                max_entries: remaining_entries,
                max_bytes: remaining_bytes,
            },
            !dry_run,
        )?;
        Ok(CachePrune {
            files: staging_files.saturating_add(quarantine_files),
            bytes: staging_bytes.saturating_add(quarantine_bytes),
            dry_run,
        })
    }

    /// Recompute an extracted tree digest before it can be trusted.
    pub fn verify_tree(
        &self,
        archive_digest: CacheDigest,
        expected_tree_digest: CacheDigest,
        limits: TreeLimits,
    ) -> Result<TreeVerification, CacheStoreError> {
        let tree = self.layout.tree(archive_digest);
        let actual = hash_tree(&tree, limits)?;
        if actual.digest != expected_tree_digest {
            return Err(CacheStoreError::DigestMismatch {
                expected: expected_tree_digest,
                actual: actual.digest,
            });
        }
        Ok(actual)
    }

    /// Verify an already copied tree without requiring an archive namespace.
    pub fn verify_tree_path(
        &self,
        path: &Path,
        limits: TreeLimits,
    ) -> Result<TreeVerification, CacheStoreError> {
        hash_tree(path, limits)
    }

    /// Hash a staging directory and publish it under its content identity.
    ///
    /// The staging directory must live below this cache's staging namespace so
    /// the final rename stays on one filesystem. Symlinks and special files are
    /// rejected by the canonical tree verifier before anything becomes visible
    /// as a cache hit.
    pub fn publish_tree(
        &self,
        staging: &Path,
        limits: TreeLimits,
    ) -> Result<(CachePublish, TreeVerification, PathBuf), CacheStoreError> {
        let staging_root = self.layout.staging();
        let canonical_staging_root = fs::canonicalize(&staging_root).map_err(|error| {
            CacheStoreError::io("canonicalize cache staging directory", &staging_root, error)
        })?;
        let canonical_staging = fs::canonicalize(staging)
            .map_err(|error| CacheStoreError::io("canonicalize staged tree", staging, error))?;
        if canonical_staging == canonical_staging_root
            || !canonical_staging.starts_with(&canonical_staging_root)
        {
            return Err(CacheStoreError::MalformedCache(format!(
                "staged tree {} is outside cache staging {}",
                staging.display(),
                staging_root.display()
            )));
        }

        let verification = hash_tree(&canonical_staging, limits)?;
        let destination = self.layout.tree(verification.digest);
        let lock_path = self.layout.entry_lock(verification.digest);
        create_parent(&lock_path)?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .map_err(|error| CacheStoreError::io("open cache entry lock", &lock_path, error))?;
        lock.lock()
            .map_err(|error| CacheStoreError::io("lock cache entry", &lock_path, error))?;

        let repaired = if destination.exists() {
            let existing = hash_tree(&destination, limits)?;
            if existing.digest == verification.digest {
                fs::remove_dir_all(&canonical_staging).map_err(|error| {
                    CacheStoreError::io("remove redundant staged tree", &canonical_staging, error)
                })?;
                return Ok((CachePublish::AlreadyPresent, existing, destination));
            }
            self.quarantine_corrupt(&destination, verification.digest)?;
            true
        } else {
            false
        };

        create_parent(&destination)?;
        fs::rename(&canonical_staging, &destination)
            .map_err(|error| CacheStoreError::io("publish cached tree", &destination, error))?;
        Ok((
            if repaired {
                CachePublish::Repaired
            } else {
                CachePublish::Added
            },
            verification,
            destination,
        ))
    }

    /// Revalidate and return a content-addressed tree.
    pub fn trusted_tree(
        &self,
        digest: CacheDigest,
        limits: TreeLimits,
    ) -> Result<PathBuf, CacheStoreError> {
        let tree = self.layout.tree(digest);
        let actual = hash_tree(&tree, limits)?;
        if actual.digest != digest {
            return Err(CacheStoreError::DigestMismatch {
                expected: digest,
                actual: actual.digest,
            });
        }
        Ok(tree)
    }

    pub fn layout(&self) -> &CacheLayout {
        &self.layout
    }

    fn quarantine_corrupt(&self, path: &Path, digest: CacheDigest) -> Result<(), CacheStoreError> {
        let quarantine = self.layout.quarantine();
        fs::create_dir_all(&quarantine)
            .map_err(|error| CacheStoreError::io("create cache quarantine", &quarantine, error))?;
        let destination = quarantine.join(format!(
            "{}-{}-{}.corrupt",
            digest.hex(),
            std::process::id(),
            nonce()
        ));
        fs::rename(path, &destination)
            .map_err(|error| CacheStoreError::io("quarantine cached archive", path, error))
    }

    fn unique_staging_path(&self, digest: CacheDigest) -> PathBuf {
        self.layout.staging().join(format!(
            "archive-{}-{}-{}.tmp",
            digest.hex(),
            std::process::id(),
            nonce()
        ))
    }
}

pub(crate) fn create_parent(path: &Path) -> Result<(), CacheStoreError> {
    let parent = path.parent().ok_or_else(|| {
        CacheStoreError::io(
            "resolve cache parent",
            path,
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "path has no parent"),
        )
    })?;
    fs::create_dir_all(parent)
        .map_err(|error| CacheStoreError::io("create cache directory", parent, error))
}

fn write_new_synced(path: &Path, bytes: &[u8]) -> Result<(), CacheStoreError> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| CacheStoreError::io("create cache staging file", path, error))?;
    if let Err(error) = file.write_all(bytes).and_then(|()| file.sync_all()) {
        let _ = fs::remove_file(path);
        return Err(CacheStoreError::io("flush cache staging file", path, error));
    }
    Ok(())
}

pub(crate) fn nonce() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos())
}

pub(crate) fn is_plain_directory(path: &Path) -> Result<bool, CacheStoreError> {
    fs::symlink_metadata(path)
        .map(|metadata| !is_link_like(&metadata) && metadata.file_type().is_dir())
        .map_err(|error| CacheStoreError::io("inspect cache directory", path, error))
}

pub(crate) fn is_link_like(metadata: &fs::Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt as _;
        use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;

        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    {
        false
    }
}

pub(crate) fn is_lower_hex(value: &str) -> bool {
    value
        .bytes()
        .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}
