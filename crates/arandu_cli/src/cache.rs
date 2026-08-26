//! Platform-native cache-root discovery for package artifacts.

use std::env;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use arandu_query::{CacheDigest, CacheLayout};
use sha2::{Digest as _, Sha256};

pub const CACHE_DIR_ENV: &str = "ARANDU_CACHE_DIR";

pub const DEFAULT_SCAN_ENTRIES: usize = 100_000;
pub const DEFAULT_SCAN_BYTES: u64 = 16 * 1024 * 1024 * 1024;

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

        let repaired = match fs::read(&destination) {
            Ok(existing) if CacheDigest::sha256(&existing) == expected => {
                return Ok(CachePublish::AlreadyPresent);
            }
            Ok(_) => {
                self.quarantine_corrupt(&destination, expected)?;
                true
            }
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

fn hash_tree(root: &Path, limits: TreeLimits) -> Result<TreeVerification, CacheStoreError> {
    if limits.max_files == 0 || limits.max_bytes == 0 || limits.max_depth == 0 {
        return Err(CacheStoreError::LimitExceeded(
            "tree limits must be greater than zero".to_string(),
        ));
    }
    if !is_plain_directory(root)? {
        return Err(CacheStoreError::MalformedCache(format!(
            "extracted tree is not a directory: {}",
            root.display()
        )));
    }
    let mut scan = TreeScan {
        hasher: Sha256::new(),
        ..TreeScan::default()
    };
    walk_tree(root, Path::new(""), 0, limits, &mut scan)?;
    Ok(TreeVerification {
        digest: CacheDigest::from_bytes(scan.hasher.finalize().into()),
        files: scan.files,
        bytes: scan.bytes,
        depth: scan.depth,
    })
}

#[derive(Default)]
struct TreeScan {
    hasher: Sha256,
    files: usize,
    bytes: u64,
    depth: usize,
}

fn walk_tree(
    root: &Path,
    relative: &Path,
    depth: usize,
    limits: TreeLimits,
    scan: &mut TreeScan,
) -> Result<(), CacheStoreError> {
    if depth > limits.max_depth {
        return Err(CacheStoreError::LimitExceeded(format!(
            "extracted tree exceeded depth limit {}",
            limits.max_depth
        )));
    }
    scan.depth = scan.depth.max(depth);
    let directory = if relative.as_os_str().is_empty() {
        root.to_path_buf()
    } else {
        root.join(relative)
    };
    let entries = sorted_entries(&directory, limits.max_files.saturating_add(1))?;
    for path in entries {
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| CacheStoreError::io("inspect extracted tree entry", &path, error))?;
        let name = path
            .strip_prefix(root)
            .map_err(|error| CacheStoreError::MalformedCache(error.to_string()))?;
        let portable = portable_relative(name)?;
        if metadata.file_type().is_symlink() {
            return Err(CacheStoreError::MalformedCache(format!(
                "symlink in extracted tree: {portable}"
            )));
        }
        let kind = if metadata.file_type().is_dir() {
            b'D'
        } else if metadata.file_type().is_file() {
            b'F'
        } else {
            return Err(CacheStoreError::MalformedCache(format!(
                "unsupported filesystem entry in extracted tree: {portable}"
            )));
        };
        let header_len = if kind == b'D' { 0 } else { metadata.len() };
        hash_tree_entry_header(&mut scan.hasher, kind, portable.as_bytes(), header_len);
        if kind == b'D' {
            walk_tree(root, name, depth.saturating_add(1), limits, scan)?;
        } else {
            scan.files = scan.files.saturating_add(1);
            if scan.files > limits.max_files {
                return Err(CacheStoreError::LimitExceeded(format!(
                    "extracted tree exceeded {} files",
                    limits.max_files
                )));
            }
            scan.bytes = scan.bytes.checked_add(metadata.len()).ok_or_else(|| {
                CacheStoreError::LimitExceeded("expanded byte count overflowed".to_string())
            })?;
            if scan.bytes > limits.max_bytes {
                return Err(CacheStoreError::LimitExceeded(format!(
                    "extracted tree exceeded {} bytes",
                    limits.max_bytes
                )));
            }
            hash_file_into(&path, metadata.len(), &mut scan.hasher)?;
        }
    }
    Ok(())
}

fn hash_tree_entry_header(hasher: &mut Sha256, kind: u8, path: &[u8], length: u64) {
    hasher.update([kind]);
    hasher.update(u64::try_from(path.len()).unwrap_or(u64::MAX).to_le_bytes());
    hasher.update(path);
    hasher.update(length.to_le_bytes());
}

