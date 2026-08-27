//! Project-mode CLI: `new`, `doctor`, package `check`/`run`/`build`.
//!
//! Gold bars:
//! - stdlib via [`arandu_query::resolve_stdlib_root`] (`current_exe`, never cwd)
//! - `Arandu.toml` as Salsa [`ProjectManifest`] input (hash in invalidation key)
//! - `arandu doctor` diagnoses env using the same init points as compile
//! - `build` default = Cranelift; `--release` reserved for future LLVM

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::cli_error::{CliFailure, CliResult, CliSuccess};
use arandu_query::{
    DirectoryListing, LocalPackageGraph, MANIFEST_FILENAME, ManifestError, ManifestSpelling,
    ModuleRoots, ProjectManifest, STDLIB_ENV, StdlibResolveOpts, StdlibRoot,
    ensure_toolchain_compatible, find_manifest, load_manifest, register_manifest,
    resolve_stdlib_root, scan_aru_entries,
};

/// Official default entry path for `arandu new`.
pub const DEFAULT_ENTRY: &str = "src/main.aru";

/// Default `src/main.aru` content — Minimal 0.1 IN surface only.
///
/// Kept in sync with `examples/minimal/TEMPLATE_main.aru` (covered by parse CI).
pub const TEMPLATE_MAIN_ARU: &str = r#"// Default project template for Arandu Minimal 0.1 (installer / `arandu new`).
// Only IN surface — no experimental runtime/OS modules.
module my_app

import io

func main(): int {
    io.println("hello, arandu")
    return 0
}
"#;

/// CLI version string (mirrors package version).
pub const ARANDU_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Resolved package context for project-mode commands.
#[allow(dead_code)] // fields reserved for doctor/logs and future multi-file package graph
pub struct ProjectContext {
    pub root: PathBuf,
    pub manifest_path: PathBuf,
    /// Live Salsa input — kept so dependents can re-query fingerprint/entry.
    pub manifest: ProjectManifest,
    pub entry_path: PathBuf,
    /// Resolved stdlib root (cascade); available for doctor/logs.
    pub stdlib: StdlibRoot,
    /// Platform-native, verified global package cache.
    pub cache: arandu_query::CacheLayout,
    pub lockfile: arandu_query::Lockfile,
    pub name: String,
    pub version: String,
    pub entry_rel: String,
}

/// Shared flags for project / doctor commands.
#[derive(Debug, Clone, Default)]
pub struct ProjectFlags {
    pub stdlib_path: Option<PathBuf>,
    pub cache_dir: Option<PathBuf>,
    pub release: bool,
    pub verbose: bool,
    pub locked: bool,
    pub offline: bool,
    /// Explicit authority to publish a changed graph containing remote code.
    pub accept_lock: bool,
}

/// Parse `--stdlib-path=…` / `--stdlib-path …` / `--release` / `-v` from leftover args.
pub fn parse_project_flags(args: &[String]) -> Result<(ProjectFlags, Vec<String>), String> {
    let mut flags = ProjectFlags::default();
    let mut rest = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if let Some(v) = a.strip_prefix("--stdlib-path=") {
            flags.stdlib_path = Some(PathBuf::from(v));
        } else if a == "--stdlib-path" {
            i += 1;
            if i < args.len() {
                flags.stdlib_path = Some(PathBuf::from(&args[i]));
            } else {
                return Err("--stdlib-path requires a directory argument".into());
            }
        } else if let Some(v) = a.strip_prefix("--cache-dir=") {
            if v.is_empty() {
                return Err("--cache-dir requires a directory argument".into());
            }
            flags.cache_dir = Some(PathBuf::from(v));
        } else if a == "--cache-dir" {
            i += 1;
            if i < args.len() {
                flags.cache_dir = Some(PathBuf::from(&args[i]));
            } else {
                return Err("--cache-dir requires a directory argument".into());
            }
        } else if a == "--release" {
            flags.release = true;
        } else if a == "-v" || a == "--verbose" {
            flags.verbose = true;
        } else if a == "--locked" {
            flags.locked = true;
        } else if a == "--offline" {
            flags.offline = true;
        } else if a == "--frozen" {
            flags.locked = true;
            flags.offline = true;
        } else if a == "--accept" {
            flags.accept_lock = true;
        } else {
            rest.push(a.clone());
        }
        i += 1;
    }
    Ok((flags, rest))
}

