//! Project-mode CLI: `new`, `doctor`, package `check`/`run`/`build`.
//!
//! Gold bars:
//! - stdlib via [`arandu_query::resolve_stdlib_root`] (`current_exe`, never cwd)
//! - `Arandu.toml` as Salsa [`ProjectManifest`] input (hash in invalidation key)
//! - `arandu doctor` diagnoses env using the same init points as compile
//! - `build` default = Cranelift; `--release` reserved for future LLVM

use std::fs;
use std::path::{Path, PathBuf};

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
pub fn cmd_new(name: &str) -> CliResult {
    if name.is_empty() || name.contains('/') || name.contains('\\') || name == "." || name == ".." {
        return Err(CliFailure::usage(format!(
            "invalid project name `{name}` (use a single path segment)"
        )));
    }
    let root = PathBuf::from(name);
    if root.exists() {
        return Err(CliFailure::operational(
            "create project",
            Some(root),
            "path already exists",
        ));
    }
    fs::create_dir_all(root.join("src")).map_err(|e| {
        CliFailure::operational(
            "create project directories",
            Some(root.clone()),
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
    let main_src = TEMPLATE_MAIN_ARU.replace("module my_app", &format!("module {name}"));

    let manifest_path = root.join(MANIFEST_FILENAME);
    let entry_path = root.join(DEFAULT_ENTRY);
    fs::write(&manifest_path, toml).map_err(|e| {
        CliFailure::operational("write project manifest", Some(manifest_path), e.to_string())
    })?;
    fs::write(&entry_path, main_src).map_err(|e| {
        CliFailure::operational("write project entry", Some(entry_path), e.to_string())
    })?;

    println!("created {name}/");
    println!("  {MANIFEST_FILENAME}");
    println!("  {DEFAULT_ENTRY}");
    println!();
    println!("next:");
    println!("  cd {name}");
    println!("  arandu check");
    println!("  arandu run");
    Ok(CliSuccess::Done)
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