fn hash_file_into(
    path: &Path,
    expected_len: u64,
    hasher: &mut Sha256,
) -> Result<(), CacheStoreError> {
    let mut file = File::open(path)
        .map_err(|error| CacheStoreError::io("read extracted tree file", path, error))?;
    let mut remaining = expected_len;
    let mut buffer = [0_u8; 32 * 1024];
    while remaining > 0 {
        let requested = usize::try_from(remaining)
            .unwrap_or(buffer.len())
            .min(buffer.len());
        let read = file
            .read(&mut buffer[..requested])
            .map_err(|error| CacheStoreError::io("hash extracted tree file", path, error))?;
        if read == 0 {
            return Err(CacheStoreError::MalformedCache(format!(
                "tree file changed while verifying: {}",
                path.display()
            )));
        }
        hasher.update(&buffer[..read]);
        remaining = remaining.saturating_sub(u64::try_from(read).unwrap_or(0));
    }
    let mut extra = [0_u8; 1];
    if file
        .read(&mut extra)
        .map_err(|error| CacheStoreError::io("verify extracted tree file", path, error))?
        != 0
    {
        return Err(CacheStoreError::MalformedCache(format!(
            "tree file changed size while verifying: {}",
            path.display()
        )));
    }
    Ok(())
}

fn portable_relative(path: &Path) -> Result<String, CacheStoreError> {
    let text = path
        .to_str()
        .ok_or_else(|| CacheStoreError::MalformedCache("tree path is not UTF-8".to_string()))?;
    let portable = text.replace('\\', "/");
    if portable.is_empty()
        || portable.starts_with('/')
        || portable
            .split('/')
            .any(|component| component.is_empty() || component == "." || component == "..")
    {
        return Err(CacheStoreError::MalformedCache(format!(
            "non-portable extracted tree path: {portable}"
        )));
    }
    Ok(portable)
}

#[derive(Debug, Default)]
struct ArchiveScan {
    scanned: usize,
    valid: usize,
    corrupt: usize,
    invalid: usize,
    bytes: u64,
}

