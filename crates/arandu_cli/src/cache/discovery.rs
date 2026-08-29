//! Native platform cache-root resolution and path validation.

use std::env;
use std::path::{Path, PathBuf};

use super::types::CACHE_DIR_ENV;
use arandu_query::CacheLayout;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HostPlatform {
    Windows,
    MacOs,
    Unix,
}

impl HostPlatform {
    pub(crate) const fn current() -> Self {
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
pub(crate) struct CacheEnvironment {
    pub(crate) local_app_data: Option<PathBuf>,
    pub(crate) xdg_cache_home: Option<PathBuf>,
    pub(crate) home: Option<PathBuf>,
}

impl CacheEnvironment {
    pub(crate) fn current() -> Self {
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

pub(crate) fn resolve_cache_root_for(
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

pub(crate) fn is_absolute_for(platform: HostPlatform, path: &Path) -> bool {
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