/// Create a new project directory with `Arandu.toml` + template entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScaffoldKind {
    Binary,
    Library,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VcsChoice {
    Auto,
    Git,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScaffoldOptions {
    pub kind: ScaffoldKind,
    pub vcs: VcsChoice,
}

impl Default for ScaffoldOptions {
    fn default() -> Self {
        Self {
            kind: ScaffoldKind::Binary,
            vcs: VcsChoice::Auto,
        }
    }
}

pub fn parse_scaffold_options(args: &[String]) -> Result<ScaffoldOptions, String> {
    let mut options = ScaffoldOptions::default();
    let mut kind_seen = false;
    for argument in args {
        match argument.as_str() {
            "--bin" if !kind_seen => {
                options.kind = ScaffoldKind::Binary;
                kind_seen = true;
            }
            "--lib" if !kind_seen => {
                options.kind = ScaffoldKind::Library;
                kind_seen = true;
            }
            "--bin" | "--lib" => return Err("use only one of `--bin` or `--lib`".into()),
            "--vcs=auto" => options.vcs = VcsChoice::Auto,
            "--vcs=git" => options.vcs = VcsChoice::Git,
            "--vcs=none" => options.vcs = VcsChoice::None,
            other => return Err(format!("unknown project option `{other}`")),
        }
    }
    Ok(options)
}

pub fn cmd_new(name: &str, options: ScaffoldOptions) -> CliResult {
    if name.is_empty() || name.contains('/') || name.contains('\\') || name == "." || name == ".." {
        return Err(CliFailure::usage(format!(
            "invalid project name `{name}` (use a single path segment)"
        )));
    }
    let root = PathBuf::from(name);
    let parent = root
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    reject_case_collision(parent, name)?;
    if root.exists() {
        return Err(CliFailure::operational(
            "create project",
            Some(root),
            "path already exists",
        ));
    }
    arandu_query::validate_package_name(name).map_err(|error| {
        CliFailure::operational(
            "validate project name",
            Some(root.clone()),
            error.to_string(),
        )
    })?;
    let leaf = root
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or(name);
    let staging = staging_path(parent, leaf, "new");
    if let Err(error) = scaffold_into(&staging, name, options, parent) {
        let _ = fs::remove_dir_all(&staging);
        return Err(error);
    }
    fs::rename(&staging, &root).map_err(|error| {
        let _ = fs::remove_dir_all(&staging);
        CliFailure::operational("publish project", Some(root.clone()), error.to_string())
    })?;

    print_created(name, options.kind);
    Ok(CliSuccess::Done)
}

pub fn cmd_init(root: &Path, name: &str, options: ScaffoldOptions) -> CliResult {
    if !root.is_dir() {
        return Err(CliFailure::operational(
            "initialize project",
            Some(root.to_path_buf()),
            "directory does not exist",
        ));
    }
    let source_name = source_name(options.kind);
    let generated = [
        MANIFEST_FILENAME,
        "README.md",
        ".gitignore",
        source_name,
        "tests/smoke.aru",
    ];
    for relative in generated {
        if root.join(relative).exists() {
            return Err(CliFailure::operational(
                "initialize project",
                Some(root.join(relative)),
                "project file already exists",
            ));
        }
    }
    arandu_query::validate_package_name(name).map_err(|error| {
        CliFailure::operational(
            "validate project name",
            Some(root.to_path_buf()),
            error.to_string(),
        )
    })?;

    let parent = root.parent().unwrap_or_else(|| Path::new("."));
    let leaf = root
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or(name);
    let staging = staging_path(parent, leaf, "init");
    let staged_options = ScaffoldOptions {
        kind: options.kind,
        vcs: VcsChoice::None,
    };
    if let Err(error) = scaffold_into(&staging, name, staged_options, root) {
        let _ = fs::remove_dir_all(&staging);
        return Err(error);
    }
    if let Err(error) = publish_into_existing(root, &staging, source_name, options.vcs) {
        let _ = fs::remove_dir_all(&staging);
        return Err(error);
    }
    let _ = fs::remove_dir_all(&staging);
    print_created(name, options.kind);
    Ok(CliSuccess::Done)
}

fn source_name(kind: ScaffoldKind) -> &'static str {
    match kind {
        ScaffoldKind::Binary => "src/main.aru",
        ScaffoldKind::Library => "src/lib.aru",
    }
}

fn staging_path(parent: &Path, leaf: &str, operation: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    parent.join(format!(
        ".{leaf}.arandu-{operation}-{}-{nonce}",
        std::process::id()
    ))
}

fn reject_case_collision(parent: &Path, requested: &str) -> Result<(), CliFailure> {
    let entries = match fs::read_dir(parent) {
        Ok(entries) => entries,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(CliFailure::operational(
                "inspect project parent",
                Some(parent.to_path_buf()),
                error.to_string(),
            ));
        }
    };
    for entry in entries {
        let entry = entry.map_err(|error| {
            CliFailure::operational(
                "inspect project parent",
                Some(parent.to_path_buf()),
                error.to_string(),
            )
        })?;
        let existing = entry.file_name();
        if let Some(existing) = existing.to_str()
            && existing != requested
            && existing.eq_ignore_ascii_case(requested)
        {
            return Err(CliFailure::operational(
                "validate project path",
                Some(parent.join(requested)),
                format!("name differs only by case from existing `{existing}`"),
            ));
        }
    }
    Ok(())
}

