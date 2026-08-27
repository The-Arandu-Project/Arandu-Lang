#![allow(clippy::collapsible_if)]
mod artifact;
mod cli_error;
mod linker;
mod project;
mod test_runner;
mod watch;

use arandu_package::cache;
use arandu_query::ArandCompilerDb;
use cli_error::{CliFailure, CliResult, CliSuccess};

use std::{
    env, fs,
    path::{Path, PathBuf},
    process,
};

fn print_diagnostics_and_exit(
    diagnostics: impl IntoIterator<Item = arandu_middle::Diagnostic>,
    filepath: &str,
) -> ! {
    let source_path = (!filepath.is_empty()).then(|| PathBuf::from(filepath));
    finish(Err(CliFailure::diagnostics(diagnostics, source_path)))
}

fn finish(result: CliResult) -> ! {
    let code = match result {
        Ok(success) => success.exit_code(),
        Err(failure) => {
            failure.render();
            failure.exit_code()
        }
    };
    arandu_base::print_perf_summary();
    arandu_base::finalize_self_profile();
    process::exit(code);
}

fn fail_usage(message: impl Into<String>) -> ! {
    finish(Err(CliFailure::usage(message)))
}

fn fail_operational(
    operation: &'static str,
    context: Option<PathBuf>,
    source: impl Into<String>,
) -> ! {
    finish(Err(CliFailure::operational(operation, context, source)))
}

fn print_parse_error_and_exit(err: &arandu_parser::ParseError, filepath: &str) -> ! {
    let diag = arandu_middle::Diagnostic::from(err.clone());
    print_diagnostics_and_exit(std::iter::once(diag), filepath);
}

fn optimize_amir_or_exit(
    amir: &mut arandu_semantics::amir::AmirProgram,
    type_check: &arandu_semantics::TypeCheckResult,
    filepath: &str,
) {
    if let Err(diag) = arandu_semantics::optimize_amir_checked(
        amir,
        type_check.symbols.as_ref(),
        &type_check.type_info.type_interner,
    ) {
        print_diagnostics_and_exit(std::iter::once(diag), filepath);
    }
}

fn validate_hir_and_monomorphize(
    hir: &mut arandu_semantics::hir::HirProgram,
    type_check: &mut arandu_semantics::TypeCheckResult,
    filepath: &str,
) {
    if let Err(err) = hir.validate_invariants(&hir.pool, &type_check.symbols) {
        let diag = arandu_middle::Diagnostic::ice(
            arandu_middle::DiagCode::ICEL001,
            format!("HIR invariant violation before monomorphization: {err}"),
            arandu_middle::Span::new(0, 0, 0),
        );
        print_diagnostics_and_exit(std::iter::once(diag), filepath);
    }

    if let Err(diags) =
        arandu_semantics::passes::monomorphize::monomorphize_program(type_check, hir)
    {
        print_diagnostics_and_exit(diags, filepath);
    }
}

struct CheckedProgram {
    /// Shared with Salsa memo — never deep-clone the AST.
    program: std::sync::Arc<arandu_parser::Program>,
    type_check: arandu_semantics::TypeCheckResult,
}

/// Render non-fatal Salsa diagnostics or terminate through the typed diagnostic path.
fn handle_accumulated_diags(
    diags: &[impl std::ops::Deref<Target = arandu_middle::db::DiagnosticsAccumulator>],
    filepath: &str,
) {
    if diags.is_empty() {
        return;
    }
    let diagnostics: Vec<_> = diags.iter().map(|d| d.0.clone()).collect();
    if diagnostics
        .iter()
        .any(|d| matches!(d.severity, arandu_middle::Severity::Error))
    {
        print_diagnostics_and_exit(diagnostics, filepath);
    }
    let source = std::fs::read_to_string(filepath).unwrap_or_default();
    let named_source = miette::NamedSource::new(filepath, source);
    for diagnostic in diagnostics {
        let report = miette::Report::new(diagnostic).with_source_code(named_source.clone());
        eprintln!("{:?}", report);
    }
}

/// Single pipeline entry for check / run / amir / emit-c:
/// parse → type_check (diags once) → lower_amir (lower diags once).
/// Salsa memos each step; no second HIR/mono outside the query.
fn pipeline_lower(
    db: &dyn arandu_query::db::ArandCompilerDb,
    file: arandu_query::db::SourceFile,
    filepath: &str,
) -> std::sync::Arc<arandu_query::LowerAmirArtifacts> {
    {
        arandu_base::time_pass!("parse");
        let program_res = arandu_query::passes::parse(db, file);
        if let Err(err) = &**program_res {
            print_parse_error_and_exit(err, filepath);
        }
    }

    {
        arandu_base::time_pass!("type_check");
        let _ = arandu_query::passes::type_check(db, file);
    }
    let type_diags = arandu_query::passes::type_check::accumulated::<
        arandu_middle::db::DiagnosticsAccumulator,
    >(db, file);
    handle_accumulated_diags(&type_diags, filepath);

    let artifacts = {
        arandu_base::time_pass!("lower-amir");
        arandu_query::passes::lower_amir(db, file)
    };
    let lower_diags = arandu_query::passes::lower_amir::accumulated::<
        arandu_middle::db::DiagnosticsAccumulator,
    >(db, file);
    handle_accumulated_diags(&lower_diags, filepath);

    std::sync::Arc::clone(&artifacts.value)
}

/// Opt-in GenRef observability, intentionally outside Salsa queries. This
/// derives metrics from the immutable AMIR artifact and has no query-visible
/// side effects.
fn print_genref_report(filepath: &str, artifacts: &arandu_query::LowerAmirArtifacts) {
    use arandu_middle::amir::{AmirRvalue, AmirStmt};

    let mut total_promotions = 0usize;
    let mut total_checks = 0usize;
    let mut rows = Vec::new();
    for func in &artifacts.amir.funcs {
        let mut promotions = 0usize;
        let mut checks = 0usize;
        for stmt in func.stmts.payloads.iter() {
            let AmirStmt::Assign { rhs, .. } = stmt else {
                continue;
            };
            match rhs {
                AmirRvalue::GenInsert { .. } => promotions += 1,
                AmirRvalue::GenGet { .. }
                | AmirRvalue::GenSet { .. }
                | AmirRvalue::GenUpsert { .. }
                | AmirRvalue::GenRemove { .. } => checks += 1,
                _ => {}
            }
        }
        if promotions != 0 || checks != 0 {
            let name = artifacts.type_check.symbols.get(func.symbol).name.clone();
            rows.push((name, promotions, checks));
            total_promotions += promotions;
            total_checks += checks;
        }
    }
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    eprintln!(
        "[arandu][genref] module={filepath} promotions={total_promotions} checks={total_checks} functions={}",
        rows.len()
    );
    for (name, promotions, checks) in rows {
        eprintln!("[arandu][genref] function={name} promotions={promotions} checks={checks}");
    }
}

