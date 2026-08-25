//! Project-mode CLI: `new`, `doctor`, package `check`/`run`/`build`.
//!
//! Gold bars:
//! - stdlib via [`arandu_query::resolve_stdlib_root`] (`current_exe`, never cwd)
//! - `Arandu.toml` as Salsa [`ProjectManifest`] input (hash in invalidation key)
//! - `arandu doctor` diagnoses env using the same init points as compile
//! - `build` default = Cranelift; `--release` reserved for future LLVM

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::cli_error::{CliFailure, CliResult, CliSuccess};
use arandu_query::{
    DirectoryListing, MANIFEST_FILENAME, ManifestError, ManifestSpelling, ModuleRoots,
    ProjectManifest, STDLIB_ENV, StdlibResolveOpts, StdlibRoot, ensure_toolchain_compatible,
    find_manifest, load_manifest, register_manifest, resolve_stdlib_root, scan_aru_entries,
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
    pub name: String,
    pub version: String,
    pub entry_rel: String,
}

/// Shared flags for project / doctor commands.
#[derive(Debug, Clone, Default)]
pub struct ProjectFlags {
    pub stdlib_path: Option<PathBuf>,
    pub release: bool,
    pub verbose: bool,
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
        } else if a == "--release" {
            flags.release = true;
        } else if a == "-v" || a == "--verbose" {
            flags.verbose = true;
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

    let entry_path = root.join(&data.entry);
    if !entry_path.is_file() {
        return Err(format!(
            "entry `{}` from {} does not exist (resolved to {})",
            data.entry,
            manifest_path.display(),
            entry_path.display()
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

    let name = data.name.clone();
    let version = data.version.clone();
    let entry_rel = data.entry.clone();

    let manifest = register_manifest(db, manifest_path.clone(), data, hash);
    // Touch tracked fingerprint so the input is live in the Salsa graph.
    let _fp = arandu_query::manifest_fingerprint(db, manifest);
    db.set_project_manifest(manifest);

    Ok(ProjectContext {
        root,
        manifest_path,
        manifest,
        entry_path,
        stdlib,
        name,
        version,
        entry_rel,
    })
}

/// Backend selection convention (roadmap 4.1 dual backend).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendChoice {
    /// Fast dev path (Cranelift JIT) — default `build` / `run`.
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