fn publish_into_existing(
    root: &Path,
    staging: &Path,
    source_name: &str,
    vcs: VcsChoice,
) -> Result<(), CliFailure> {
    let src_created = !root.join("src").exists();
    let tests_created = !root.join("tests").exists();
    let git_existed = root.join(".git").exists();
    let mut published = Vec::new();

    let result = (|| {
        fs::create_dir_all(root.join("src")).map_err(|error| {
            CliFailure::operational(
                "create source directory",
                Some(root.join("src")),
                error.to_string(),
            )
        })?;
        fs::create_dir_all(root.join("tests")).map_err(|error| {
            CliFailure::operational(
                "create test directory",
                Some(root.join("tests")),
                error.to_string(),
            )
        })?;
        for relative in [
            MANIFEST_FILENAME,
            "README.md",
            ".gitignore",
            source_name,
            "tests/smoke.aru",
        ] {
            let destination = root.join(relative);
            fs::rename(staging.join(relative), &destination).map_err(|error| {
                CliFailure::operational(
                    "publish project file",
                    Some(destination.clone()),
                    error.to_string(),
                )
            })?;
            published.push(destination);
        }
        initialize_git(root, root, vcs)
    })();

    if let Err(error) = result {
        for path in published.iter().rev() {
            let _ = fs::remove_file(path);
        }
        if tests_created {
            let _ = fs::remove_dir(root.join("tests"));
        }
        if src_created {
            let _ = fs::remove_dir(root.join("src"));
        }
        if !git_existed && root.join(".git").exists() {
            let _ = fs::remove_dir_all(root.join(".git"));
        }
        return Err(error);
    }
    Ok(())
}

fn scaffold_into(root: &Path, name: &str, options: ScaffoldOptions, vcs_probe: &Path) -> CliResult {
    fs::create_dir_all(root.join("src")).map_err(|e| {
        CliFailure::operational(
            "create project directories",
            Some(root.to_path_buf()),
            e.to_string(),
        )
    })?;
    fs::create_dir_all(root.join("tests")).map_err(|e| {
        CliFailure::operational(
            "create test directory",
            Some(root.to_path_buf()),
            e.to_string(),
        )
    })?;

    let toml = format!(
        r#"# Arandu package manifest
schema = 1

[package]
name = "{name}"
version = "0.0.1"
edition = "2026"

[toolchain]
arandu = ">=0.1.0-rc.4, <0.2.0"

[targets.bin]
name = "{name}"
root = "{DEFAULT_ENTRY}"

[dependencies]

# Reserved policy surface. A2 will infer effects; empty capabilities deny authority.
[capabilities]
network = []
filesystem_read = []
filesystem_write = []
environment = []
process = []
foreign = false

[policy.effects]
deny_new_authority = true
warn_new_resources = true
deny = ["UnknownCapability"]
"#
    );
    let (target_table, source_name, source) = match options.kind {
        ScaffoldKind::Binary => (
            "targets.bin",
            "main.aru",
            TEMPLATE_MAIN_ARU.replace("module my_app", &format!("module {name}")),
        ),
        ScaffoldKind::Library => (
            "targets.lib",
            "lib.aru",
            format!("module {name}\n\npublic func answer(): int {{\n    return 42\n}}\n"),
        ),
    };
    let toml = toml
        .replace("targets.bin", target_table)
        .replace(DEFAULT_ENTRY, &format!("src/{source_name}"));

    let manifest_path = root.join(MANIFEST_FILENAME);
    let entry_path = root.join("src").join(source_name);
    fs::write(&manifest_path, toml).map_err(|e| {
        CliFailure::operational("write project manifest", Some(manifest_path), e.to_string())
    })?;
    fs::write(&entry_path, source).map_err(|e| {
        CliFailure::operational("write project entry", Some(entry_path), e.to_string())
    })?;

    fs::write(
        root.join("README.md"),
        format!("# {name}\n\nCreated with the Arandu toolchain.\n"),
    )
    .map_err(|e| {
        CliFailure::operational("write README", Some(root.join("README.md")), e.to_string())
    })?;
    fs::write(root.join(".gitignore"), "/target/\n/.arandu/\n").map_err(|e| {
        CliFailure::operational(
            "write VCS ignore",
            Some(root.join(".gitignore")),
            e.to_string(),
        )
    })?;
    fs::write(
        root.join("tests/smoke.aru"),
        format!("module {name}_tests\n\nfunc smoke(): int {{ return 0 }}\n"),
    )
    .map_err(|e| {
        CliFailure::operational(
            "write test template",
            Some(root.join("tests/smoke.aru")),
            e.to_string(),
        )
    })?;
    initialize_git(root, vcs_probe, options.vcs)?;
    Ok(CliSuccess::Done)
}