/// Parse + type-check for paths that still need a local TypeCheckResult (e.g. `hir`).
fn parse_and_check(
    db: &dyn arandu_query::db::ArandCompilerDb,
    file: arandu_query::db::SourceFile,
    filepath: &str,
) -> CheckedProgram {
    let program_res = {
        arandu_base::time_pass!("parse");
        arandu_query::passes::parse(db, file)
    };
    let program = match &**program_res {
        Ok(program) => std::sync::Arc::clone(program),
        Err(err) => print_parse_error_and_exit(err, filepath),
    };

    let type_check = {
        arandu_base::time_pass!("type_check");
        arandu_query::passes::type_check(db, file)
    };

    let diagnostics = arandu_query::passes::type_check::accumulated::<
        arandu_middle::db::DiagnosticsAccumulator,
    >(db, file);
    handle_accumulated_diags(&diagnostics, filepath);

    // TypeCheckResult is Arc-heavy (symbols/resolved/type_info) — clone is O(1) for IR.
    CheckedProgram {
        program,
        type_check: (**type_check).clone(),
    }
}

fn usage_and_exit() -> ! {
    let message = concat!(
        "usage:\n",
        "  arandu_cli <lex|parse|check|hir|amir|run|emit-c|graph|fmt> <path> [flags]\n",
        "  arandu_cli new <project-name> [--bin|--lib] [--vcs=auto|git|none]\n",
        "  arandu_cli init [--bin|--lib] [--vcs=auto|git|none]\n",
        "  arandu_cli doctor [--stdlib-path=<dir>] [-v]\n",
        "  arandu_cli cache <dir|inspect|verify|verify-tree|prune> [--cache-dir=<absolute-dir>] [limits]\n",
        "  arandu_cli hash-file <path>          # BLAKE3 hex (packaging checksums)\n",
        "  arandu_cli watch [package-path]      # re-check on FS changes (package mode)\n",
        "  arandu_cli test [package-path] --list [--exact <id>] # list compiler-validated tests\n",
        "  arandu_cli clean [package-path]      # remove owned project artifacts\n",
        "  arandu_cli tree [package-path]       # canonical resolved dependency graph\n",
        "  arandu_cli audit [package-path]      # locked provenance and policy audit\n",
        "  arandu_cli vendor [package-path]     # verified offline source snapshot\n",
        "  arandu_cli update [package-path] --accept # review and publish remote graph\n",
        "  arandu_cli verify [package-path]     # locked, offline cache verification\n",
        "  arandu_cli check|run|build [--release] [--stdlib-path=<dir>] [package-path]\n\n",
        "  emit-c options: --layout=host|ptr4|ptr8|i686  (default: host)\n",
        "                  layout model only; cross compiler/sysroot are external\n",
        "  G2/F2.3: --no-generational-fallback  (promote O004 notes to errors)\n",
        "  G5: --genref-report  (per-module/function promotion and check counts on stderr)\n",
        "  -Z flags: -Ztime-passes  -Zprofile-queries  -Zprint-alloc-stats  -Zdump-mir\n",
        "           : -Zdebug-parser -Zdebug-typeck -Zdebug-ossa -Zdebug-layout -Zdebug-backend -Zdebug-all\n",
        "           : -Zself-profile=<path>  -Zexplain-rebuild  -Zno-generational-fallback\n\n",
        "  backend: build → Cranelift (dev); build --release → LLVM when available\n",
        "  stdlib:  --stdlib-path > ARANDU_STDLIB > relative to binary (never cwd)\n",
        "  cache:   --cache-dir > ARANDU_CACHE_DIR > platform-native user cache"
    );
    finish(Err(CliFailure::usage(message)))
}

/// Attach resolved stdlib root to the DB (install cascade; never cwd-only).
fn attach_stdlib(db: &arandu_query::DatabaseImpl, explicit: Option<PathBuf>) {
    match arandu_query::resolve_stdlib_root(arandu_query::StdlibResolveOpts {
        explicit,
        ..Default::default()
    }) {
        Ok(root) => db.set_stdlib_root(root.path),
        Err(e) => {
            // Soft for single-file tools that only need prelude; hard later on import.
            // Doctor / project mode surface the hard error. Log once at debug.
            tracing::debug!("stdlib resolve deferred: {e}");
        }
    }
}

fn parse_data_layout(flags: &[String]) -> arandu_middle::layout::DataLayout {
    use arandu_middle::layout::DataLayout;
    for f in flags {
        if let Some(rest) = f.strip_prefix("--layout=") {
            return match rest {
                "host" => DataLayout::host(),
                "ptr4" | "32" => DataLayout::ptr_width(4),
                "i686" | "i686-sysv" => DataLayout::i686_sysv(),
                "ptr8" | "64" => DataLayout::ptr_width(8),
                other => {
                    fail_usage(format!(
                        "unknown --layout={other} (use host|ptr4|ptr8|i686)"
                    ));
                }
            };
        }
    }
    DataLayout::host()
}

fn find_aru_files(dir: &Path, files: &mut Vec<PathBuf>) -> std::io::Result<()> {
    if dir.is_dir() {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                find_aru_files(&path, files)?;
            } else if path.extension().and_then(|s| s.to_str()) == Some("aru") {
                files.push(path);
            }
        }
    }
    Ok(())
}

