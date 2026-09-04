#![allow(clippy::expect_used)]

use arandu_middle::types::BorrowPath;
use arandu_query::db::DatabaseImpl;
use arandu_query::{borrow_interfaces, lower_amir, SourceFile, StableHash};

fn summary_for(
    db: &DatabaseImpl,
    file: SourceFile,
    name: &str,
) -> arandu_middle::types::ReturnBorrowSummary {
    let lowered = lower_amir(db, file);
    let diagnostics = arandu_query::passes::lower_amir::accumulated::<
        arandu_middle::db::DiagnosticsAccumulator,
    >(db, file)
    .into_iter()
    .map(|diagnostic| &diagnostic.0)
    .collect::<Vec<_>>();
    assert!(
        !lowered.amir.funcs.is_empty(),
        "program did not lower: {diagnostics:?}"
    );
    let symbol = lowered
        .type_check
        .symbols
        .iter()
        .find(|symbol| symbol.name == name)
        .map(|symbol| symbol.id)
        .expect("function symbol");
    lowered
        .type_check
        .type_info
        .return_borrow_summaries
        .get(&symbol)
        .cloned()
        .expect("flow-derived borrow interface")
}

fn root_sources(summary: &arandu_middle::types::ReturnBorrowSummary) -> Vec<u32> {
    summary
        .dependencies
        .iter()
        .find(|dependency| dependency.result_path == BorrowPath::root())
        .expect("root dependency")
        .sources
        .iter()
        .map(|source| source.parameter_index)
        .collect()
}

#[test]
fn branch_union_contains_only_origins_that_reach_the_return() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file(
        "choice.aru".into(),
        r#"func choose(flag: bool, left: ref int, right: ref int, unused: ref int): ref int {
    if flag {
        return left
    }
    return right
}
"#
        .into(),
    );

    assert_eq!(root_sources(&summary_for(&db, file, "choose")), vec![1, 2]);
}

#[test]
fn call_composition_maps_callee_formals_to_caller_formals() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file(
        "forward.aru".into(),
        r#"func choose(flag: bool, left: ref int, right: ref int): ref int {
    if flag {
        return left
    }
    return right
}

func forward(flag: bool, first: ref int, second: ref int): ref int {
    return choose(flag, second, first)
}
"#
        .into(),
    );

    assert_eq!(root_sources(&summary_for(&db, file, "forward")), vec![1, 2]);
}

#[test]
fn recursive_component_reaches_a_deterministic_least_fixpoint() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file(
        "recursive.aru".into(),
        r#"func rotate(depth: int, left: ref int, right: ref int): ref int {
    if depth == 0 {
        return left
    }
    return rotate(depth - 1, right, left)
}

"#
        .into(),
    );

    assert_eq!(root_sources(&summary_for(&db, file, "rotate")), vec![1, 2]);
}

#[test]
fn imported_body_interface_is_composed_without_exposing_its_body() {
    let mut db = DatabaseImpl::new();
    let _library = db.new_file(
        "loans.aru".into(),
        r#"module loans

public func choose(flag: bool, left: ref int, right: ref int): ref int {
    if flag {
        return left
    }
    return right
}

"#
        .into(),
    );
    let caller = db.new_file(
        "caller.aru".into(),
        r#"module caller
import loans

func forward(flag: bool, first: ref int, second: ref int): ref int {
    return loans.choose(flag, second, first)
}
"#
        .into(),
    );

    assert_eq!(
        root_sources(&summary_for(&db, caller, "forward")),
        vec![1, 2]
    );
}

#[test]
fn monomorphized_forwarder_preserves_the_template_dependency() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file(
        "generic.aru".into(),
        r#"func identity<T>(value: ref T): ref T {
    return value
}

func forward(value: ref int): ref int {
    return identity(value)
}
"#
        .into(),
    );

    assert_eq!(root_sources(&summary_for(&db, file, "forward")), vec![0]);
}

#[test]
fn option_carrier_publishes_a_structural_result_path() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file(
        "option.aru".into(),
        r#"func wrap(value: ref int): Option<ref int> {
    return Option.Some(value)
}
"#
        .into(),
    );

    let summary = summary_for(&db, file, "wrap");
    assert_eq!(summary.dependencies.len(), 1);
    assert_eq!(
        summary.dependencies[0].result_path,
        BorrowPath(vec![arandu_middle::types::BorrowPathSegment::OptionSome])
    );
    assert_eq!(
        summary.dependencies[0]
            .sources
            .iter()
            .map(|source| source.parameter_index)
            .collect::<Vec<_>>(),
        vec![0]
    );
}

#[test]
fn isolated_query_hash_ignores_body_edits_that_preserve_the_contract() {
    use salsa::Setter;
    use std::sync::Arc;

    let mut db = DatabaseImpl::new();
    let source = |extra: &str| {
        format!(
            r#"func identity(value: ref int): ref int {{
    {extra}
    return value
}}
"#
        )
    };
    let file = db.new_file("cutoff.aru".into(), source(""));
    let before = borrow_interfaces(&db, file).stable_hash();

    file.set_text(&mut db)
        .to(Arc::from(source("let bodyOnly = 1 + 1")));
    let after = borrow_interfaces(&db, file).stable_hash();
    assert!(
        before == after,
        "body-only edit changed the public interface hash"
    );
}