fn initialize_git(root: &Path, vcs_probe: &Path, vcs: VcsChoice) -> Result<(), CliFailure> {
    let initialize_git = match vcs {
        VcsChoice::None => false,
        VcsChoice::Git => true,
        VcsChoice::Auto => !has_git_ancestor(vcs_probe),
    };
    if initialize_git {
        let status = Command::new("git")
            .arg("init")
            .arg("--quiet")
            .arg(root)
            .status()
            .map_err(|e| {
                CliFailure::operational(
                    "initialize Git repository",
                    Some(root.to_path_buf()),
                    e.to_string(),
                )
            })?;
        if !status.success() {
            return Err(CliFailure::operational(
                "initialize Git repository",
                Some(root.to_path_buf()),
                format!("git exited with {status}"),
            ));
        }
    }
    Ok(())
}

fn has_git_ancestor(start: &Path) -> bool {
    start.ancestors().any(|path| path.join(".git").exists())
}

fn print_created(name: &str, kind: ScaffoldKind) {
    println!(
        "created {name} ({})",
        if kind == ScaffoldKind::Binary {
            "bin"
        } else {
            "lib"
        }
    );
    println!("next:\n  cd {name}\n  arandu check");
    if kind == ScaffoldKind::Binary {
        println!("  arandu run");
    }
}

