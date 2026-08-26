//! Project-local artifact lifecycle. Filesystem effects stay outside Salsa.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::cli_error::CliFailure;

const TARGET_MARKER: &str = ".arandu-target-v1";

#[derive(Debug)]
pub struct ArtifactLayout {
    pub target_root: PathBuf,
    pub profile_root: PathBuf,
    pub bin: PathBuf,
    pub deps: PathBuf,
    pub incremental: PathBuf,
    pub triple: String,
}

#[derive(Serialize)]
struct BuildState<'a> {
    schema: u32,
    package: &'a str,
    version: &'a str,
    profile: &'a str,
    target: &'a str,
    backend: &'a str,
    artifact_digest: &'a str,
    compiler_version: &'a str,
    artifact: &'a str,
    object: &'a str,
    linker: &'a str,
}

pub fn host_triple() -> String {
    let arch = match std::env::consts::ARCH {
        "x86" => "i686",
        other => other,
    };
    match (arch, std::env::consts::OS) {
        (arch, "windows") => format!("{arch}-pc-windows-msvc"),
        (arch, "macos") => format!("{arch}-apple-darwin"),
        (arch, "linux") => format!("{arch}-unknown-linux-gnu"),
        (arch, os) => format!("{arch}-unknown-{os}"),
    }
}

pub fn layout(project_root: &Path, profile: &str) -> ArtifactLayout {
    let triple = host_triple();
    let target_root = project_root.join("target");
    let profile_root = target_root.join(profile).join(&triple);
    let bin = profile_root.join("bin");
    let deps = profile_root.join("deps");
    let incremental = profile_root.join("incremental");
    ArtifactLayout {
        target_root,
        profile_root,
        bin,
        deps,
        incremental,
        triple,
    }
}

pub fn publish_native_artifact(
    project_root: &Path,
    package: &str,
    version: &str,
    object: &[u8],
    link: impl FnOnce(&Path, &Path) -> Result<&'static str, CliFailure>,
) -> Result<PathBuf, CliFailure> {
    let layout = layout(project_root, "dev");
    for directory in [&layout.bin, &layout.deps, &layout.incremental] {
        fs::create_dir_all(directory)
            .map_err(|error| failure("create artifact layout", directory, error))?;
    }
    // Builds for one profile may run in independent processes. Keep the
    // content-addressed publication and its mutable state pointer in one
    // transaction. File locks are released by the OS if a process crashes.
    let lock_path = layout.profile_root.join(".publish.lock");
    let publish_lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|error| failure("open build publication lock", &lock_path, error))?;
    publish_lock
        .lock()
        .map_err(|error| failure("lock build publication", &lock_path, error))?;
    atomic_write(
        &layout.target_root.join(TARGET_MARKER),
        b"arandu-target-v1\n",
    )?;

    let digest = blake3::hash(object).to_hex().to_string();
    let extension = if cfg!(windows) { "obj" } else { "o" };
    let artifact_name = format!("{package}-{}.{extension}", &digest[..16]);
    let artifact_path = layout.deps.join(&artifact_name);
    atomic_write(&artifact_path, object)?;

    let staging = unique_staging_path(
        &layout.bin.join(if cfg!(windows) {
            format!("{package}.exe")
        } else {
            package.to_string()
        }),
        "link",
    );
    let linker = match link(&artifact_path, &staging) {
        Ok(linker) => linker,
        Err(error) => {
            let _ = fs::remove_file(&staging);
            return Err(error);
        }
    };
    let executable =
        fs::read(&staging).map_err(|error| failure("read linked artifact", &staging, error))?;
    if executable.is_empty() {
        let _ = fs::remove_file(&staging);
        return Err(CliFailure::operational(
            "publish linked artifact",
            Some(staging),
            "linker produced an empty file",
        ));
    }
    let executable_digest = blake3::hash(&executable).to_hex().to_string();
    let executable_name = if cfg!(windows) {
        format!("{package}-{}.exe", &executable_digest[..16])
    } else {
        format!("{package}-{}", &executable_digest[..16])
    };
    let executable_path = layout.bin.join(&executable_name);
    publish_staging(&staging, &executable_path)?;

    let relative = format!("bin/{executable_name}");
    let object_relative = format!("deps/{artifact_name}");
    let state = BuildState {
        schema: 2,
        package,
        version,
        profile: "dev",
        target: &layout.triple,
        backend: "cranelift-aot",
        artifact_digest: &executable_digest,
        compiler_version: crate::project::ARANDU_VERSION,
        artifact: &relative,
        object: &object_relative,
        linker,
    };
    let mut encoded = serde_json::to_vec_pretty(&state).map_err(|error| {
        CliFailure::operational("serialize build provenance", None, error.to_string())
    })?;
    encoded.push(b'\n');
    atomic_replace(&layout.profile_root.join("build-state.json"), &encoded)?;
    Ok(executable_path)
}

