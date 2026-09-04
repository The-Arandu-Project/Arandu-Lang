//! Pure contracts for the verified global package cache.
//!
//! This module deliberately performs no I/O. The CLI owns platform discovery
//! and cache mutation; package resolution consumes only validated identities.

use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

/// Digest algorithm accepted by the first public package-cache format.
pub const CACHE_DIGEST_ALGORITHM: &str = "sha256";

/// A canonical SHA-256 content identity (`sha256:<lowercase hex>`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CacheDigest([u8; 32]);

impl CacheDigest {
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub const fn bytes(self) -> [u8; 32] {
        self.0
    }

    /// Compute the canonical cache identity for an immutable byte stream.
    pub fn sha256(bytes: &[u8]) -> Self {
        use sha2::{Digest as _, Sha256};

        Self(Sha256::digest(bytes).into())
    }

    /// Lowercase hexadecimal form safe for portable cache path components.
    pub fn hex(self) -> String {
        let mut output = String::with_capacity(64);
        for byte in self.0 {
            use fmt::Write as _;
            let _ = write!(output, "{byte:02x}");
        }
        output
    }
}

impl fmt::Display for CacheDigest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{CACHE_DIGEST_ALGORITHM}:{}", self.hex())
    }
}

impl FromStr for CacheDigest {
    type Err = CacheDigestError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let hex = value
            .strip_prefix("sha256:")
            .ok_or(CacheDigestError::UnsupportedAlgorithm)?;
        if hex.len() != 64 {
            return Err(CacheDigestError::InvalidLength(hex.len()));
        }
        if !hex
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        {
            return Err(CacheDigestError::NonCanonicalHex);
        }
        let mut bytes = [0_u8; 32];
        let (chunks, _) = hex.as_bytes().as_chunks::<2>();
        for (index, pair) in chunks.iter().enumerate() {
            let pair = std::str::from_utf8(pair).map_err(|_| CacheDigestError::NonCanonicalHex)?;
            bytes[index] =
                u8::from_str_radix(pair, 16).map_err(|_| CacheDigestError::NonCanonicalHex)?;
        }
        Ok(Self(bytes))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CacheDigestError {
    UnsupportedAlgorithm,
    InvalidLength(usize),
    NonCanonicalHex,
}

impl fmt::Display for CacheDigestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedAlgorithm => write!(
                formatter,
                "cache digest must use `{CACHE_DIGEST_ALGORITHM}:`"
            ),
            Self::InvalidLength(actual) => {
                write!(
                    formatter,
                    "SHA-256 digest must have 64 hex digits, found {actual}"
                )
            }
            Self::NonCanonicalHex => write!(
                formatter,
                "SHA-256 digest must use lowercase hexadecimal digits"
            ),
        }
    }
}

impl std::error::Error for CacheDigestError {}

/// Stable, content-addressed cache layout.
///
/// Archives and extracted trees have distinct namespaces. Staging is never a
/// valid cache hit, and a lock covers only one digest rather than the cache.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheLayout {
    root: PathBuf,
}

impl CacheLayout {
    pub fn new(root: PathBuf) -> Result<Self, CacheLayoutError> {
        if !root.is_absolute() {
            return Err(CacheLayoutError::RelativeRoot(root));
        }
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn archive(&self, digest: CacheDigest) -> PathBuf {
        self.digest_path("archives", digest)
            .with_extension("tar.zst")
    }

    pub fn tree(&self, digest: CacheDigest) -> PathBuf {
        self.digest_path("trees", digest)
    }

    pub fn metadata(&self, digest: CacheDigest) -> PathBuf {
        self.digest_path("metadata", digest).with_extension("toml")
    }

    pub fn entry_lock(&self, digest: CacheDigest) -> PathBuf {
        self.digest_path("locks", digest).with_extension("lock")
    }

    pub fn staging(&self) -> PathBuf {
        self.root.join("staging")
    }

    pub fn quarantine(&self) -> PathBuf {
        self.root.join("quarantine")
    }

    fn digest_path(&self, namespace: &str, digest: CacheDigest) -> PathBuf {
        let hex = digest.hex();
        self.root
            .join("v1")
            .join(namespace)
            .join(CACHE_DIGEST_ALGORITHM)
            .join(&hex[..2])
            .join(&hex[2..])
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CacheLayoutError {
    RelativeRoot(PathBuf),
}

impl fmt::Display for CacheLayoutError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RelativeRoot(path) => write!(
                formatter,
                "cache root must be absolute, found `{}`",
                path.display()
            ),
        }
    }
}

impl std::error::Error for CacheLayoutError {}

#[cfg(test)]
mod tests {
    use super::*;

    const HEX: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn absolute_root() -> PathBuf {
        if cfg!(windows) {
            PathBuf::from(r"C:\cache\arandu")
        } else {
            PathBuf::from("/cache/arandu")
        }
    }

    #[test]
    fn digest_round_trips_only_canonical_sha256() {
        let digest: CacheDigest = format!("sha256:{HEX}").parse().unwrap();
        assert_eq!(digest.to_string(), format!("sha256:{HEX}"));
        assert!(format!("sha256:{}", HEX.to_uppercase())
            .parse::<CacheDigest>()
            .is_err());
        assert!(HEX.parse::<CacheDigest>().is_err());
    }

    #[test]
    fn layout_fans_out_by_digest_and_separates_entry_locks() {
        let layout = CacheLayout::new(absolute_root()).unwrap();
        let digest: CacheDigest = format!("sha256:{HEX}").parse().unwrap();
        let tree = layout.tree(digest);
        let lock = layout.entry_lock(digest);
        assert!(tree.ends_with(Path::new(&format!("sha256/01/{}", &HEX[2..]))));
        assert!(lock.ends_with(Path::new(&format!("sha256/01/{}.lock", &HEX[2..]))));
        assert_ne!(tree.parent(), lock.parent());
        assert!(!tree.to_string_lossy().contains("package-name"));
    }

    #[test]
    fn relative_roots_are_rejected() {
        assert!(matches!(
            CacheLayout::new(PathBuf::from("cache")),
            Err(CacheLayoutError::RelativeRoot(_))
        ));
    }
}