/// Diagnose toolchain / project / backend (Flutter-style doctor report).
pub fn cmd_doctor(flags: &ProjectFlags) -> i32 {
    let color = use_color();
    let mut categories: Vec<DoctorCategory> = Vec::new();

    // [Arandu] toolchain binary (show raw + canonical when they differ)
    categories.push(match std::env::current_exe() {
        Ok(exe) => {
            let (real, _) = arandu_query::resolve_exe_path(exe.clone());
            let mut details = vec![
                DoctorDetail::Info(format!("binary at {}", exe.display())),
                DoctorDetail::Info(format!("version {ARANDU_VERSION}")),
            ];
            if real != exe {
                details.push(DoctorDetail::Info(format!(
                    "resolved path {} (symlink followed)",
                    real.display()
                )));
            } else if flags.verbose {
                details.push(DoctorDetail::Info(format!(
                    "canonical path {}",
                    real.display()
                )));
            }
            if flags.verbose {
                details.push(DoctorDetail::Info(format!(
                    "host {}-{}",
                    std::env::consts::OS,
                    std::env::consts::ARCH
                )));
            }
            DoctorCategory {
                status: DoctorStatus::Ok,
                title: format!("Arandu toolchain (v{ARANDU_VERSION})"),
                details,
            }
        }
        Err(e) => DoctorCategory {
            status: DoctorStatus::Fail,
            title: "Arandu toolchain".into(),
            details: vec![
                DoctorDetail::Error(format!("could not resolve current_exe(): {e}")),
                DoctorDetail::Hint(
                    "reinstall the arandu binary or check PATH / install prefix".into(),
                ),
            ],
        },
    });

    // [Stdlib]
    categories.push(
        match resolve_stdlib_root(StdlibResolveOpts {
            explicit: flags.stdlib_path.clone(),
            ..Default::default()
        }) {
            Ok(root) => {
                let mut details = vec![
                    DoctorDetail::Info(format!("stdlib at {}", root.path.display())),
                    DoctorDetail::Info(format!("resolved via {}", root.source)),
                ];
                if flags.verbose {
                    details.push(DoctorDetail::Info(
                        "cascade: --stdlib-path > ARANDU_STDLIB > relative to binary (never cwd)"
                            .into(),
                    ));
                }
                DoctorCategory {
                    status: DoctorStatus::Ok,
                    title: "Stdlib".into(),
                    details,
                }
            }
            Err(e) => {
                let mut details = vec![DoctorDetail::Error(e.to_string().replace('\n', " "))];
                // Expand "tried" lines as nested bullets when verbose.
                if flags.verbose {
                    for line in e.tried {
                        details.push(DoctorDetail::Info(line));
                    }
                }
                details.push(DoctorDetail::Hint(format!(
                "pass --stdlib-path=<dir>, set {STDLIB_ENV}, or install under share/arandu/stdlib"
            )));
                DoctorCategory {
                    status: DoctorStatus::Fail,
                    title: "Stdlib".into(),
                    details,
                }
            }
        },
    );

    // [Project] Arandu.toml (optional when not in a package)
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    categories.push(match find_manifest(&cwd) {
        Ok(Some(discovery)) => {
            let path = discovery.path;
            match load_manifest(&path) {
                Ok((data, hash, _)) => {
                    let mut details = vec![
                        DoctorDetail::Info(format!("manifest at {}", path.display())),
                        DoctorDetail::Info(format!(
                            "package {} {}  entry={}",
                            data.name, data.version, data.entry
                        )),
                    ];
                    if discovery.spelling == ManifestSpelling::Legacy {
                        details.push(DoctorDetail::Error(format!(
                            "legacy manifest name `{}`; rename it to `{MANIFEST_FILENAME}`",
                            arandu_query::LEGACY_MANIFEST_FILENAME
                        )));
                    }
                    let toolchain_error =
                        ensure_toolchain_compatible(&path, &data, ARANDU_VERSION).err();
                    if let Some(error) = &toolchain_error {
                        details.push(DoctorDetail::Error(error.to_string()));
                    }
                    if flags.verbose {
                        details.push(DoctorDetail::Info(format!(
                            "schema={} edition={:?} kind={:?}",
                            data.schema, data.edition, data.kind
                        )));
                        details.push(DoctorDetail::Info(format!(
                            "content hash {}…",
                            &hash[..12.min(hash.len())]
                        )));
                        let entry = path
                            .parent()
                            .unwrap_or_else(|| Path::new("."))
                            .join(&data.entry);
                        if entry.is_file() {
                            details.push(DoctorDetail::Info(format!(
                                "entry file ok ({})",
                                entry.display()
                            )));
                        } else {
                            details.push(DoctorDetail::Error(format!(
                                "entry file missing ({})",
                                entry.display()
                            )));
                        }
                    }
                    let entry_ok = path
                        .parent()
                        .map(|p| p.join(&data.entry).is_file())
                        .unwrap_or(false);
                    DoctorCategory {
                        status: if entry_ok
                            && discovery.spelling == ManifestSpelling::Canonical
                            && toolchain_error.is_none()
                        {
                            DoctorStatus::Ok
                        } else {
                            DoctorStatus::Partial
                        },
                        title: format!("Project ({MANIFEST_FILENAME})"),
                        details: {
                            let mut d = details;
                            if !entry_ok {
                                d.push(DoctorDetail::Error(format!(
                                    "entry `{}` does not exist on disk",
                                    data.entry
                                )));
                                d.push(DoctorDetail::Hint(
                                    "fix the entry path in Arandu.toml or create the file".into(),
                                ));
                            }
                            d
                        },
                    }
                }
                Err(e) => DoctorCategory {
                    status: DoctorStatus::Fail,
                    title: format!("Project ({MANIFEST_FILENAME})"),
                    details: vec![
                        // BUG-09: never swallow parse errors
                        DoctorDetail::Error(e.to_string()),
                        DoctorDetail::Hint(
                            "fix the TOML (required: name, version, entry as quoted strings)"
                                .into(),
                        ),
                    ],
                },
            }
        }
        Ok(None) => DoctorCategory {
            status: DoctorStatus::Skip,
            title: format!("Project ({MANIFEST_FILENAME})"),
            details: vec![
                DoctorDetail::Info(format!("no package found from {}", cwd.display())),
                DoctorDetail::Info("not an error outside a project directory".into()),
                DoctorDetail::Hint("run `arandu_cli new <name>` to scaffold a package".into()),
            ],
        },
        Err(error) => DoctorCategory {
            status: DoctorStatus::Fail,
            title: format!("Project ({MANIFEST_FILENAME})"),
            details: vec![DoctorDetail::Error(error.to_string())],
        },
    });

    // [Cranelift] dev backend
    categories.push(
        match arandu_backend_cranelift::CraneliftBackend::try_new() {
            Ok(_) => DoctorCategory {
                status: DoctorStatus::Ok,
                title: "Cranelift backend (dev JIT)".into(),
                details: vec![
                    DoctorDetail::Info("ISA initialized".into()),
                    DoctorDetail::Info("used by `run` and `build` (default)".into()),
                ],
            },
            Err(diag) => DoctorCategory {
                status: DoctorStatus::Fail,
                title: "Cranelift backend (dev JIT)".into(),
                details: vec![
                    DoctorDetail::Error(format!("failed to initialize ISA ({})", diag.message)),
                    DoctorDetail::Hint(
                        "run `arandu_cli run <file.aru> -Zdebug-backend` for more detail".into(),
                    ),
                ],
            },
        },
    );

    // [LLVM] release backend (reserved convention)
    categories.push(DoctorCategory {
        status: DoctorStatus::Skip,
        title: "LLVM backend (release)".into(),
        details: vec![
            DoctorDetail::Info("not implemented yet".into()),
            DoctorDetail::Info(
                "convention is fixed: `build` → Cranelift, `build --release` → LLVM".into(),
            ),
            DoctorDetail::Hint(
                "`arandu_cli build --release` exits with a clear error until LLVM lands".into(),
            ),
        ],
    });

    // Env extras only in verbose
    if flags.verbose {
        if let Ok(val) = std::env::var(STDLIB_ENV) {
            categories.push(DoctorCategory {
                status: DoctorStatus::Ok,
                title: format!("Environment ({STDLIB_ENV})"),
                details: vec![DoctorDetail::Info(val)],
            });
        }
    }

    // ── Print Flutter-style report ──────────────────────────────────────
    if flags.verbose {
        println!("Doctor summary (verbose):");
    } else {
        println!("Doctor summary (to see all details, run arandu_cli doctor -v):");
    }
    println!();

    let mut issues = 0usize;
    for cat in &categories {
        if matches!(cat.status, DoctorStatus::Fail | DoctorStatus::Partial) {
            issues += 1;
        }
        print_category(cat, color, flags.verbose);
        println!();
    }

    if issues == 0 {
        println!("{} No issues found!", bullet_ok(color));
        0
    } else {
        println!(
            "{} Doctor found issues in {issues} categor{}.",
            bullet_warn(color),
            if issues == 1 { "y" } else { "ies" }
        );
        1
    }
}

