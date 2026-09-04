//! Canonical SHA-256 tree hashing and portable relative path validation.

use std::fs::{self, File};
use std::io::Read;
use std::path::Path;

use arandu_query::CacheDigest;
use sha2::{Digest as _, Sha256};

use super::ops::sorted_entries;
use super::store::{is_link_like, is_plain_directory};
use super::types::{COPY_BUFFER_SIZE, CacheStoreError, TreeLimits, TreeVerification};

pub(crate) fn hash_tree(
    root: &Path,
    limits: TreeLimits,
) -> Result<TreeVerification, CacheStoreError> {
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
        if is_link_like(&metadata) {
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
    let mut buffer = [0_u8; COPY_BUFFER_SIZE];
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
