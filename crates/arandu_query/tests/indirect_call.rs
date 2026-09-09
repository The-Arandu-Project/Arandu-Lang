//! W5: indirect calls must be rejected at typeck (T033), not only in JIT.
#![allow(clippy::unwrap_used)]
use arandu_diagnostics::DiagCode;
use arandu_query::db::DatabaseImpl;
use arandu_query::passes::type_check;

#[test]
fn function_value_call_is_t033() {
    let mut db = DatabaseImpl::default();
    let f = db.new_file(
        "ind.aru".to_string(),
        r#"
            func add(a: int, b: int): int { return a + b }
            func main(): int {
                let f = add
                return f(1, 2)
            }
        "#
        .to_string(),
    );
    let tc = type_check(&db, f);
    assert!(
        tc.diagnostics
            .iter()
            .any(|d| d.code == DiagCode::T033IndirectCallNotSupported),
        "expected T033 indirect call, got {:?}",
        tc.diagnostics
    );
}

/// A callback stored in a product type must not bypass the same boundary as
/// a local function value. Structured jobs need this boundary implemented in
/// both backends before exposing a callback-based parallel API.
#[test]
fn function_value_through_inferred_struct_field_is_t033() {
    let mut db = DatabaseImpl::default();
    let f = db.new_file(
        "job.aru".to_string(),
        r#"
            struct Job<T> { callback: T }
            func count(value: int): int { return value + 1 }
            func main(): int {
                let job = Job { callback: count }
                return (job.callback)(41)
            }
        "#
        .to_string(),
    );
    let tc = type_check(&db, f);
    assert!(
        tc.diagnostics
            .iter()
            .any(|d| d.code == DiagCode::T033IndirectCallNotSupported),
        "expected a typed callback to reach T033, got {:?}",
        tc.diagnostics
    );
}
