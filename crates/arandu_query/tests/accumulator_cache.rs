#![allow(clippy::unwrap_used, clippy::expect_used)]
use arandu_query::db::{DatabaseImpl, SourceFile};

#[test]
fn accumulator_diagnostic_survives_cache_hit() {
    let (db, rebuild_log) = DatabaseImpl::with_rebuild_log();

    // Create a source file with an intentional resolution error (duplicate field)
    let code = std::sync::Arc::from("struct Foo { a: i32; a: i32; }");
    let file = SourceFile::new(
        &db,
        1,
        code,
        std::sync::Arc::new(std::path::PathBuf::from("test.ar")),
    );

    rebuild_log.clear();

    // First execution: query runs, diagnostic is emitted via accumulate()
    let _ = arandu_query::passes::type_check(&db, file);

    let diags1 = arandu_query::passes::type_check::accumulated::<
        arandu_middle::db::DiagnosticsAccumulator,
    >(&db, file);
    assert!(
        !diags1.is_empty(),
        "Expected diagnostics on first run of query"
    );

    // Prova nº 1: A query de fato rodou.
    let execs1 = rebuild_log.count_executions_matching("type_check");
    assert!(
        execs1 >= 1,
        "Query should have executed on initial run, got {execs1}"
    );

    rebuild_log.clear();

    // Second execution, WITHOUT changing the input: it must be a cache hit.
    let _ = arandu_query::passes::type_check(&db, file);
    let diags2 = arandu_query::passes::type_check::accumulated::<
        arandu_middle::db::DiagnosticsAccumulator,
    >(&db, file);
    assert!(
        !diags2.is_empty(),
        "Expected diagnostics to be re-emitted on cache hit"
    );

    // Prova nº 2: A query bateu no cache e NÃO re-executou a query.
    let execs2 = rebuild_log.count_executions_matching("type_check");
    assert_eq!(
        execs2, 0,
        "Query should NOT have executed again on a cache hit"
    );

    // Prova nº 3: O exato mesmo diagnóstico que estava guardado no cache foi re-emitido nativamente.
    assert_eq!(diags1.len(), diags2.len());
    assert_eq!(diags1[0].0.code, diags2[0].0.code);
}