#[derive(Clone, Copy)]
enum DoctorStatus {
    Ok,
    Partial,
    Fail,
    Skip,
}

struct DoctorCategory {
    status: DoctorStatus,
    title: String,
    details: Vec<DoctorDetail>,
}

enum DoctorDetail {
    Info(String),
    Error(String),
    Hint(String),
}

fn use_color() -> bool {
    use std::io::IsTerminal;
    std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none()
}

fn paint(color: bool, code: &str, text: &str) -> String {
    if color {
        format!("\x1b[{code}m{text}\x1b[0m")
    } else {
        text.to_string()
    }
}

fn status_tag(status: DoctorStatus, color: bool) -> String {
    match status {
        DoctorStatus::Ok => paint(color, "32", "[✓]"),
        DoctorStatus::Partial => paint(color, "33", "[!]"),
        DoctorStatus::Fail => paint(color, "31", "[✗]"),
        DoctorStatus::Skip => paint(color, "90", "[-]"),
    }
}

fn bullet_ok(color: bool) -> String {
    paint(color, "32", "•")
}

fn bullet_warn(color: bool) -> String {
    paint(color, "33", "!")
}

fn print_category(cat: &DoctorCategory, color: bool, verbose: bool) {
    println!("{} {}", status_tag(cat.status, color), cat.title);
    let show_all = verbose || matches!(cat.status, DoctorStatus::Fail | DoctorStatus::Partial);
    if !show_all && !verbose {
        // Compact mode: one-line category is enough when healthy; still show
        // first info line for Skip so users know why it is blank.
        if matches!(cat.status, DoctorStatus::Skip) {
            if let Some(DoctorDetail::Info(msg)) = cat.details.first() {
                println!("    • {msg}");
            }
        }
        return;
    }
    for d in &cat.details {
        match d {
            DoctorDetail::Info(msg) => println!("    • {msg}"),
            DoctorDetail::Error(msg) => {
                println!("    {} {msg}", paint(color, "31", "✗"));
            }
            DoctorDetail::Hint(msg) => {
                println!("    {} {msg}", paint(color, "36", "→"));
            }
        }
    }
}