fn main() {
    let mut debug = false;
    let mut opt = false;
    let mut parallel = false;
    let mut genref_report = false;
    let mut args = Vec::new();
    let mut z_flags: Vec<String> = Vec::new();
    let mut layout_flags: Vec<String> = Vec::new();
    let mut raw_project_flags: Vec<String> = Vec::new();

    for arg in env::args() {
        match arg.as_str() {
            "--debug" => debug = true,
            "--opt" => opt = true,
            "--parallel" => parallel = true,
            "--genref-report" => genref_report = true,
            // G2: long form of -Zno-generational-fallback (same atomic).
            "--no-generational-fallback" => {
                z_flags.push("-Zno-generational-fallback".into());
            }
            s if s.starts_with("-Z") => z_flags.push(arg),
            s if s.starts_with("--layout=") => layout_flags.push(arg),
            // Collect project flags even before we know the subcommand.
            s if s.starts_with("--stdlib-path")
                || s.starts_with("--cache-dir")
                || s == "--release"
                || s == "-v"
                || s == "--verbose"
                || s == "--locked"
                || s == "--offline"
                || s == "--frozen"
                || s == "--accept" =>
            {
                raw_project_flags.push(arg);
            }
            _ => args.push(arg),
        }
    }
    let data_layout = parse_data_layout(&layout_flags);
    let (project_flags, extra_positional) = project::parse_project_flags(&raw_project_flags)
        .unwrap_or_else(|message| fail_usage(format!("error: {message}")));
    // positional extras from flag parser should not exist; merge any leftovers
    let _ = extra_positional;

    // Initialise global perf flags (written once, read-only afterwards).
    arandu_base::init_z_flags(&z_flags);

    // Initialise the tracing subscriber from -Zdebug-* / -Zself-profile flags.
    let tracing_cfg = arandu_base::build_tracing_config();
    arandu_base::tracing_bridge::init_tracing(tracing_cfg);

    if args.len() == 2 && matches!(args[1].as_str(), "--version" | "-V") {
        println!("arandu {}", env!("CARGO_PKG_VERSION"));
        finish(Ok(CliSuccess::Done));
    }

    if args.len() < 2 {
        usage_and_exit();
    }

    let command = args[1].as_str();
    if project_flags.accept_lock && command != "update" {
        fail_usage("--accept is valid only with 'arandu update'");
    }

    // ── Project / environment commands (no mandatory .aru path) ──────────
    match command {
        "new" => {
            if args.len() < 3 {
                fail_usage(
                    "usage: arandu_cli new <project-name> [--bin|--lib] [--vcs=auto|git|none]",
                );
            }
            let options = project::parse_scaffold_options(&args[3..])
                .unwrap_or_else(|error| fail_usage(format!("error: {error}")));
            finish(project::cmd_new(&args[2], options));
        }
        "init" => {
            let options = project::parse_scaffold_options(&args[2..])
                .unwrap_or_else(|error| fail_usage(format!("error: {error}")));
            let root = env::current_dir().unwrap_or_else(|error| {
                fail_operational("resolve current directory", None, error.to_string())
            });
            let name = root
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or_else(|| fail_usage("current directory has no valid UTF-8 package name"));
            finish(project::cmd_init(&root, name, options));
        }
        "doctor" => {
            if args.len() != 2 {
                fail_usage("usage: arandu_cli doctor [--stdlib-path=<dir>] [-v]");
            }
            finish(Ok(CliSuccess::ProgramExit(project::cmd_doctor(
                &project_flags,
            ))));
        }
        "cache" => {
            if args.len() < 3 {
                fail_usage("usage: arandu_cli cache <dir|inspect|verify|prune> [options]");
            }
            let layout = cache::resolve_cache_layout(project_flags.cache_dir.as_deref())
                .unwrap_or_else(|error| fail_usage(format!("error: {error}")));
            if args[2] == "dir" {
                if args.len() != 3 {
                    fail_usage("usage: arandu_cli cache dir [--cache-dir=<absolute-dir>]");
                }
                println!("{}", layout.root().display());
                finish(Ok(CliSuccess::Done));
            }
            let allow_dry_run = args[2] == "prune";
            if args[2] == "verify-tree" {
                if args.len() != 5 {
                    fail_usage(
                        "usage: arandu_cli cache verify-tree <archive-digest> <tree-digest>",
                    );
                }
                let archive: arandu_query::CacheDigest = args[3].parse().unwrap_or_else(|error| {
                    fail_usage(format!("error: invalid archive digest: {error}"))
                });
                let tree: arandu_query::CacheDigest = args[4].parse().unwrap_or_else(|error| {
                    fail_usage(format!("error: invalid tree digest: {error}"))
                });
                let report = cache::CacheStore::new(layout)
                    .verify_tree(archive, tree, cache::TreeLimits::default())
                    .unwrap_or_else(|error| {
                        fail_operational("verify extracted package tree", None, error.to_string())
                    });
                println!(
                    "tree={} files={} bytes={} depth={}",
                    report.digest, report.files, report.bytes, report.depth
                );
                finish(Ok(CliSuccess::Done));
            }
            let (limits, dry_run) = cache::parse_scan_flags(&args[3..], allow_dry_run)
                .unwrap_or_else(|error| fail_usage(format!("error: {error}")));
            let store = cache::CacheStore::new(layout);
            match args[2].as_str() {
                "inspect" => {
                    let report = store.inspect(limits).unwrap_or_else(|error| {
                        fail_operational("inspect package cache", None, error.to_string())
                    });
                    println!(
                        "archives={} bytes={} invalid={} staging={} quarantine={}",
                        report.archives,
                        report.archive_bytes,
                        report.invalid_entries,
                        report.staging_files,
                        report.quarantine_files
                    );
                    finish(Ok(CliSuccess::Done));
                }
                "verify" => {
                    let report = store.verify(limits).unwrap_or_else(|error| {
                        fail_operational("verify package cache", None, error.to_string())
                    });
                    println!(
                        "verified={} bytes={} corrupt={} invalid={}",
                        report.verified,
                        report.verified_bytes,
                        report.corrupt,
                        report.invalid_entries
                    );
                    let code = i32::from(report.corrupt != 0 || report.invalid_entries != 0);
                    finish(Ok(CliSuccess::ProgramExit(code)));
                }
                "prune" => {
                    let report = store.prune(limits, dry_run).unwrap_or_else(|error| {
                        fail_operational("prune package cache", None, error.to_string())
                    });
                    println!(
                        "files={} bytes={} dry_run={}",
                        report.files, report.bytes, report.dry_run
                    );
                    finish(Ok(CliSuccess::Done));
                }
                _ => fail_usage("usage: arandu_cli cache <dir|inspect|verify|prune> [options]"),
            }
        }
        "hash-file" => {
            if args.len() != 3 {
                fail_usage("usage: arandu_cli hash-file <path>");
            }
            finish(cmd_hash_file(Path::new(&args[2])));
        }
        "watch" => {
            let start = if args.len() >= 3 {
                PathBuf::from(&args[2])
            } else {
                env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
            };
            finish(watch::cmd_watch(&start, &project_flags));
        }
        "clean" => {
            let start = if args.len() >= 3 {
                PathBuf::from(&args[2])
            } else {
                env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
            };
            let discovery = arandu_query::find_manifest(&start)
                .unwrap_or_else(|error| {
                    fail_operational("discover project", Some(start.clone()), error.to_string())
                })
                .unwrap_or_else(|| {
                    fail_operational(
                        "discover project",
                        Some(start.clone()),
                        "no arandu.toml found",
                    )
                });
            arandu_query::load_manifest(&discovery.path).unwrap_or_else(|error| {
                fail_operational(
                    "load project manifest",
                    Some(discovery.path.clone()),
                    error.to_string(),
                )
            });
            let root = discovery.path.parent().unwrap_or_else(|| Path::new("."));
            let removed = artifact::clean(root).unwrap_or_else(|error| finish(Err(error)));
            if removed {
                println!("removed target");
            } else {
                println!("already clean");
            }
            finish(Ok(CliSuccess::Done));
        }
        "tree" | "verify" | "audit" | "vendor" | "update" => {
            let start = if args.len() >= 3 {
                PathBuf::from(&args[2])
            } else {
                env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
            };
            if command == "update" {
                let mut db = arandu_query::DatabaseImpl::default();
                let ctx =
                    project::load_project(&mut db, &start, &project_flags).unwrap_or_else(|e| {
                        fail_operational("review dependency graph", Some(start.clone()), e)
                    });
                println!("accepted graph {}", ctx.lockfile.manifest_fingerprint);
                finish(Ok(CliSuccess::Done));
            } else if command == "vendor" {
                let mut db = arandu_query::DatabaseImpl::default();
                let policy = project::ProjectFlags {
                    locked: true,
                    offline: true,
                    ..project_flags.clone()
                };
                let ctx = project::load_project(&mut db, &start, &policy).unwrap_or_else(|e| {
                    fail_operational("prepare verified vendor", Some(start.clone()), e)
                });
                let path =
                    arandu_package::vendor::materialize(&ctx.root, &ctx.cache, &ctx.lockfile)
                        .unwrap_or_else(|e| {
                            fail_operational("publish verified vendor", Some(ctx.root.clone()), e)
                        });
                println!("vendored locked graph at {}", path.display());
                finish(Ok(CliSuccess::Done));
            } else if command == "audit" {
                finish(cmd_project_audit(&start, &project_flags));
            } else {
                finish(cmd_project_inspect(
                    &start,
                    &project_flags,
                    command == "verify",
                ));
            }
        }
        "build" => {
            // Package mode: always project-oriented.
            let start = if args.len() >= 3 {
                PathBuf::from(&args[2])
            } else {
                env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
            };
            finish(cmd_project_build(&start, &project_flags, opt, debug));
        }
        "test" => {
            test_runner::install_ctrlc_handler();
            let mut start = None;
            let mut list = false;
            let mut exact = None;
            let mut filter = None;
            let mut harness_child = false;
            let mut runner = test_runner::RunnerOptions {
                jobs: 1,
                timeout: std::time::Duration::from_secs(300),
                fail_fast: false,
                seed: 0,
                format_json: false,
                output: None,
                target: None,
                backend: None,
            };
            let mut arguments = args[2..].iter();
            while let Some(argument) = arguments.next() {
                if argument == "--list" {
                    list = true;
                } else if argument == "--harness-child" {
                    harness_child = true;
                } else if argument == "--exact" {
                    exact = arguments.next().cloned();
                    if exact.is_none() {
                        fail_usage("usage: arandu_cli test [package-path] --list [--exact <id>]");
                    }
                } else if argument == "--fail-fast" {
                    runner.fail_fast = true;
                } else if argument == "--jobs" {
                    runner.jobs = arguments
                        .next()
                        .and_then(|value| value.parse().ok())
                        .filter(|jobs| *jobs > 0)
                        .unwrap_or_else(|| {
                            fail_usage("--jobs requires an integer greater than zero")
                        });
                } else if argument == "--timeout" {
                    let seconds = arguments
                        .next()
                        .and_then(|value| value.parse::<u64>().ok())
                        .filter(|seconds| *seconds > 0)
                        .unwrap_or_else(|| {
                            fail_usage("--timeout requires seconds greater than zero")
                        });
                    runner.timeout = std::time::Duration::from_secs(seconds);
                } else if argument == "--seed" {
                    runner.seed = arguments
                        .next()
                        .and_then(|value| value.parse().ok())
                        .unwrap_or_else(|| fail_usage("--seed requires an unsigned integer"));
                } else if argument == "--format" {
                    runner.format_json = match arguments.next().map(String::as_str) {
                        Some("json") => true,
                        Some("human") => false,
                        _ => fail_usage("--format requires 'human' or 'json'"),
                    };
                } else if argument == "--filter" {
                    filter = arguments.next().cloned();
                    if filter.is_none() {
                        fail_usage("--filter requires a literal substring");
                    }
                } else if argument == "--output" {
                    runner.output = arguments.next().map(PathBuf::from);
                    if runner.output.is_none() {
                        fail_usage("--output requires a file path");
                    }
                } else if argument.starts_with('-') || start.is_some() {
                    fail_usage("usage: arandu_cli test [package-path] --list [--exact <id>]");
                } else {
                    start = Some(PathBuf::from(argument));
                }
            }
            let start =
                start.unwrap_or_else(|| env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
            finish(cmd_project_test_list(
                &start,
                &project_flags,
                list,
                exact.as_deref(),
                filter.as_deref(),
                harness_child,
                &runner,
            ));
        }
        // Project-mode check/run when the path is a package (Arandu.toml) or omitted.
        "check" | "run"
            if args.len() == 2 || is_project_target(args.get(2).map(String::as_str)) =>
        {
            let start = if args.len() >= 3 {
                PathBuf::from(&args[2])
            } else {
                env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
            };
            let result = if command == "check" {
                cmd_project_check(&start, &project_flags, opt, debug)
            } else {
                cmd_project_run(&start, &project_flags, opt, debug)
            };
            finish(result);
        }
        _ => {}
    }

    // ── Legacy single-path commands ──────────────────────────────────────
    if args.len() != 3 {
        usage_and_exit();
    }

    if !matches!(
        command,
        "lex" | "parse" | "check" | "hir" | "amir" | "run" | "emit-c" | "graph" | "fmt"
    ) {
        usage_and_exit();
    }

    let path = Path::new(&args[2]);

    let mut paths = Vec::new();
    if path.is_dir() {
        if let Err(err) = find_aru_files(path, &mut paths) {
            fail_operational(
                "failed to list directory",
                Some(path.to_path_buf()),
                err.to_string(),
            );
        }
        paths.sort();
    } else {
        paths.push(path.to_path_buf());
    }

    if paths.is_empty() {
        fail_operational(
            "find Arandu sources",
            Some(path.to_path_buf()),
            "no .aru source files found",
        );
    }

    if command == "fmt" {
        let mut changed = 0usize;
        for p in &paths {
            let src = match fs::read_to_string(p) {
                Ok(s) => s,
                Err(err) => {
                    fail_operational("failed to read", Some(p.clone()), err.to_string());
                }
            };
            let formatted = arandu_fmt::format_source(&src);
            if formatted != src {
                if let Err(err) = fs::write(p, &formatted) {
                    fail_operational("failed to write", Some(p.clone()), err.to_string());
                }
                changed += 1;
                eprintln!("formatted {}", p.display());
            }
        }
        if changed == 0 {
            eprintln!("already formatted ({} file(s))", paths.len());
        }
        return;
    }

    let use_parallel = parallel || paths.len() > 1;

    if use_parallel {
        if matches!(command, "lex" | "parse" | "run" | "emit-c") {
            fail_operational(
                "run command",
                None,
                format!("parallel/multi-file mode is not supported for command '{command}'"),
            );
        }
    }

    // DX.5: always record rebuild events for `run` so we can print [cached]/[rebuilt].
    let explain = arandu_base::EXPLAIN_REBUILD.load(std::sync::atomic::Ordering::Relaxed);
    let want_status = command == "run" || explain;
    let (db, rebuild_log) = if want_status {
        let (db, log) = arandu_query::db::DatabaseImpl::with_rebuild_log();
        (db, Some(log))
    } else {
        (arandu_query::db::DatabaseImpl::new(), None)
    };
    attach_stdlib(&db, project_flags.stdlib_path.clone());
    let mut registry = arandu_base::SourceRegistry::default();

    let mut source_files = Vec::new();
    for p in &paths {
        match fs::read_to_string(p) {
            Ok(source) => {
                let filepath = p.to_string_lossy().into_owned();
                let file_id = registry.register(&filepath, &source);
                let code = std::sync::Arc::from(source.clone());
                let source_file = arandu_query::db::SourceFile::new(
                    &db,
                    file_id,
                    code,
                    std::sync::Arc::new(p.clone()),
                );
                db.register_source_file(filepath.clone(), source_file);
                source_files.push((source_file, filepath, source));
            }
            Err(err) => {
                fail_operational("failed to read", Some(p.clone()), err.to_string());
            }
        }
    }

    use rayon::prelude::*;
    let process_file = |source_file: arandu_query::db::SourceFile,
                        filepath: String,
                        source: String,
                        db: arandu_query::db::DatabaseImpl| {
        match command {
            "lex" => match arandu_lexer::lex_to_string(&source) {
                Ok(output) => println!("{output}"),
                Err(err) => {
                    fail_operational(
                        "lex source",
                        Some(PathBuf::from(&filepath)),
                        err.to_string(),
                    );
                }
            },

            "parse" => match arandu_parser::parse_to_string(&source) {
                Ok(output) => println!("{output}"),
                Err(err) => {
                    fail_operational(
                        "parse source",
                        Some(PathBuf::from(&filepath)),
                        err.to_string(),
                    );
                }
            },

            "check" => {
                // One pipeline: parse → typeck → lower_amir (Salsa memos; diags once each).
                let artifacts = pipeline_lower(&db, source_file, &filepath);
                if genref_report {
                    print_genref_report(&filepath, &artifacts);
                }
                tracing::info!(
                    "Compilation verified successfully — no errors found for {}",
                    filepath
                );
                println!("ok {}", filepath);
            }

            "hir" => {
                let mut checked = parse_and_check(&db, source_file, &filepath);
                let mut hir = {
                    arandu_base::time_pass!("lower-hir");
                    match arandu_semantics::lower_to_hir(&mut checked.type_check, &checked.program)
                    {
                        Ok(hir) => hir,
                        Err(diags) => print_diagnostics_and_exit(diags, &filepath),
                    }
                };
                validate_hir_and_monomorphize(&mut hir, &mut checked.type_check, &filepath);

                if debug {
                    println!("{hir:#?}");
                } else {
                    let ctx = arandu_semantics::hir::HirPrettyCtx {
                        pool: &hir.pool,
                        symbols: &checked.type_check.symbols,
                        show_spans: false,
                        type_interner: Some(&checked.type_check.type_info.type_interner),
                    };
                    println!("--- HIR for {} ---", filepath);
                    print!("{}", hir.pretty_print(&ctx));
                }
            }

            "amir" => {
                let artifacts = pipeline_lower(&db, source_file, &filepath);
                if genref_report {
                    print_genref_report(&filepath, &artifacts);
                }
                let symbols = artifacts.type_check.symbols.as_ref();
                let interner = &artifacts.type_check.type_info.type_interner;
                let mut amir_owned = if opt {
                    Some(artifacts.amir.clone())
                } else {
                    None
                };
                if let Some(ref mut amir) = amir_owned {
                    arandu_base::time_pass!("optimize-amir");
                    optimize_amir_or_exit(amir, &artifacts.type_check, &filepath);
                }
                let amir = match &amir_owned {
                    Some(a) => a,
                    None => &artifacts.amir,
                };

                if debug {
                    println!("{amir:#?}");
                } else {
                    println!("--- AMIR for {} ---", filepath);
                    print!("{}", amir.pretty_print(symbols, interner));
                }
            }

            "run" => {
                let artifacts = pipeline_lower(&db, source_file, &filepath);
                if genref_report {
                    print_genref_report(&filepath, &artifacts);
                }
                tracing::info!("AMIR lowering completed (Salsa: single pipeline)");

                // DX.5 one-liner: did Salsa re-execute work or hit memos?
                if let Some(log) = db.rebuild_log() {
                    eprintln!("{}", log.status_line());
                }

                let type_check = &artifacts.type_check;
                let mut amir_owned = if opt {
                    Some(artifacts.amir.clone())
                } else {
                    None
                };
                if let Some(ref mut amir) = amir_owned {
                    arandu_base::time_pass!("optimize-amir");
                    optimize_amir_or_exit(amir, type_check, &filepath);
                    tracing::info!("Optimisation passes applied");
                }
                let amir = match &amir_owned {
                    Some(a) => a,
                    None => &artifacts.amir,
                };

                use arandu_semantics::{CodegenBackend, CompiledCode};
                let output = {
                    let backend = {
                        arandu_base::time_pass!("codegen-init");
                        match arandu_backend_cranelift::CraneliftBackend::try_new() {
                            Ok(backend) => backend,
                            Err(diag) => {
                                print_diagnostics_and_exit(std::iter::once(diag), &filepath)
                            }
                        }
                    };
                    arandu_base::time_pass!("codegen-translate");
                    match CodegenBackend::compile(
                        backend,
                        amir,
                        type_check.symbols.as_ref(),
                        type_check.type_info.as_ref(),
                    ) {
                        Ok(out) => out,
                        Err(diag) => print_diagnostics_and_exit(std::iter::once(diag), &filepath),
                    }
                };
                tracing::info!("Machine code generated (Cranelift JIT backend)");

                let main_is_void = amir.funcs.iter().any(|f| {
                    let name = type_check.symbols.get(f.symbol).name.as_str();
                    if name != "main" {
                        return false;
                    }
                    matches!(
                        type_check.type_info.type_interner.resolve(f.return_type),
                        arandu_semantics::types::ArType::Void
                    )
                });
                let has_main = amir
                    .funcs
                    .iter()
                    .any(|f| type_check.symbols.get(f.symbol).name.as_str() == "main");
                if !has_main {
                    fail_operational(
                        "run program",
                        Some(PathBuf::from(&filepath)),
                        "'main' function not found in compiled program",
                    );
                }

                unsafe {
                    if main_is_void {
                        if let Some(main_fn) = CompiledCode::get_fn::<unsafe fn()>(&output, "main")
                        {
                            main_fn();
                            finish(Ok(CliSuccess::Done));
                        }
                    } else if let Some(main_fn) =
                        CompiledCode::get_fn::<unsafe fn() -> i32>(&output, "main")
                    {
                        let code = main_fn();
                        finish(Ok(CliSuccess::ProgramExit(code)));
                    }
                    fail_operational(
                        "run program",
                        Some(PathBuf::from(&filepath)),
                        "compiled module does not export a callable 'main' function",
                    );
                }
            }
            "emit-c" => {
                let artifacts = pipeline_lower(&db, source_file, &filepath);
                if genref_report {
                    print_genref_report(&filepath, &artifacts);
                }
                let type_check = &artifacts.type_check;
                let mut amir_owned = if opt {
                    Some(artifacts.amir.clone())
                } else {
                    None
                };
                if let Some(ref mut amir) = amir_owned {
                    arandu_base::time_pass!("optimize-amir");
                    optimize_amir_or_exit(amir, type_check, &filepath);
                }
                let amir = match &amir_owned {
                    Some(a) => a,
                    None => &artifacts.amir,
                };

                arandu_base::time_pass!("emit-c");
                let c_src = arandu_backend_c::emit_c(
                    amir,
                    type_check.symbols.as_ref(),
                    type_check.type_info.as_ref(),
                    &type_check.type_info.type_interner,
                    data_layout,
                )
                .unwrap_or_else(|diag| {
                    print_diagnostics_and_exit(std::iter::once(diag), &filepath)
                });
                print!("{c_src}");
            }

            "graph" => {
                use arandu_query::db::ArandCompilerDb;
                let dep_graph = arandu_query::passes::module_dependency_graph(&db, source_file);
                let mut dot_graph = petgraph::Graph::<String, ()>::new();
                let mut node_map = std::collections::HashMap::new();
                for node in dep_graph.node_indices() {
                    let file_id = dep_graph[node];
                    let path = db.file_path(file_id);
                    let path_str = path.to_string_lossy().into_owned();
                    let new_node = dot_graph.add_node(path_str);
                    node_map.insert(node, new_node);
                }
                for edge in dep_graph.edge_indices() {
                    let Some((source, target)) = dep_graph.edge_endpoints(edge) else {
                        continue;
                    };
                    if let (Some(&s), Some(&t)) = (node_map.get(&source), node_map.get(&target)) {
                        dot_graph.add_edge(s, t, ());
                    }
                }
                println!("{:?}", petgraph::dot::Dot::with_config(&dot_graph, &[]));
            }

            _ => {
                fail_usage("error: unknown command");
            }
        }
    };

    if use_parallel {
        let db_mutex = std::sync::Mutex::new(db);
        source_files
            .into_par_iter()
            .for_each(|(source_file, filepath, source)| {
                let thread_db = match db_mutex.lock() {
                    Ok(guard) => guard.clone(),
                    Err(poisoned) => poisoned.into_inner().clone(),
                };
                process_file(source_file, filepath, source, thread_db);
            });
    } else {
        for (source_file, filepath, source) in source_files {
            process_file(source_file, filepath, source, db.clone());
        }
    }

    if let Some(log) = rebuild_log {
        // Full chain only when -Zexplain-rebuild; run already printed status_line.
        let explain = arandu_base::EXPLAIN_REBUILD.load(std::sync::atomic::Ordering::Relaxed);
        if explain {
            eprint!("{}", log.format_chain(true));
        }
    }

    arandu_base::print_perf_summary();
    arandu_base::finalize_self_profile();
}

/// Print BLAKE3-256 hex of a file (packaging / install integrity).
fn cmd_hash_file(path: &Path) -> CliResult {
    match fs::read(path) {
        Ok(bytes) => {
            println!("{}", blake3::hash(&bytes).to_hex());
            Ok(CliSuccess::Done)
        }
        Err(error) => Err(CliFailure::operational(
            "failed to read",
            Some(path.to_path_buf()),
            error.to_string(),
        )),
    }
}

/// True when `path` should use package mode (dir with Arandu.toml, or the toml itself).
fn is_project_target(arg: Option<&str>) -> bool {
    let Some(arg) = arg else {
        return true;
    };
    let p = Path::new(arg);
    if p.file_name().and_then(|s| s.to_str()) == Some(arandu_query::MANIFEST_FILENAME) {
        return true;
    }
    if p.is_dir() && p.join(arandu_query::MANIFEST_FILENAME).is_file() {
        return true;
    }
    // Explicit: if a parent walk finds a manifest and the path is not a .aru file,
    // still allow package mode when the user points at the package root.
    matches!(arandu_query::find_manifest(p), Ok(Some(_)))
        && p.extension().and_then(|e| e.to_str()) != Some("aru")
}

fn open_entry_file(
    db: &arandu_query::DatabaseImpl,
    registry: &mut arandu_base::SourceRegistry,
    entry: &Path,
) -> (arandu_query::SourceFile, String) {
    let source = match fs::read_to_string(entry) {
        Ok(s) => s,
        Err(err) => {
            fail_operational("failed to read", Some(entry.to_path_buf()), err.to_string());
        }
    };
    let filepath = entry.to_string_lossy().into_owned();
    let file_id = registry.register(&filepath, &source);
    let code = std::sync::Arc::from(source);
    let source_file =
        arandu_query::SourceFile::new(db, file_id, code, std::sync::Arc::new(entry.to_path_buf()));
    db.register_source_file(filepath.clone(), source_file);
    (source_file, filepath)
}

fn cmd_project_check(
    start: &Path,
    flags: &project::ProjectFlags,
    _opt: bool,
    _debug: bool,
) -> CliResult {
    let (mut db, rebuild_log) = arandu_query::DatabaseImpl::with_rebuild_log();
    let ctx = match project::load_project(&mut db, start, flags) {
        Ok(c) => c,
        Err(e) => {
            return Err(CliFailure::operational(
                "load project",
                Some(start.into()),
                e,
            ));
        }
    };
    let mut registry = arandu_base::SourceRegistry::default();
    let (file, filepath) = open_entry_file(&db, &mut registry, &ctx.entry_path);
    let _ = pipeline_lower(&db, file, &filepath);
    eprintln!("{}", rebuild_log.status_line());
    println!("ok {} ({}/{})", filepath, ctx.name, ctx.version);
    Ok(CliSuccess::Done)
}

fn project_test_sources(ctx: &project::ProjectContext) -> Result<Vec<(PathBuf, String)>, String> {
    let mut sources = std::collections::BTreeMap::new();
    let source_root = ctx
        .entry_path
        .parent()
        .ok_or_else(|| format!("entry {} has no source directory", ctx.entry_path.display()))?;
    for (directory, target) in [
        (source_root.to_path_buf(), ctx.target_kind),
        (ctx.root.join("tests"), "test"),
    ] {
        if !directory.is_dir() {
            continue;
        }
        for relative in arandu_query::scan_aru_entries(&directory) {
            let candidate = directory.join(&relative);
            let physical = fs::canonicalize(&candidate).map_err(|error| {
                format!(
                    "cannot canonicalize test source {}: {error}",
                    candidate.display()
                )
            })?;
            if !physical.starts_with(&ctx.root) || !physical.is_file() {
                return Err(format!(
                    "test source {} escapes the project root or is not a file",
                    candidate.display()
                ));
            }
            let module = relative
                .strip_suffix(".aru")
                .unwrap_or(&relative)
                .replace('\\', "/");
            sources.insert(physical, format!("{target}::{module}"));
        }
    }
    Ok(sources.into_iter().collect())
}

fn cmd_project_test_list(
    start: &Path,
    flags: &project::ProjectFlags,
    list_only: bool,
    exact: Option<&str>,
    filter: Option<&str>,
    harness_child: bool,
    runner: &test_runner::RunnerOptions,
) -> CliResult {
    let mut db = arandu_query::DatabaseImpl::new();
    let ctx = project::load_project(&mut db, start, flags).map_err(|error| {
        CliFailure::operational("load test project", Some(start.to_path_buf()), error)
    })?;
    let sources = project_test_sources(&ctx).map_err(|error| {
        CliFailure::operational("discover test sources", Some(ctx.root.clone()), error)
    })?;
    let mut registry = arandu_codegen::testing::TestRegistry::default();
    for (path, module) in sources {
        let key = path.to_string_lossy().into_owned();
        let file = if let Some(existing) = db.as_source_db().resolve_module_path(&key) {
            existing
        } else {
            let text = fs::read_to_string(&path).map_err(|error| {
                CliFailure::operational("read test source", Some(path.clone()), error.to_string())
            })?;
            db.new_file(key, text)
        };
        let checked = arandu_query::passes::type_check(&db, file);
        if checked
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == arandu_middle::Severity::Error)
        {
            return Err(CliFailure::diagnostics(
                checked.diagnostics.clone(),
                Some(path),
            ));
        }
        for case in arandu_query::file_test_manifest(&db, file).iter() {
            let id = format!("{}::{module}::{}", ctx.name, case.name);
            registry.insert(arandu_codegen::testing::TestEntry {
                id,
                function: case.name.to_string(),
            });
        }
    }
    let cases: Vec<String> = registry.iter().map(|entry| entry.id.clone()).collect();
    let cases: Vec<String> = match filter {
        Some(filter) => cases
            .into_iter()
            .filter(|case| case.contains(filter))
            .collect(),
        None => cases,
    };
    let harness = if harness_child {
        None
    } else {
        Some(artifact::publish_test_harness(&ctx.root, &registry)?)
    };
    if let Some(exact) = exact {
        if !cases.iter().any(|case| case == exact) {
            return Err(CliFailure::operational(
                "select test case",
                Some(ctx.root),
                format!("test case `{exact}` was not found"),
            ));
        }
        if list_only {
            println!("{exact}");
        } else if harness_child {
            let start = std::time::Instant::now();
            let result = run_exact_test(&ctx, exact);
            let sequence: u64 = std::env::var("ARANDU_TEST_SEQUENCE")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            let status = if result.is_ok() {
                arandu_codegen::testing::TestStatus::Passed
            } else {
                arandu_codegen::testing::TestStatus::Failed
            };
            let failure = result
                .as_ref()
                .err()
                .map(|error| arandu_codegen::testing::TestFailure::simple(format!("{error:?}")));
            let event = arandu_codegen::testing::TestEventV1 {
                sequence,
                id: exact.to_string(),
                status,
                duration: start.elapsed(),
                stdout: arandu_codegen::testing::CapturedOutput {
                    bytes: Vec::new(),
                    truncated: false,
                },
                stderr: arandu_codegen::testing::CapturedOutput {
                    bytes: Vec::new(),
                    truncated: false,
                },
                failure,
            };
            let _ = test_runner::send_child_event(sequence, &event);
            return result;
        } else {
            let passed = test_runner::run_cases(&ctx.root, vec![exact.to_string()], runner)
                .map_err(|error| CliFailure::operational("run tests", None, error))?;
            if !passed {
                return Err(CliFailure::operational(
                    "run tests",
                    None,
                    "one or more tests failed",
                ));
            }
        }
    } else if list_only {
        for case in cases {
            println!("{case}");
        }
    } else {
        if !runner.format_json {
            let (harness_manifest, harness_c) = harness.as_ref().ok_or_else(|| {
                CliFailure::operational("run tests", None, "missing published harness")
            })?;
            eprintln!(
                "test harness: {} cases (manifest={}, c={})",
                cases.len(),
                harness_manifest.display(),
                harness_c.display()
            );
        }
        let passed = test_runner::run_cases(&ctx.root, cases, runner)
            .map_err(|error| CliFailure::operational("run tests", None, error))?;
        if !passed {
            return Err(CliFailure::operational(
                "run tests",
                None,
                "one or more tests failed",
            ));
        }
    }
    Ok(CliSuccess::Done)
}

