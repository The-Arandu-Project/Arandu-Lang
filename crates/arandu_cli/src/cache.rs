//! Platform-native cache-root discovery for package artifacts.

use std::env;
use std::path::{Path, PathBuf};

use arandu_query::CacheLayout;

pub const CACHE_DIR_ENV: &str = "ARANDU_CACHE_DIR";

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
}