/// Load project from `start` (file, dir, or cwd) and register Salsa inputs.
pub fn load_project(
    db: &mut arandu_query::DatabaseImpl,
    start: &Path,
    flags: &ProjectFlags,
) -> Result<ProjectContext, String> {
    let discovery = find_manifest(start)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| {
            format!(
                "no {MANIFEST_FILENAME} found from {} — run `arandu_cli new <name>` or pass a path to a package",
                start.display()
            )
        })?;
    if discovery.spelling == ManifestSpelling::Legacy {
        eprintln!(
            "warning: `{}` is deprecated; rename it to `{MANIFEST_FILENAME}`",
            arandu_query::LEGACY_MANIFEST_FILENAME
        );
    }
    let manifest_path = discovery.path;

    let (data, hash, _bytes) =
        load_manifest(&manifest_path).map_err(|e: ManifestError| e.to_string())?;
    ensure_toolchain_compatible(&manifest_path, &data, ARANDU_VERSION)
        .map_err(|error| error.to_string())?;

    let root = manifest_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let canonical_root = fs::canonicalize(&root).map_err(|error| {
        format!(
            "cannot canonicalize project root {}: {error}",
            root.display()
        )
    })?;

    let cache = arandu_package::cache::resolve_cache_layout(flags.cache_dir.as_deref())?;
    let remote_packages = arandu_package::resolver::materialize_remote_graph(
        &canonical_root,
        &manifest_path,
        &data,
        &cache,
        arandu_package::resolver::ResolutionPolicy {
            locked: flags.locked,
            offline: flags.offline,
        },
    )?;
    let package_graph = LocalPackageGraph::discover_materialized(
        &canonical_root,
        &manifest_path,
        &data,
        &remote_packages,
    )?;
    let lockfile = package_graph.lockfile(&data);
    synchronize_lockfile(&canonical_root, lockfile.clone(), flags)?;

    let entry_candidate = canonical_root.join(&data.entry);
    let entry_path = fs::canonicalize(&entry_candidate).map_err(|error| {
        format!(
            "entry `{}` from {} does not exist (resolved to {}): {error}",
            data.entry,
            manifest_path.display(),
            entry_candidate.display()
        )
    })?;
    if !entry_path.starts_with(&canonical_root) || !entry_path.is_file() {
        return Err(format!(
            "entry `{}` from {} escapes the project root or is not a file (resolved to {})",
            data.entry,
            manifest_path.display(),
            entry_path.display(),
        ));
    }

    let stdlib = resolve_stdlib_root(StdlibResolveOpts {
        explicit: flags.stdlib_path.clone(),
        ..Default::default()
    })
    .map_err(|e| e.to_string())?;
    db.set_stdlib_root(stdlib.path.clone());

    // Package source root = directory containing the entry file (usually `src/`).
    let package_src = entry_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| root.clone());
    let entries = scan_aru_entries(&package_src);
    let listing = DirectoryListing::new(
        db,
        std::sync::Arc::new(package_src.clone()),
        std::sync::Arc::new(entries),
    );
    let roots = ModuleRoots::new(
        db,
        data.name.clone(),
        std::sync::Arc::new(package_src),
        Some(std::sync::Arc::new(stdlib.path.clone())),
        listing,
    );
    db.set_module_roots(roots);
    install_package_module_map(db, &package_graph, &entry_path)?;

    let name = data.name.clone();
    let version = data.version.clone();
    let entry_rel = data.entry.clone();

    let manifest = register_manifest(db, manifest_path.clone(), data, hash);
    // Touch tracked fingerprint so the input is live in the Salsa graph.
    let _fp = arandu_query::manifest_fingerprint(db, manifest);
    db.set_project_manifest(manifest);

    Ok(ProjectContext {
        root: canonical_root,
        manifest_path,
        manifest,
        entry_path,
        stdlib,
        cache,
        lockfile,
        name,
        version,
        entry_rel,
    })
}

fn synchronize_lockfile(
    root: &Path,
    expected: arandu_query::Lockfile,
    flags: &ProjectFlags,
) -> Result<(), String> {
    // Serialize the complete read/compare/publish transaction. A persistent
    // lock inode is intentional: deleting it after unlock can let two
    // processes lock different inodes. The OS releases this lock on crash.
    let lock_dir = root.join(".arandu").join("locks");
    fs::create_dir_all(&lock_dir)
        .map_err(|error| format!("cannot create {}: {error}", lock_dir.display()))?;
    let lock_path = lock_dir.join("lockfile");
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|error| format!("cannot open {}: {error}", lock_path.display()))?;
    lock.lock()
        .map_err(|error| format!("cannot lock {}: {error}", lock_path.display()))?;

    let path = root.join(arandu_query::LOCK_FILENAME);
    let expected_bytes = expected.to_canonical_bytes();
    let mut previous = None;
    match fs::read(&path) {
        Ok(bytes) => {
            let text = std::str::from_utf8(&bytes).map_err(|error| {
                format!("invalid {}: file is not UTF-8: {error}", path.display())
            })?;
            let current =
                arandu_query::Lockfile::parse(&path, text).map_err(|error| error.to_string())?;
            if current == expected && bytes == expected_bytes {
                return Ok(());
            }
            if flags.locked {
                return Err(format!(
                    "{} is stale or noncanonical and --locked forbids updating it",
                    path.display()
                ));
            }
            previous = Some(current);
        }
        Err(error) if error.kind() == ErrorKind::NotFound => {
            if flags.locked {
                return Err(format!(
                    "{} is missing and --locked forbids creating it",
                    path.display()
                ));
            }
        }
        Err(error) => return Err(format!("cannot read {}: {error}", path.display())),
    }
    let remote_authority_changes = expected.contains_remote_packages()
        || previous
            .as_ref()
            .is_some_and(arandu_query::Lockfile::contains_remote_packages);
    if remote_authority_changes {
        let empty = arandu_query::Lockfile {
            manifest_fingerprint:
                "blake3:0000000000000000000000000000000000000000000000000000000000000000".into(),
            packages: Vec::new(),
        };
        let current = previous.as_ref().unwrap_or(&empty);
        let diff = current.graph_diff(&expected).join("\n");
        if !flags.accept_lock {
            return Err(format!(
                "remote dependency graph requires explicit review:\n{diff}\nreview the complete diff, then run 'arandu update --accept'"
            ));
        }
        eprintln!("accepting reviewed remote dependency graph:\n{diff}");
    }
    crate::artifact::atomic_replace(&path, &expected_bytes).map_err(|error| match error {
        CliFailure::Operational {
            operation,
            context,
            source,
        } => format!(
            "{operation}{}: {source}",
            context
                .map(|path| format!(" {}", path.display()))
                .unwrap_or_default()
        ),
        other => format!("unexpected lockfile publication failure: {other:?}"),
    })
}