fn scan_archives(
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

fn scan_transient(
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

fn sorted_entries(root: &Path, max_entries: usize) -> Result<Vec<PathBuf>, CacheStoreError> {
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

fn is_plain_directory(path: &Path) -> Result<bool, CacheStoreError> {
    fs::symlink_metadata(path)
        .map(|metadata| metadata.file_type().is_dir())
        .map_err(|error| CacheStoreError::io("inspect cache directory", path, error))
}

fn is_lower_hex(value: &str) -> bool {
    value
        .bytes()
        .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
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

fn validate_limits(limits: CacheScanLimits) -> Result<(), CacheStoreError> {
    if limits.max_entries == 0 || limits.max_bytes == 0 {
        return Err(CacheStoreError::LimitExceeded(
            "cache limits must be greater than zero".to_string(),
        ));
    }
    Ok(())
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
    fn io(operation: &'static str, path: &Path, source: std::io::Error) -> Self {
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
            Self::DigestMismatch { .. } => None,
            Self::Io { source, .. } => Some(source),
            Self::LimitExceeded(_) | Self::MalformedCache(_) => None,
        }
    }
}

fn create_parent(path: &Path) -> Result<(), CacheStoreError> {
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

fn nonce() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HostPlatform {
    Windows,
    MacOs,
    Unix,
}

impl HostPlatform {
    const fn current() -> Self {
        if cfg!(windows) {
            Self::Windows
        } else if cfg!(target_os = "macos") {
            Self::MacOs
        } else {
            Self::Unix
        }
    }
}

#[derive(Debug, Default)]
struct CacheEnvironment {
    local_app_data: Option<PathBuf>,
    xdg_cache_home: Option<PathBuf>,
    home: Option<PathBuf>,
}

impl CacheEnvironment {
    fn current() -> Self {
        Self {
            local_app_data: env::var_os("LOCALAPPDATA").map(PathBuf::from),
            xdg_cache_home: env::var_os("XDG_CACHE_HOME").map(PathBuf::from),
            home: env::var_os("HOME")
                .map(PathBuf::from)
                .or_else(|| env::var_os("USERPROFILE").map(PathBuf::from)),
        }
    }
}

/// Resolve the global cache with precedence: flag, Arandu environment, native default.
pub fn resolve_cache_layout(explicit: Option<&Path>) -> Result<CacheLayout, String> {
    let environment_override = env::var_os(CACHE_DIR_ENV).map(PathBuf::from);
    let root = resolve_cache_root_for(
        HostPlatform::current(),
        explicit.map(Path::to_path_buf),
        environment_override,
        CacheEnvironment::current(),
    )?;
    CacheLayout::new(root).map_err(|error| error.to_string())
}

fn resolve_cache_root_for(
    platform: HostPlatform,
    explicit: Option<PathBuf>,
    environment_override: Option<PathBuf>,
    environment: CacheEnvironment,
) -> Result<PathBuf, String> {
    if let Some(root) = explicit.or(environment_override) {
        if !is_absolute_for(platform, &root) {
            return Err(format!(
                "cache root must be absolute, found `{}`",
                root.display()
            ));
        }
        return Ok(root);
    }

    let root = match platform {
        HostPlatform::Windows => environment
            .local_app_data
            .filter(|path| is_absolute_for(platform, path))
            .map(|path| path.join("Arandu").join("Cache"))
            .ok_or_else(|| {
                "cannot locate Windows local application data for Arandu cache".to_string()
            })?,
        HostPlatform::MacOs => environment
            .home
            .filter(|path| is_absolute_for(platform, path))
            .map(|path| path.join("Library").join("Caches").join("Arandu"))
            .ok_or_else(|| "cannot locate the home directory for Arandu cache".to_string())?,
        HostPlatform::Unix => environment
            .xdg_cache_home
            .filter(|path| is_absolute_for(platform, path))
            .map(|path| path.join("arandu"))
            .or_else(|| {
                environment
                    .home
                    .filter(|path| is_absolute_for(platform, path))
                    .map(|path| path.join(".cache").join("arandu"))
            })
            .ok_or_else(|| {
                "cannot locate XDG_CACHE_HOME or the home directory for Arandu cache".to_string()
            })?,
    };
    Ok(root)
}

fn is_absolute_for(platform: HostPlatform, path: &Path) -> bool {
    let value = path.as_os_str().to_string_lossy();
    match platform {
        HostPlatform::Windows => {
            let bytes = value.as_bytes();
            value.starts_with(r"\\")
                || (bytes.len() >= 3
                    && bytes[0].is_ascii_alphabetic()
                    && bytes[1] == b':'
                    && matches!(bytes[2], b'\\' | b'/'))
        }
        HostPlatform::MacOs | HostPlatform::Unix => value.starts_with('/'),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};
    use std::thread;

    fn temp_layout(name: &str) -> CacheLayout {
        let root = env::temp_dir().join(format!(
            "arandu-p5-{name}-{}-{}",
            std::process::id(),
            nonce()
        ));
        CacheLayout::new(root).unwrap()
    }

    #[test]
    fn explicit_absolute_override_wins() {
        let root = "/explicit";
        let resolved = resolve_cache_root_for(
            HostPlatform::Unix,
            Some(PathBuf::from(root)),
            Some(PathBuf::from("/environment")),
            CacheEnvironment::default(),
        )
        .unwrap();
        assert_eq!(resolved, Path::new(root));
    }

    #[test]
    fn relative_arandu_override_is_an_error() {
        let error = resolve_cache_root_for(
            HostPlatform::Unix,
            None,
            Some(PathBuf::from("relative-cache")),
            CacheEnvironment::default(),
        )
        .unwrap_err();
        assert!(error.contains("must be absolute"));
    }

    #[test]
    fn windows_uses_local_application_data() {
        let resolved = resolve_cache_root_for(
            HostPlatform::Windows,
            None,
            None,
            CacheEnvironment {
                local_app_data: Some(PathBuf::from(r"C:\Users\test\AppData\Local")),
                ..CacheEnvironment::default()
            },
        )
        .unwrap();
        assert_eq!(
            resolved,
            Path::new(r"C:\Users\test\AppData\Local\Arandu\Cache")
        );
    }

    #[test]
    fn macos_uses_library_caches() {
        let resolved = resolve_cache_root_for(
            HostPlatform::MacOs,
            None,
            None,
            CacheEnvironment {
                home: Some(PathBuf::from("/Users/test")),
                ..CacheEnvironment::default()
            },
        )
        .unwrap();
        assert_eq!(resolved, Path::new("/Users/test/Library/Caches/Arandu"));
    }

    #[test]
    fn unix_uses_absolute_xdg_then_home_fallback() {
        let xdg = resolve_cache_root_for(
            HostPlatform::Unix,
            None,
            None,
            CacheEnvironment {
                xdg_cache_home: Some(PathBuf::from("/var/cache/user")),
                home: Some(PathBuf::from("/home/test")),
                ..CacheEnvironment::default()
            },
        )
        .unwrap();
        assert_eq!(xdg, Path::new("/var/cache/user/arandu"));

        let fallback = resolve_cache_root_for(
            HostPlatform::Unix,
            None,
            None,
            CacheEnvironment {
                xdg_cache_home: Some(PathBuf::from("relative")),
                home: Some(PathBuf::from("/home/test")),
                ..CacheEnvironment::default()
            },
        )
        .unwrap();
        assert_eq!(fallback, Path::new("/home/test/.cache/arandu"));
    }

    #[test]
    fn archive_publish_is_verified_and_immutable() {
        let layout = temp_layout("immutable");
        let store = CacheStore::new(layout.clone());
        let bytes = b"canonical package archive";
        let digest = CacheDigest::sha256(bytes);

        assert_eq!(
            store.publish_archive(digest, bytes).unwrap(),
            CachePublish::Added
        );
        assert_eq!(
            store.publish_archive(digest, bytes).unwrap(),
            CachePublish::AlreadyPresent
        );
        assert_eq!(fs::read(layout.archive(digest)).unwrap(), bytes);
        assert!(store.publish_archive(digest, b"substitution").is_err());
        assert_eq!(fs::read(layout.archive(digest)).unwrap(), bytes);

        fs::remove_dir_all(layout.root()).unwrap();
    }

    #[test]
    fn corrupt_entry_is_quarantined_and_repaired_under_lock() {
        let layout = temp_layout("repair");
        let store = CacheStore::new(layout.clone());
        let bytes = b"verified package";
        let digest = CacheDigest::sha256(bytes);
        let archive = layout.archive(digest);
        create_parent(&archive).unwrap();
        fs::write(&archive, b"tampered").unwrap();

        assert_eq!(
            store.publish_archive(digest, bytes).unwrap(),
            CachePublish::Repaired
        );
        assert_eq!(fs::read(&archive).unwrap(), bytes);
        assert_eq!(fs::read_dir(layout.quarantine()).unwrap().count(), 1);

        fs::remove_dir_all(layout.root()).unwrap();
    }

    #[test]
    fn concurrent_publishers_converge_on_one_verified_object() {
        let layout = temp_layout("concurrent");
        let store = Arc::new(CacheStore::new(layout.clone()));
        let bytes: Arc<[u8]> = Arc::from(&b"shared package archive"[..]);
        let digest = CacheDigest::sha256(&bytes);
        let barrier = Arc::new(Barrier::new(8));
        let mut workers = Vec::new();
        for _ in 0..8 {
            let store = Arc::clone(&store);
            let bytes = Arc::clone(&bytes);
            let barrier = Arc::clone(&barrier);
            workers.push(thread::spawn(move || {
                barrier.wait();
                store.publish_archive(digest, &bytes)
            }));
        }

        let results = workers
            .into_iter()
            .map(|worker| worker.join().unwrap().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            results
                .iter()
                .filter(|result| **result == CachePublish::Added)
                .count(),
            1
        );
        assert_eq!(fs::read(layout.archive(digest)).unwrap(), &*bytes);
        assert_eq!(fs::read_dir(layout.staging()).unwrap().count(), 0);

        fs::remove_dir_all(layout.root()).unwrap();
    }

    #[test]
    fn stale_staging_file_is_not_a_cache_hit() {
        let layout = temp_layout("stale");
        fs::create_dir_all(layout.staging()).unwrap();
        fs::write(layout.staging().join("interrupted.tmp"), b"partial").unwrap();
        let store = CacheStore::new(layout.clone());
        let bytes = b"complete archive";
        let digest = CacheDigest::sha256(bytes);

        assert_eq!(
            store.publish_archive(digest, bytes).unwrap(),
            CachePublish::Added
        );
        assert_eq!(fs::read(layout.archive(digest)).unwrap(), bytes);

        fs::remove_dir_all(layout.root()).unwrap();
    }

    #[test]
    fn inspect_and_verify_report_tampering_deterministically() {
        let layout = temp_layout("verify-command");
        let store = CacheStore::new(layout.clone());
        let first = b"first archive";
        let second = b"second archive";
        let first_digest = CacheDigest::sha256(first);
        let second_digest = CacheDigest::sha256(second);
        store.publish_archive(first_digest, first).unwrap();
        store.publish_archive(second_digest, second).unwrap();

        let inspect = store.inspect(CacheScanLimits::default()).unwrap();
        assert_eq!(inspect.archives, 2);
        assert_eq!(
            inspect.archive_bytes,
            u64::try_from(first.len() + second.len()).unwrap()
        );
        assert_eq!(inspect.invalid_entries, 0);

        fs::write(layout.archive(second_digest), b"tampered archive").unwrap();
        let verify = store.verify(CacheScanLimits::default()).unwrap();
        assert_eq!(verify.verified, 1);
        assert_eq!(verify.corrupt, 1);
        assert_eq!(verify.invalid_entries, 0);

        fs::remove_dir_all(layout.root()).unwrap();
    }

    #[test]
    fn prune_is_dry_run_capable_and_never_removes_archives() {
        let layout = temp_layout("prune-command");
        let store = CacheStore::new(layout.clone());
        let bytes = b"keep this archive";
        let digest = CacheDigest::sha256(bytes);
        store.publish_archive(digest, bytes).unwrap();
        fs::write(layout.staging().join("stale.tmp"), b"partial").unwrap();
        fs::create_dir_all(layout.quarantine()).unwrap();
        fs::write(layout.quarantine().join("bad.corrupt"), b"bad").unwrap();

        let preview = store.prune(CacheScanLimits::default(), true).unwrap();
        assert_eq!(preview.files, 2);
        assert!(layout.staging().join("stale.tmp").exists());
        assert!(layout.quarantine().join("bad.corrupt").exists());

        let removed = store.prune(CacheScanLimits::default(), false).unwrap();
        assert_eq!(removed.files, 2);
        assert!(layout.archive(digest).exists());
        assert!(!layout.staging().join("stale.tmp").exists());
        assert!(!layout.quarantine().join("bad.corrupt").exists());

        fs::remove_dir_all(layout.root()).unwrap();
    }

    #[test]
    fn scans_fail_closed_at_entry_and_byte_limits() {
        let layout = temp_layout("limits");
        let store = CacheStore::new(layout.clone());
        let bytes = b"bounded archive";
        let digest = CacheDigest::sha256(bytes);
        store.publish_archive(digest, bytes).unwrap();

        assert!(
            store
                .verify(CacheScanLimits {
                    max_entries: 1,
                    max_bytes: u64::MAX,
                })
                .is_err()
        );
        assert!(
            store
                .verify(CacheScanLimits {
                    max_entries: 10,
                    max_bytes: 1,
                })
                .is_err()
        );

        fs::remove_dir_all(layout.root()).unwrap();
    }

    #[test]
    fn extracted_tree_digest_is_order_independent_and_detects_tampering() {
        let layout = temp_layout("tree-verify");
        let store = CacheStore::new(layout.clone());
        let archive_digest = CacheDigest::sha256(b"archive");
        let tree = layout.tree(archive_digest);
        fs::create_dir_all(tree.join("src/nested")).unwrap();
        fs::write(tree.join("src/z.aru"), b"z").unwrap();
        fs::write(tree.join("src/nested/a.aru"), b"a").unwrap();

        let first = hash_tree(&tree, TreeLimits::default()).unwrap();
        let verified = store
            .verify_tree(archive_digest, first.digest, TreeLimits::default())
            .unwrap();
        assert_eq!(verified, first);

        fs::write(tree.join("src/z.aru"), b"tampered").unwrap();
        assert!(
            store
                .verify_tree(archive_digest, first.digest, TreeLimits::default())
                .is_err()
        );
        fs::remove_dir_all(layout.root()).unwrap();
    }

    #[test]
    fn extracted_tree_rejects_expansion_bombs() {
        let layout = temp_layout("tree-limits");
        let archive_digest = CacheDigest::sha256(b"archive");
        let tree = layout.tree(archive_digest);
        fs::create_dir_all(&tree).unwrap();
        fs::write(tree.join("large.aru"), b"123456789").unwrap();
        assert!(matches!(
            hash_tree(
                &tree,
                TreeLimits {
                    max_files: 1,
                    max_bytes: 4,
                    max_depth: 2
                }
            ),
            Err(CacheStoreError::LimitExceeded(_))
        ));
        fs::remove_dir_all(layout.root()).unwrap();
    }
}
