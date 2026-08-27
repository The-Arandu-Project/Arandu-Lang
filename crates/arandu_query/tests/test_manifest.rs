#![allow(clippy::expect_used, clippy::unwrap_used)]

use arandu_query::passes::{module_signatures, parse, type_check};
use arandu_query::testing::{file_test_manifest, item_test_case, ITEM_TEST_CASE_EXEC_COUNT};
use arandu_query::{DatabaseImpl, SourceFile};
use salsa::Setter;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

static COUNTER_LOCK: Mutex<()> = Mutex::new(());

fn items(db: &DatabaseImpl, file: SourceFile) -> Vec<arandu_middle::SymbolId> {
    let parsed = parse(db, file);
    let parsed_result = parsed.as_ref();
    let program = parsed_result.as_ref().expect("parse");
    let signatures = module_signatures(db, file);
    arandu_semantics::body_item_symbols(program, signatures.resolved.as_ref())
}

#[test]
fn manifest_discovers_tests_in_stable_name_order() {
    let _guard = COUNTER_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut db = DatabaseImpl::new();
    let file = db.new_file(
        "unicode-tests.aru".into(),
        "@Test\nfunc zeta(): void {}\n@Test\nfunc árvore(): Result<void, Err> { return .Ok }\n"
            .into(),
    );

    let manifest = file_test_manifest(&db, file);
    let names = manifest
        .iter()
        .map(|case| case.name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(names, ["zeta", "árvore"]);
}

#[test]
fn invalid_signature_is_diagnosed_and_not_discovered() {
    let _guard = COUNTER_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut db = DatabaseImpl::new();
    let file = db.new_file(
        "invalid-test.aru".into(),
        "@Test\nfunc needsInput(value: int): int { return value }\n".into(),
    );

    let checked = type_check(&db, file);
    assert!(checked
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code == arandu_middle::DiagCode::T036InvalidTestContract));
    assert!(file_test_manifest(&db, file).is_empty());
}

#[test]
fn benchmark_name_is_reserved_until_its_runtime_contract_exists() {
    let _guard = COUNTER_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut db = DatabaseImpl::new();
    let file = db.new_file(
        "planned-benchmark.aru".into(),
        "@Benchmark\nfunc measure(): void {}\n".into(),
    );

    let checked = type_check(&db, file);
    assert!(checked.diagnostics.iter().any(|diagnostic| {
        diagnostic.code == arandu_middle::DiagCode::N012UnknownAnnotation
            && diagnostic.message.contains("planned but not available")
    }));
}

#[test]
fn item_discovery_preserves_sibling_early_cutoff() {
    let _guard = COUNTER_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut db = DatabaseImpl::new();
    let file = db.new_file(
        "test-cutoff.aru".into(),
        "@Test\nfunc alpha(): void {}\n\nfunc beta(): int { return 1 }\n".into(),
    );
    let initial = items(&db, file);
    for &symbol in &initial {
        let _ = item_test_case(&db, file, symbol);
    }
    ITEM_TEST_CASE_EXEC_COUNT.store(0, Ordering::SeqCst);

    file.set_text(&mut db).to(Arc::from(
        "@Test\nfunc alpha(): void {}\n\nfunc beta(): int { return 2 }\n",
    ));
    let changed = items(&db, file);
    let _ = item_test_case(&db, file, changed[0]);
    let unchanged_executions = ITEM_TEST_CASE_EXEC_COUNT.load(Ordering::SeqCst);
    let _ = item_test_case(&db, file, changed[1]);
    let total_executions = ITEM_TEST_CASE_EXEC_COUNT.load(Ordering::SeqCst);

    assert!(
        unchanged_executions == 0 && total_executions <= 1,
        "editing a sibling body must not rediscover the unchanged test: unchanged={unchanged_executions}, total={total_executions}"
    );
}