fn run_exact_test(ctx: &project::ProjectContext, exact: &str) -> CliResult {
    let function = exact.rsplit("::").next().unwrap_or_default();
    let sources = project_test_sources(ctx).map_err(|error| {
        CliFailure::operational("discover test sources", Some(ctx.root.clone()), error)
    })?;
    for (path, module) in sources {
        let db = arandu_query::DatabaseImpl::new();
        let (file, filepath) =
            open_entry_file(&db, &mut arandu_base::SourceRegistry::default(), &path);
        let artifacts = pipeline_lower(&db, file, &filepath);
        let target = format!("{}::{}::{}", ctx.name, module, function);
        if !exact.eq(&target) {
            continue;
        }
        let backend = arandu_backend_cranelift::CraneliftBackend::try_new()
            .map_err(|diag| CliFailure::diagnostics([diag], Some(path.clone())))?;
        let output = arandu_semantics::CodegenBackend::compile(
            backend,
            &artifacts.amir,
            artifacts.type_check.symbols.as_ref(),
            artifacts.type_check.type_info.as_ref(),
        )
        .map_err(|diag| CliFailure::diagnostics([diag], Some(path.clone())))?;
        let return_type = artifacts
            .amir
            .funcs
            .iter()
            .find(|func_def| {
                artifacts
                    .type_check
                    .symbols
                    .get(func_def.symbol)
                    .name
                    .as_str()
                    == function
            })
            .map(|func_def| {
                artifacts
                    .type_check
                    .type_info
                    .type_interner
                    .resolve(func_def.return_type)
            });
        unsafe {
            if matches!(return_type, Some(arandu_semantics::types::ArType::Void)) {
                if let Some(test_fn) =
                    arandu_semantics::CompiledCode::get_fn::<unsafe fn()>(&output, function)
                {
                    test_fn();
                    return Ok(CliSuccess::Done);
                }
            } else if let Some(arandu_semantics::types::ArType::Result(ok, _)) = return_type
                && matches!(
                    artifacts.type_check.type_info.type_interner.resolve(ok),
                    arandu_semantics::types::ArType::Void
                )
                && let Some(test_fn) = arandu_semantics::CompiledCode::get_fn::<
                    unsafe fn() -> *mut u8,
                >(&output, function)
            {
                let result = test_fn();
                if result.is_null() || *(result.cast::<usize>()) == 0 {
                    return Ok(CliSuccess::Done);
                }
                return Err(CliFailure::operational(
                    "run test case",
                    Some(path),
                    "test returned Result::Err",
                ));
            }
        }
        return Err(CliFailure::operational(
            "run test case",
            Some(path),
            format!("test function `{function}` is not callable as `fn() -> void`"),
        ));
    }
    Err(CliFailure::operational(
        "run test case",
        Some(ctx.root.clone()),
        format!("test case `{exact}` could not be compiled"),
    ))
}

