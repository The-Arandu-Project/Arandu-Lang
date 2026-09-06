#![allow(clippy::unwrap_used, clippy::expect_used)]

use arandu_middle::docs::{DocItemKind, EffectBadge};
use arandu_query::db::DatabaseImpl;
use arandu_query::docs::{file_doctests, item_doc, module_doc};
use salsa::Setter;
use std::sync::Arc;

fn make_source(beta_body: &str) -> String {
    format!(
        r#"module tests.docs

/// Adds two numbers with constant complexity.
///
/// # Complexity
/// O(1) amortized.
///
/// # Allocations
/// Zero-Alloc.
///
/// # Examples
/// ```arandu
/// let res = add(2, 3)
/// assert(res == 5)
/// ```
public func add(a: int, b: int): int {{
    return a + b
}}

/// Second function whose body will be edited.
public func compute(): int {{
    {beta_body}
}}
"#
    )
}

#[test]
fn doc_queries_extract_contracts_and_survive_body_edits() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file("docs_sample.aru".into(), make_source("return 10"));

    let mod_doc = module_doc(&db, file);
    assert_eq!(mod_doc.name, "tests.docs");
    assert_eq!(mod_doc.items.len(), 2);

    let add_item = mod_doc
        .items
        .iter()
        .find(|i| i.name == "add")
        .expect("add item");
    assert_eq!(add_item.kind, DocItemKind::Function);
    assert_eq!(
        add_item.summary.as_deref(),
        Some("Adds two numbers with constant complexity.")
    );
    assert_eq!(
        add_item.sections.complexity.as_deref(),
        Some("O(1) amortized.")
    );
    assert_eq!(
        add_item.sections.allocations.as_deref(),
        Some("Zero-Alloc.")
    );
    assert_eq!(add_item.sections.examples.len(), 1);
    assert!(add_item.sections.examples[0]
        .code
        .contains("let res = add(2, 3)"));
    assert!(add_item.effect_badges.contains(&EffectBadge::Pure));
    assert!(add_item.effect_badges.contains(&EffectBadge::ZeroAlloc));

    // Test doctest extraction query
    let doctests = file_doctests(&db, file);
    assert_eq!(doctests.len(), 1);
    assert!(doctests[0].code.contains("assert(res == 5)"));

    let add_sym = add_item.symbol_id;

    // Mutate ONLY the body of compute() - add() is untouched
    file.set_text(&mut db)
        .to(Arc::from(make_source("let mut x = 20\n    return x * 2")));

    // Verify item_doc for add still returns valid and identical documentation
    let add_item_after = item_doc(&db, file, add_sym)
        .clone()
        .expect("add item after edit");
    assert_eq!(add_item_after.name, "add");
    assert_eq!(
        add_item_after.summary.as_deref(),
        Some("Adds two numbers with constant complexity.")
    );
    assert_eq!(
        add_item_after.sections.complexity.as_deref(),
        Some("O(1) amortized.")
    );

    let mod_doc_after = module_doc(&db, file);
    assert_eq!(mod_doc_after.items.len(), 2);
    assert_eq!(mod_doc_after.name, "tests.docs");
}

#[test]
fn ffi_call_does_not_derive_pure_or_thread_safe() {
    let source = r#"module tests.runtime_docs

extern "C" {
    func ar_rt_wake(id: int): void
}

public func trigger_wake(id: int): void {
    unsafe {
        ar_rt_wake(id)
    }
}
"#;
    let mut db = DatabaseImpl::new();
    let file = db.new_file("runtime_docs.aru".into(), source.into());

    let mod_doc = module_doc(&db, file);
    let trigger_item = mod_doc
        .items
        .iter()
        .find(|i| i.name == "trigger_wake")
        .expect("trigger_wake item");

    assert!(!trigger_item.effect_badges.contains(&EffectBadge::Pure));
    assert!(!trigger_item.effect_badges.contains(&EffectBadge::ZeroAlloc));
    assert!(!trigger_item
        .effect_badges
        .contains(&EffectBadge::ThreadSafe));
}
