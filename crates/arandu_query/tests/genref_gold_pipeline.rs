#![allow(clippy::expect_used)]

//! G0 characterization through the canonical query pipeline.

use arandu_middle::DiagCode;
use arandu_query::db::DatabaseImpl;
use arandu_query::passes::lower_amir;

fn lower(source: &str) -> String {
    let mut db = DatabaseImpl::new();
    let file = db.new_file("genref_gold.aru".into(), source.into());
    let lowered = lower_amir(&db, file);
    assert!(
        !lowered.amir.funcs.is_empty(),
        "surface program did not reach AMIR: {:?}",
        lowered.type_check.diagnostics
    );
    lowered.amir.pretty_print(
        lowered.type_check.symbols.as_ref(),
        &lowered.type_check.type_info.type_interner,
    )
}

#[test]
fn proven_local_borrow_has_no_generational_runtime_operations() {
    let amir = lower(
        r#"
func main(): int {
    let value = 41
    let borrowed = &value
    return *borrowed + 1
}
"#,
    );

    assert!(!amir.contains("gen_insert"), "unexpected fallback:\n{amir}");
    assert!(!amir.contains("gen_get"), "unexpected fallback:\n{amir}");
    assert!(!amir.contains("gen_remove"), "unexpected fallback:\n{amir}");
}

#[test]
fn aggregate_store_escape_characterizes_the_missing_surface_to_amir_path() {
    let source = r#"
struct Holder {
    value: &int
}
struct Pair {
    left: int
    right: int
}

func main(): int {
    let pair = Pair { left: 42, right: 7 }
    let mut holder = Holder { value: &pair.left }
    set holder.value = &pair.left
    return *holder.value
}
"#;
    let mut db = DatabaseImpl::new();
    let file = db.new_file("genref_escape_gap.aru".into(), source.into());
    let lowered = lower_amir(&db, file);
    let diagnostics =
        lower_amir::accumulated::<arandu_middle::db::DiagnosticsAccumulator>(&db, file);

    assert!(
        lowered.amir.funcs.is_empty(),
        "G0 characterization changed: aggregate escape unexpectedly reached AMIR"
    );
    assert!(
        !diagnostics.is_empty(),
        "lowering discarded the program without an accumulated diagnostic"
    );
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.0.code == DiagCode::O004GenerationalFallback),
        "expected the aggregate escape to remain inspectable as O004"
    );
    assert!(
        diagnostics.iter().any(|diagnostic| {
            diagnostic.0.code == DiagCode::O004GenerationalFallback
                && diagnostic.0.severity == arandu_middle::Severity::Error
        }),
        "projected escape must hard-fail instead of creating a stale snapshot"
    );
}