fn install_package_module_map(
    db: &mut arandu_query::DatabaseImpl,
    graph: &LocalPackageGraph,
    entry_path: &Path,
) -> Result<(), String> {
    let Some(plan) = graph.module_plan(entry_path)? else {
        return Ok(());
    };
    let mut bindings = BTreeMap::new();
    let mut files = BTreeMap::new();
    for planned in plan.bindings {
        let file = if let Some(file) = files.get(&planned.physical) {
            *file
        } else {
            let text = fs::read_to_string(&planned.physical).map_err(|error| {
                format!("cannot read module {}: {error}", planned.physical.display())
            })?;
            let file = db.new_file(planned.physical.to_string_lossy().into_owned(), text);
            files.insert(planned.physical.clone(), file);
            file
        };
        bindings.insert(
            planned.logical,
            arandu_query::ModuleBinding {
                package: planned.package,
                target: planned.target,
                module: planned.module,
                file,
            },
        );
    }

    let map = arandu_query::PackageModuleMap::new(
        db,
        plan.current_package,
        plan.current_target,
        std::sync::Arc::new(bindings.into_iter().collect()),
    );
    db.set_package_module_map(map);
    Ok(())
}

/// Backend selection convention (roadmap 4.1 dual backend).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendChoice {
    /// Fast host path — Cranelift JIT for `run`, AOT object/link for `build`.
    Cranelift,
    /// Future release path — `build --release` (not implemented yet).
    LlvmReserved,
}

impl BackendChoice {
    #[must_use]
    pub fn from_release_flag(release: bool) -> Self {
        if release {
            BackendChoice::LlvmReserved
        } else {
            BackendChoice::Cranelift
        }
    }

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            BackendChoice::Cranelift => "cranelift",
            BackendChoice::LlvmReserved => "llvm (reserved)",
        }
    }
}

#[cfg(test)]
mod lock_review_tests {
    use super::*;
    use arandu_query::{LockedPackage, Lockfile};

    fn remote_lock(commit: &str, digest: &str) -> Lockfile {
        let source = format!("git+https://example.com/math.git#{commit}");
        Lockfile {
            manifest_fingerprint:
                "blake3:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
            packages: vec![LockedPackage {
                name: "math".into(),
                version: "1.0.0".into(),
                source,
                manifest_fingerprint:
                    "blake3:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into(),
                origin: Some("https://example.com/math.git".into()),
                commit: Some(commit.into()),
                content_digest: Some(digest.into()),
                dependencies: Vec::new(),
            }],
        }
    }

    #[test]
    fn remote_graph_requires_review_then_explicit_acceptance() {
        let root = std::env::temp_dir().join(format!(
            "arandu-lock-review-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        fs::create_dir_all(&root).expect("temp project");
        let commit = "0123456789abcdef0123456789abcdef01234567";
        let digest = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let expected = remote_lock(commit, digest);
        let error = synchronize_lockfile(&root, expected.clone(), &ProjectFlags::default())
            .expect_err("first remote trust must not be implicit");
        assert!(error.contains("+ package git+https://example.com/math.git"));
        assert!(error.contains("arandu update --accept"));
        assert!(!root.join(arandu_query::LOCK_FILENAME).exists());

        synchronize_lockfile(
            &root,
            expected.clone(),
            &ProjectFlags {
                accept_lock: true,
                ..ProjectFlags::default()
            },
        )
        .expect("explicit acceptance publishes lock");
        let bytes = fs::read(root.join(arandu_query::LOCK_FILENAME)).expect("published lock");
        assert_eq!(bytes, expected.to_canonical_bytes());

        let next = remote_lock(
            "1111111111111111111111111111111111111111",
            "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
        );
        let error = synchronize_lockfile(&root, next, &ProjectFlags::default())
            .expect_err("remote update must not be implicit");
        assert!(error.contains("- package "));
        assert!(error.contains("+ package "));
        assert_eq!(
            fs::read(root.join(arandu_query::LOCK_FILENAME)).expect("old lock retained"),
            bytes
        );
        fs::remove_dir_all(root).expect("cleanup");
    }
}
