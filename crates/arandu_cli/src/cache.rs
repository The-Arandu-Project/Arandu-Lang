//! Platform-native cache-root discovery for package artifacts.

use std::env;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use arandu_query::{CacheDigest, CacheLayout};

pub const CACHE_DIR_ENV: &str = "ARANDU_CACHE_DIR";

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
        }
    }
}

impl std::error::Error for CacheStoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::DigestMismatch { .. } => None,
            Self::Io { source, .. } => Some(source),
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
}
