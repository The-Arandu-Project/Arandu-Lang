//! Types, limits, and error definitions for the package cache subsystem.

use std::fmt;
use std::path::{Path, PathBuf};

use arandu_query::CacheDigest;

pub const CACHE_DIR_ENV: &str = "ARANDU_CACHE_DIR";

pub const DEFAULT_SCAN_ENTRIES: usize = 100_000;
pub const DEFAULT_SCAN_BYTES: u64 = 16 * 1024 * 1024 * 1024;
pub const COPY_BUFFER_SIZE: usize = 32 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CacheScanLimits {
    pub max_entries: usize,
    pub max_bytes: u64,
}

impl Default for CacheScanLimits {
    fn default() -> Self {
        Self {
            max_entries: DEFAULT_SCAN_ENTRIES,
            max_bytes: DEFAULT_SCAN_BYTES,
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct CacheInspect {
    pub archives: usize,
    pub archive_bytes: u64,
    pub invalid_entries: usize,
    pub staging_files: usize,
    pub quarantine_files: usize,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct CacheVerify {
    pub verified: usize,
    pub verified_bytes: u64,
    pub corrupt: usize,
    pub invalid_entries: usize,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct CachePrune {
    pub files: usize,
    pub bytes: u64,
    pub dry_run: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TreeLimits {
    pub max_files: usize,
    pub max_bytes: u64,
    pub max_depth: usize,
}

impl Default for TreeLimits {
    fn default() -> Self {
        Self {
            max_files: 100_000,
            max_bytes: 16 * 1024 * 1024 * 1024,
            max_depth: 64,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TreeVerification {
    pub digest: CacheDigest,
    pub files: usize,
    pub bytes: u64,
    pub depth: usize,
}

pub fn parse_scan_flags(
    args: &[String],
    allow_dry_run: bool,
) -> Result<(CacheScanLimits, bool), String> {
    let mut limits = CacheScanLimits::default();
    let mut dry_run = false;
    for argument in args {
        if let Some(value) = argument.strip_prefix("--max-entries=") {
            limits.max_entries = value
                .parse()
                .map_err(|_| "--max-entries requires a positive integer".to_string())?;
        } else if let Some(value) = argument.strip_prefix("--max-bytes=") {
            limits.max_bytes = value
                .parse()
                .map_err(|_| "--max-bytes requires a positive integer".to_string())?;
        } else if argument == "--dry-run" && allow_dry_run {
            dry_run = true;
        } else {
            return Err(format!("unknown cache option `{argument}`"));
        }
    }
    if limits.max_entries == 0 || limits.max_bytes == 0 {
        return Err("cache scan limits must be greater than zero".to_string());
    }
    Ok((limits, dry_run))
}

/// Result of publishing a verified immutable cache object.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CachePublish {
    Added,
    AlreadyPresent,
    Repaired,
}

#[derive(Debug)]
pub enum CacheStoreError {
    DigestMismatch {
        expected: CacheDigest,
        actual: CacheDigest,
    },
    Io {
        operation: &'static str,
        path: PathBuf,
        source: std::io::Error,
    },
    LimitExceeded(String),
    MalformedCache(String),
}

impl CacheStoreError {
    pub(crate) fn io(operation: &'static str, path: &Path, source: std::io::Error) -> Self {
        Self::Io {
            operation,
            path: path.to_path_buf(),
            source,
        }
    }
}

impl fmt::Display for CacheStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DigestMismatch { expected, actual } => {
                write!(
                    formatter,
                    "cache digest mismatch: expected {expected}, found {actual}"
                )
            }
            Self::Io {
                operation,
                path,
                source,
            } => write!(formatter, "{operation} {}: {source}", path.display()),
            Self::LimitExceeded(message) | Self::MalformedCache(message) => {
                formatter.write_str(message)
            }
        }
    }
}

impl std::error::Error for CacheStoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

pub(crate) fn validate_limits(limits: CacheScanLimits) -> Result<(), CacheStoreError> {
    if limits.max_entries == 0 || limits.max_bytes == 0 {
        return Err(CacheStoreError::LimitExceeded(
            "cache limits must be greater than zero".to_string(),
        ));
    }
    Ok(())
}