pub fn clean(project_root: &Path) -> Result<bool, CliFailure> {
    let canonical_root = fs::canonicalize(project_root)
        .map_err(|error| failure("resolve project root", project_root, error))?;
    let target = canonical_root.join("target");
    let metadata = match fs::symlink_metadata(&target) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(failure("inspect target directory", &target, error)),
    };
    let marker_valid = fs::read(target.join(TARGET_MARKER))
        .is_ok_and(|contents| contents == b"arandu-target-v1\n");
    if !metadata.is_dir() || metadata.file_type().is_symlink() || !marker_valid {
        return Err(CliFailure::operational(
            "clean project artifacts",
            Some(target),
            "refusing to remove an unowned, non-directory, symlink or junction-like target",
        ));
    }
    fs::remove_dir_all(&target)
        .map_err(|error| failure("clean project artifacts", &target, error))?;
    Ok(true)
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), CliFailure> {
    if path.is_file() {
        return Ok(());
    }
    write_staging(path, bytes).and_then(|staging| {
        fs::rename(&staging, path).map_err(|error| {
            let _ = fs::remove_file(&staging);
            failure("publish artifact", path, error)
        })
    })
}

pub(crate) fn atomic_replace(path: &Path, bytes: &[u8]) -> Result<(), CliFailure> {
    let staging = write_staging(path, bytes)?;
    if !path.exists() {
        return fs::rename(&staging, path)
            .map_err(|error| failure("publish build state", path, error));
    }
    atomic_platform_replace(path, &staging)
}

#[cfg(not(windows))]
fn atomic_platform_replace(path: &Path, staging: &Path) -> Result<(), CliFailure> {
    fs::rename(staging, path).map_err(|error| failure("publish build state", path, error))
}

#[cfg(windows)]
fn atomic_platform_replace(path: &Path, staging: &Path) -> Result<(), CliFailure> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{REPLACEFILE_WRITE_THROUGH, ReplaceFileW};

    let replaced = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let replacement = staging
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    // SAFETY: both paths are owned, NUL-terminated UTF-16 buffers that remain
    // alive for the duration of the Win32 call; optional pointers are null.
    let result = unsafe {
        ReplaceFileW(
            replaced.as_ptr(),
            replacement.as_ptr(),
            std::ptr::null(),
            REPLACEFILE_WRITE_THROUGH,
            std::ptr::null(),
            std::ptr::null(),
        )
    };
    if result == 0 {
        let error = std::io::Error::last_os_error();
        let _ = fs::remove_file(staging);
        return Err(failure("publish build state", path, error));
    }
    Ok(())
}

fn publish_staging(staging: &Path, destination: &Path) -> Result<(), CliFailure> {
    if destination.is_file() {
        fs::remove_file(staging)
            .map_err(|error| failure("discard duplicate artifact", staging, error))?;
        return Ok(());
    }
    fs::rename(staging, destination)
        .map_err(|error| failure("publish linked artifact", destination, error))
}

fn write_staging(path: &Path, bytes: &[u8]) -> Result<PathBuf, CliFailure> {
    let staging = unique_staging_path(path, "write");
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&staging)
        .map_err(|error| failure("create artifact staging file", &staging, error))?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|error| {
            let _ = fs::remove_file(&staging);
            failure("flush artifact staging file", &staging, error)
        })?;
    Ok(staging)
}

fn unique_staging_path(path: &Path, operation: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    path.with_extension(format!("{operation}-tmp-{}-{nonce}", std::process::id()))
}

fn failure(operation: &'static str, path: &Path, error: std::io::Error) -> CliFailure {
    CliFailure::operational(operation, Some(path.to_path_buf()), error.to_string())
}
