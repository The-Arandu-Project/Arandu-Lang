//! Filesystem and URI path normalization helpers for compiler/VFS registries.

use std::path::{Path, PathBuf};

pub(super) fn registry_path_key(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    #[cfg(windows)]
    let text = text.strip_prefix("//?/").unwrap_or(&text).to_string();
    text
}

pub(super) fn normalize_path_soft(path: &Path) -> PathBuf {
    let resolved = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    PathBuf::from(registry_path_key(&resolved))
}

pub(super) fn package_relative_path(path: &Path, package_src: &Path) -> Option<String> {
    // Rename notifications arrive after the old path has ceased to exist.
    // Derive its import key lexically before trying filesystem normalization,
    // which may otherwise retain a Windows verbatim prefix on one side only.
    let lexical_path = PathBuf::from(registry_path_key(path));
    let lexical_src = PathBuf::from(registry_path_key(package_src));
    let relative = lexical_path
        .strip_prefix(&lexical_src)
        .ok()
        .map(Path::to_path_buf)
        .or_else(|| {
            let normalized_path = normalize_path_soft(path);
            let normalized_src = normalize_path_soft(package_src);
            normalized_path
                .strip_prefix(normalized_src)
                .ok()
                .map(Path::to_path_buf)
        })?;
    Some(relative.to_string_lossy().replace('\\', "/"))
}
