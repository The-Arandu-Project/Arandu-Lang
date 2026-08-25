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

pub fn publish_c_artifact(
    project_root: &Path,
    package: &str,
    version: &str,
    source: &str,
) -> Result<PathBuf, CliFailure> {
    let layout = layout(project_root, "dev");
    for directory in [&layout.bin, &layout.deps, &layout.incremental] {
        fs::create_dir_all(directory)
            .map_err(|error| failure("create artifact layout", directory, error))?;
    }
    atomic_write(
        &layout.target_root.join(TARGET_MARKER),
        b"arandu-target-v1\n",
    )?;

    let digest = blake3::hash(source.as_bytes()).to_hex().to_string();
    let artifact_name = format!("{package}-{}.c", &digest[..16]);
    let artifact_path = layout.deps.join(&artifact_name);
    atomic_write(&artifact_path, source.as_bytes())?;

    let relative = format!("deps/{artifact_name}");
    let state = BuildState {
        schema: 1,
        package,
        version,
        profile: "dev",
        target: &layout.triple,
        backend: "c-aot-source",
        artifact_digest: &digest,
        compiler_version: crate::project::ARANDU_VERSION,
        artifact: &relative,
    };
    let mut encoded = serde_json::to_vec_pretty(&state).map_err(|error| {
        CliFailure::operational("serialize build provenance", None, error.to_string())
    })?;
    encoded.push(b'\n');
    atomic_replace(&layout.profile_root.join("build-state.json"), &encoded)?;
    Ok(artifact_path)
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

fn atomic_replace(path: &Path, bytes: &[u8]) -> Result<(), CliFailure> {
    let staging = write_staging(path, bytes)?;
    if !path.exists() {
        return fs::rename(&staging, path)
            .map_err(|error| failure("publish build state", path, error));
    }
    let backup = path.with_extension("json.previous");
    let _ = fs::remove_file(&backup);
    fs::rename(path, &backup)
        .map_err(|error| failure("preserve previous build state", path, error))?;
    if let Err(error) = fs::rename(&staging, path) {
        let _ = fs::rename(&backup, path);
        let _ = fs::remove_file(&staging);
        return Err(failure("publish build state", path, error));
    }
    let _ = fs::remove_file(backup);
    Ok(())
}

fn write_staging(path: &Path, bytes: &[u8]) -> Result<PathBuf, CliFailure> {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let staging = path.with_extension(format!("tmp-{}-{nonce}", std::process::id()));
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

fn failure(operation: &'static str, path: &Path, error: std::io::Error) -> CliFailure {
    CliFailure::operational(operation, Some(path.to_path_buf()), error.to_string())
}