fn cmd_project_audit(start: &Path, flags: &project::ProjectFlags) -> CliResult {
    let mut db = arandu_query::DatabaseImpl::default();
    let policy = project::ProjectFlags {
        locked: true,
        offline: true,
        ..flags.clone()
    };
    let ctx = project::load_project(&mut db, start, &policy).map_err(|error| {
        CliFailure::operational(
            "audit locked project graph",
            Some(start.to_path_buf()),
            error,
        )
    })?;
    println!("audit graph {}", ctx.lockfile.manifest_fingerprint);
    let mut remote = 0usize;
    for package in &ctx.lockfile.packages {
        if let (Some(origin), Some(commit), Some(digest)) = (
            package.origin.as_deref(),
            package.commit.as_deref(),
            package.content_digest.as_deref(),
        ) {
            remote += 1;
            println!(
                "remote {} origin={} commit={} digest={}",
                package.name, origin, commit, digest
            );
        }
    }
    println!(
        "integrity=verified locked=offline remote_packages={remote} advisories=not-configured"
    );
    Ok(CliSuccess::Done)
}

fn cmd_project_inspect(start: &Path, flags: &project::ProjectFlags, verify: bool) -> CliResult {
    let mut policy = flags.clone();
    if verify {
        policy.locked = true;
        policy.offline = true;
    }
    let mut db = arandu_query::DatabaseImpl::default();
    let ctx = project::load_project(&mut db, start, &policy).map_err(|error| {
        CliFailure::operational(
            if verify {
                "verify locked project graph"
            } else {
                "resolve project graph"
            },
            Some(start.to_path_buf()),
            error,
        )
    })?;
    let lock = ctx.lockfile;
    println!("graph {}", lock.manifest_fingerprint);
    for package in lock.packages {
        let digest = package.content_digest.as_deref().unwrap_or("local");
        println!(
            "{} {} {} {}",
            package.name, package.version, package.source, digest
        );
        for dependency in package.dependencies {
            println!("  -> {dependency}");
        }
    }
    if verify {
        println!("verified locked offline graph");
    }
    Ok(CliSuccess::Done)
}

