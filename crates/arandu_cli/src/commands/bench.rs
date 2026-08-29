//! Benchmark discovery, execution, baseline comparison, and harness publishing.

use arandu_query::ArandCompilerDb;
use std::fs;
use std::path::Path;

use crate::artifact;
use crate::cli_error::{CliFailure, CliResult, CliSuccess};
use crate::commands::test::{
    DiscoveryCase, DiscoveryReport, discovery_path, discovery_position, project_test_sources,
};
use crate::pipeline::{open_entry_file, pipeline_lower};
use crate::project::{self, ProjectFlags};
use crate::test_runner;

pub fn cmd_project_bench(
    start: &Path,
    flags: &ProjectFlags,
    list_only: bool,
    exact: Option<&str>,
    filter: Option<&str>,
    harness_child: bool,
    runner: &test_runner::BenchmarkRunnerOptions,
) -> CliResult {
    let mut db = arandu_query::DatabaseImpl::new();
    let ctx = project::load_project(&mut db, start, flags).map_err(|error| {
        CliFailure::operational("load benchmark project", Some(start.to_path_buf()), error)
    })?;
    let sources = project_test_sources(&ctx).map_err(|error| {
        CliFailure::operational("discover benchmark sources", Some(ctx.root.clone()), error)
    })?;
    let mut registry = arandu_codegen::testing::BenchmarkRegistry::default();
    let mut discovered = Vec::new();
    for (path, module) in sources {
        let key = path.to_string_lossy().into_owned();
        let file = if let Some(existing) = db.as_source_db().resolve_module_path(&key) {
            existing
        } else {
            let text = fs::read_to_string(&path).map_err(|error| {
                CliFailure::operational(
                    "read benchmark source",
                    Some(path.clone()),
                    error.to_string(),
                )
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
        let text = file.text(&db);
        for case in arandu_query::file_benchmark_manifest(&db, file).iter() {
            let id = format!("{}::{module}::{}", ctx.name, case.name);
            let (line, column_utf16) = discovery_position(text, case.span.start);
            discovered.push(DiscoveryCase {
                id: id.clone(),
                path: discovery_path(&ctx.root, &path),
                line,
                column_utf16,
            });
            registry.insert(arandu_codegen::testing::BenchmarkEntry {
                id,
                function: case.name.to_string(),
            });
        }
    }
    let mut cases = registry
        .iter()
        .map(|entry| entry.id.clone())
        .collect::<Vec<_>>();
    if let Some(filter) = filter {
        cases.retain(|case| case.contains(filter));
    }
    if let Some(exact) = exact {
        if !cases.iter().any(|case| case == exact) {
            return Err(CliFailure::operational(
                "select benchmark",
                Some(ctx.root),
                format!("benchmark `{exact}` was not found"),
            ));
        }
        cases.retain(|case| case == exact);
    }
    if list_only {
        if runner.format_json {
            discovered.retain(|case| cases.iter().any(|id| id == &case.id));
            discovered.sort_by(|left, right| left.id.cmp(&right.id));
            let report = DiscoveryReport {
                schema: arandu_codegen::testing::BENCH_LIST_PROTOCOL_V1,
                cases: discovered,
            };
            println!(
                "{}",
                serde_json::to_string_pretty(&report).map_err(|error| {
                    CliFailure::operational(
                        "serialize benchmark discovery",
                        None,
                        error.to_string(),
                    )
                })?
            );
        } else {
            for case in cases {
                println!("{case}");
            }
        }
        return Ok(CliSuccess::Done);
    }
    if harness_child {
        let exact = exact.ok_or_else(|| {
            CliFailure::operational("run benchmark child", None, "missing exact benchmark id")
        })?;
        let sequence = std::env::var("ARANDU_BENCH_SEQUENCE")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(0);
        let config = arandu_codegen::testing::BenchmarkConfigV1 {
            warmup_ns: std::env::var("ARANDU_BENCH_WARMUP_NS")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(runner.config.warmup_ns),
            measurement_ns: std::env::var("ARANDU_BENCH_MEASUREMENT_NS")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(runner.config.measurement_ns),
            samples: std::env::var("ARANDU_BENCH_SAMPLES")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(runner.config.samples),
        };
        arandu_runtime::testing_runtime::init_benchmark_context(exact, sequence, config.clone());
        let execution = run_exact_benchmark(&ctx, exact);
        let mut event = arandu_runtime::testing_runtime::finish_benchmark_context().unwrap_or(
            arandu_codegen::testing::BenchmarkEventV1 {
                sequence,
                id: exact.to_string(),
                config,
                samples: Vec::new(),
                stdout: arandu_codegen::testing::CapturedOutput::default(),
                stderr: arandu_codegen::testing::CapturedOutput::default(),
                failure: Some("benchmark context did not produce a result".to_string()),
            },
        );
        if let Err(error) = execution {
            event.failure = Some(format!("{error:?}"));
        }
        test_runner::send_benchmark_child_event(sequence, &event)
            .map_err(|error| CliFailure::operational("send benchmark event", None, error))?;
        if event.failure.is_some() {
            return Err(CliFailure::operational(
                "run benchmark",
                None,
                "benchmark failed",
            ));
        }
        return Ok(CliSuccess::Done);
    }

    let (manifest, c_source) = artifact::publish_benchmark_harness(&ctx.root, &registry)?;
    if !runner.format_json {
        eprintln!(
            "benchmark harness: {} cases (manifest={}, c={})",
            cases.len(),
            manifest.display(),
            c_source.display()
        );
    }
    let outcome = test_runner::run_benchmarks(&ctx.root, &ctx.stdlib.path, cases, runner)
        .map_err(|error| CliFailure::operational("run benchmarks", None, error))?;
    Ok(CliSuccess::ProgramExit(outcome.exit_code()))
}

pub fn run_exact_benchmark(ctx: &project::ProjectContext, exact: &str) -> CliResult {
    let function = exact.rsplit("::").next().unwrap_or_default();
    let sources = project_test_sources(ctx).map_err(|error| {
        CliFailure::operational("discover benchmark sources", Some(ctx.root.clone()), error)
    })?;
    for (path, module) in sources {
        let target = format!("{}::{}::{function}", ctx.name, module);
        if exact != target {
            continue;
        }
        let db = arandu_query::DatabaseImpl::new();
        db.set_stdlib_root(ctx.stdlib.path.clone());
        let (file, filepath) =
            open_entry_file(&db, &mut arandu_base::SourceRegistry::default(), &path);
        let artifacts = pipeline_lower(&db, file, &filepath);
        let backend = arandu_backend_cranelift::CraneliftBackend::try_new()
            .map_err(|diagnostic| CliFailure::diagnostics([diagnostic], Some(path.clone())))?;
        let output = arandu_semantics::CodegenBackend::compile(
            backend,
            &artifacts.amir,
            artifacts.type_check.symbols.as_ref(),
            artifacts.type_check.type_info.as_ref(),
        )
        .map_err(|diagnostic| CliFailure::diagnostics([diagnostic], Some(path.clone())))?;
        unsafe {
            if let Some(benchmark_fn) =
                arandu_semantics::CompiledCode::get_fn::<unsafe fn(*mut i64)>(&output, function)
            {
                let mut handle = 1_i64;
                benchmark_fn(&raw mut handle);
                return Ok(CliSuccess::Done);
            }
        }
        return Err(CliFailure::operational(
            "run benchmark",
            Some(path),
            format!("benchmark function `{function}` is not callable"),
        ));
    }
    Err(CliFailure::operational(
        "run benchmark",
        Some(ctx.root.clone()),
        format!("benchmark `{exact}` could not be compiled"),
    ))
}
