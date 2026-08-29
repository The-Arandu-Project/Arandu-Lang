//! Canonical pipeline execution and diagnostic emission for Arandu CLI.
//!
//! Enforces: CST (`syntax_tree`) → AST (`parse`) → `resolve` → `type_check` → `lower_amir` → backend.

use std::fs;
use std::path::{Path, PathBuf};
use std::process;

use crate::cli_error::{CliFailure, CliResult};

pub fn finish(result: CliResult) -> ! {
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

pub fn fail_usage(message: impl Into<String>) -> ! {
    finish(Err(CliFailure::usage(message)))
}

pub fn fail_operational(
    operation: &'static str,
    context: Option<PathBuf>,
    source: impl Into<String>,
) -> ! {
    finish(Err(CliFailure::operational(operation, context, source)))
}

pub fn print_diagnostics_and_exit(
    diagnostics: impl IntoIterator<Item = arandu_middle::Diagnostic>,
    filepath: &str,
) -> ! {
    let source_path = (!filepath.is_empty()).then(|| PathBuf::from(filepath));
    finish(Err(CliFailure::diagnostics(diagnostics, source_path)))
}

pub fn print_parse_error_and_exit(err: &arandu_parser::ParseError, filepath: &str) -> ! {
    let diag = arandu_middle::Diagnostic::from(err.clone());
    print_diagnostics_and_exit(std::iter::once(diag), filepath);
}

pub fn optimize_amir_or_exit(
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

pub fn validate_hir_and_monomorphize(
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

pub struct CheckedProgram {
    /// Shared with Salsa memo — never deep-clone the AST.
    pub program: std::sync::Arc<arandu_parser::Program>,
    pub type_check: arandu_semantics::TypeCheckResult,
}

/// Render non-fatal Salsa diagnostics or terminate through the typed diagnostic path.
pub fn handle_accumulated_diags(
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
pub fn pipeline_lower(
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

/// Parse + type-check for paths that still need a local TypeCheckResult (e.g. `hir`).
pub fn parse_and_check(
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

/// Opt-in GenRef observability, intentionally outside Salsa queries. This
/// derives metrics from the immutable AMIR artifact and has no query-visible
/// side effects.
pub fn print_genref_report(filepath: &str, artifacts: &arandu_query::LowerAmirArtifacts) {
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

/// Attach resolved stdlib root to the DB (install cascade; never cwd-only).
pub fn attach_stdlib(db: &arandu_query::DatabaseImpl, explicit: Option<PathBuf>) {
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

pub fn open_entry_file(
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

pub fn find_aru_files(dir: &Path, files: &mut Vec<PathBuf>) -> std::io::Result<()> {
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

/// True when `path` should use package mode (dir with Arandu.toml, or the toml itself).
pub fn is_project_target(arg: Option<&str>) -> bool {
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