fn cmd_project_run(
    start: &Path,
    flags: &project::ProjectFlags,
    opt: bool,
    _debug: bool,
) -> CliResult {
    if flags.release {
        return Err(CliFailure::usage(
            "`run --release` (LLVM) is not implemented yet; use `run` for Cranelift JIT",
        ));
    }
    let (mut db, rebuild_log) = arandu_query::DatabaseImpl::with_rebuild_log();
    let ctx = match project::load_project(&mut db, start, flags) {
        Ok(c) => c,
        Err(e) => {
            return Err(CliFailure::operational(
                "load project",
                Some(start.into()),
                e,
            ));
        }
    };
    let mut registry = arandu_base::SourceRegistry::default();
    let (file, filepath) = open_entry_file(&db, &mut registry, &ctx.entry_path);
    let artifacts = pipeline_lower(&db, file, &filepath);
    eprintln!("{}", rebuild_log.status_line());

    let type_check = &artifacts.type_check;
    let mut amir_owned = if opt {
        Some(artifacts.amir.clone())
    } else {
        None
    };
    if let Some(ref mut amir) = amir_owned {
        optimize_amir_or_exit(amir, type_check, &filepath);
    }
    let amir = match &amir_owned {
        Some(a) => a,
        None => &artifacts.amir,
    };

    use arandu_semantics::{CodegenBackend, CompiledCode};
    let output = {
        let backend = match arandu_backend_cranelift::CraneliftBackend::try_new() {
            Ok(b) => b,
            Err(diag) => print_diagnostics_and_exit(std::iter::once(diag), &filepath),
        };
        match CodegenBackend::compile(
            backend,
            amir,
            type_check.symbols.as_ref(),
            type_check.type_info.as_ref(),
        ) {
            Ok(out) => out,
            Err(diag) => print_diagnostics_and_exit(std::iter::once(diag), &filepath),
        }
    };

    let main_is_void = amir.funcs.iter().any(|f| {
        let name = type_check.symbols.get(f.symbol).name.as_str();
        name == "main"
            && matches!(
                type_check.type_info.type_interner.resolve(f.return_type),
                arandu_semantics::types::ArType::Void
            )
    });
    let has_main = amir
        .funcs
        .iter()
        .any(|f| type_check.symbols.get(f.symbol).name.as_str() == "main");
    if !has_main {
        return Err(CliFailure::operational(
            "run project",
            Some(ctx.entry_path),
            "'main' function not found in compiled program",
        ));
    }

    unsafe {
        if main_is_void {
            if let Some(main_fn) = CompiledCode::get_fn::<unsafe fn()>(&output, "main") {
                main_fn();
                return Ok(CliSuccess::Done);
            }
        } else if let Some(main_fn) = CompiledCode::get_fn::<unsafe fn() -> i32>(&output, "main") {
            return Ok(CliSuccess::ProgramExit(main_fn()));
        }
    }
    Err(CliFailure::operational(
        "run project",
        Some(ctx.entry_path),
        "compiled entry point could not be loaded",
    ))
}

