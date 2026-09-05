#![allow(clippy::expect_used, clippy::unwrap_used)]

use arandu_middle::DiagCode;
use arandu_query::passes::type_check;
use arandu_query::DatabaseImpl;

#[test]
fn pure_function_calling_pure_succeeds() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file(
        "effects_pure.aru".into(),
        r#"
@Effects(Pure)
func helper(x: int): int {
    return x + 1
}

@Effects(Pure)
func compute(x: int): int {
    return helper(x) * 2
}
"#
        .into(),
    );

    let checked = type_check(&db, file);
    let effect_errors: Vec<_> = checked
        .diagnostics
        .iter()
        .filter(|d| d.code == DiagCode::T039UnsatisfiedEffect)
        .collect();
    assert!(
        effect_errors.is_empty(),
        "Expected zero effect errors, found: {effect_errors:?}"
    );
}

#[test]
fn pure_function_calling_foreign_extern_fails_with_t039() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file(
        "effects_foreign.aru".into(),
        r#"
extern "C" {
    func puts(s: str): int
}

@Effects(Pure)
func run(): int {
    unsafe {
        puts("hello")
    }
    return 0
}
"#
        .into(),
    );

    let checked = type_check(&db, file);
    let unsatisfied = checked
        .diagnostics
        .iter()
        .find(|d| d.code == DiagCode::T039UnsatisfiedEffect)
        .expect("T039UnsatisfiedEffect for foreign call");
    assert!(unsatisfied.message.contains("Foreign"));
}

#[test]
fn effect_propagation_detects_undeclared_transitive_effect() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file(
        "effects_transitive.aru".into(),
        r#"
@Effects(Net)
func fetch_data(): int {
    return 42
}

@Effects(FileRead)
func process(): int {
    let data = fetch_data()
    return data
}
"#
        .into(),
    );

    let checked = type_check(&db, file);
    let unsatisfied = checked
        .diagnostics
        .iter()
        .find(|d| d.code == DiagCode::T039UnsatisfiedEffect)
        .expect("T039 for missing Net effect in caller");
    assert!(unsatisfied.message.contains("Net"));
}

#[test]
fn effect_matching_declared_effects_succeeds() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file(
        "effects_matching.aru".into(),
        r#"
@Effects(Net)
func fetch_data(): int {
    return 42
}

@Effects(Net, FileRead)
func process(): int {
    let data = fetch_data()
    return data
}
"#
        .into(),
    );

    let checked = type_check(&db, file);
    let effect_errors: Vec<_> = checked
        .diagnostics
        .iter()
        .filter(|d| d.code == DiagCode::T039UnsatisfiedEffect)
        .collect();
    assert!(
        effect_errors.is_empty(),
        "Expected zero effect errors, found: {effect_errors:?}"
    );
}

#[test]
fn unknown_effect_name_reports_n012() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file(
        "effects_unknown.aru".into(),
        r#"
@Effects(Telepathy)
func mind_read(): int {
    return 0
}
"#
        .into(),
    );

    let checked = type_check(&db, file);
    let unknown_err = checked
        .diagnostics
        .iter()
        .find(|d| d.code == DiagCode::N012UnknownAnnotation)
        .expect("N012UnknownAnnotation for unknown effect name");
    assert!(unknown_err.message.contains("Telepathy"));
}

#[test]
fn pure_function_awaiting_coroutine_fails_with_t039() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file(
        "effects_await.aru".into(),
        r#"
async func async_task(): int {
    return 10
}

@Effects(Pure)
async func run(): int {
    let x = await async_task()
    return x
}
"#
        .into(),
    );

    let checked = type_check(&db, file);
    let unsatisfied = checked
        .diagnostics
        .iter()
        .find(|d| d.code == DiagCode::T039UnsatisfiedEffect)
        .expect("T039UnsatisfiedEffect for await in Pure function");
    assert!(unsatisfied.message.contains("Suspend"));
}