fn cmd_project_build(
    start: &Path,
    flags: &project::ProjectFlags,
    opt: bool,
    _debug: bool,
) -> CliResult {
    let backend = project::BackendChoice::from_release_flag(flags.release);
    if matches!(backend, project::BackendChoice::LlvmReserved) {
        return Err(CliFailure::usage(
            "`build --release` selects LLVM, which is not available yet; use `build` for Cranelift or `emit-c` for C output",
        ));
    }

    let (mut db, rebuild_log) = arandu_query::DatabaseImpl::with_rebuild_log();
    let ctx = match project::load_project(&mut db, start, flags) {
        Ok(c) => c,
        Err(e) => {
            return Err(CliFailure::operational(
                "load project",
                Some(start.into()),
                e,
            ));
        }
    };
    let mut registry = arandu_base::SourceRegistry::default();
    let (file, filepath) = open_entry_file(&db, &mut registry, &ctx.entry_path);
    let artifacts = pipeline_lower(&db, file, &filepath);
    eprintln!("{}", rebuild_log.status_line());

    // Dev "build" = typecheck + lower + relocatable native object emission.
    let type_check = &artifacts.type_check;
    let mut amir_owned = if opt {
        Some(artifacts.amir.clone())
    } else {
        None
    };
    if let Some(ref mut amir) = amir_owned {
        optimize_amir_or_exit(amir, type_check, &filepath);
    }
    let amir = match &amir_owned {
        Some(a) => a,
        None => &artifacts.amir,
    };

    let backend_impl = match arandu_backend_cranelift::CraneliftObjectBackend::host_baseline() {
        Ok(b) => b,
        Err(diag) => print_diagnostics_and_exit(std::iter::once(diag), &filepath),
    };
    match backend_impl.compile(
        amir,
        type_check.symbols.as_ref(),
        type_check.type_info.as_ref(),
    ) {
        Ok(object) => {
            let artifact = artifact::publish_native_artifact(
                &ctx.root,
                &ctx.name,
                &ctx.version,
                object.bytes(),
                |object, output| linker::link(object, output).map(|kind| kind.label()),
            )?;
            println!(
                "built {} v{} (backend={}, entry={}, artifact={})",
                ctx.name,
                ctx.version,
                backend.label(),
                ctx.entry_rel,
                artifact.display()
            );
            Ok(CliSuccess::Done)
        }
        Err(diag) => print_diagnostics_and_exit(std::iter::once(diag), &filepath),
    }
}
