#![allow(clippy::unwrap_used, clippy::expect_used)]
//! P1/P2 — fine-grained typeck early cutoff across sibling items.

use arandu_query::db::DatabaseImpl;
use arandu_query::file_typeck_view;
use arandu_query::passes::{item_body_typeck, module_signatures, parse, type_check};
use arandu_query::SourceFile;
use salsa::Setter;
use std::sync::Arc;

fn free_func_list(db: &DatabaseImpl, file: SourceFile) -> Vec<arandu_middle::SymbolId> {
    let program = parse(db, file);
    let Ok(program) = &**program else {
        return vec![];
    };
    let sigs = module_signatures(db, file);
    arandu_semantics::free_func_symbols(program, sigs.resolved.as_ref())
}

fn body_item_list(db: &DatabaseImpl, file: SourceFile) -> Vec<arandu_middle::SymbolId> {
    let program = parse(db, file);
    let Ok(program) = &**program else {
        return vec![];
    };
    let sigs = module_signatures(db, file);
    arandu_semantics::body_item_symbols(program, sigs.resolved.as_ref())
}

fn two_funcs(beta_ret: i32) -> String {
    format!(
        r#"
func alpha(): int {{
    return 1
}}

func beta(): int {{
    return {beta_ret}
}}
"#
    )
}

#[test]
fn body_edit_beta_skips_alpha_item_body_typeck() {
    let (mut db, rebuild_log) = DatabaseImpl::with_rebuild_log();
    let file = db.new_file("two.aru".into(), two_funcs(2));

    let tc0 = type_check(&db, file);
    assert!(
        tc0.diagnostics.is_empty(),
        "expected clean typeck: {:?}",
        tc0.diagnostics
    );

    let funcs = free_func_list(&db, file);
    assert!(funcs.len() >= 2, "need alpha+beta, got {}", funcs.len());
    let alpha = funcs[0];
    let beta = funcs[1];

    let _ = item_body_typeck(&db, file, alpha);
    let _ = item_body_typeck(&db, file, beta);

    rebuild_log.clear();

    file.set_text(&mut db).to(Arc::from(two_funcs(99)));

    let funcs2 = free_func_list(&db, file);
    assert!(funcs2.len() >= 2);
    let alpha2 = funcs2[0];
    let beta2 = funcs2[1];

    let _ = item_body_typeck(&db, file, alpha2);
    let _ = item_body_typeck(&db, file, beta2);
    let _ = file_typeck_view(&db, file);

    let execs = rebuild_log.count_executions_matching("item_body_typeck");
    assert!(
        execs <= 1,
        "expected ≤1 item_body_typeck execute after beta-only edit, got {execs}"
    );

    let tc1 = type_check(&db, file);
    assert!(
        tc1.diagnostics.is_empty(),
        "post-edit: {:?}",
        tc1.diagnostics
    );
}

fn two_consts(b_val: i32) -> String {
    format!(
        r#"
const A = 1
const B = {b_val}
"#
    )
}

#[test]
fn const_edit_b_skips_const_a() {
    let (mut db, rebuild_log) = DatabaseImpl::with_rebuild_log();
    let file = db.new_file("consts.aru".into(), two_consts(2));

    let _ = type_check(&db, file);
    let items = body_item_list(&db, file);
    assert!(
        items.len() >= 2,
        "need const A+B items, got {}",
        items.len()
    );
    let a = items[0];
    let b = items[1];
    let _ = item_body_typeck(&db, file, a);
    let _ = item_body_typeck(&db, file, b);

    rebuild_log.clear();
    file.set_text(&mut db).to(Arc::from(two_consts(99)));

    let items2 = body_item_list(&db, file);
    assert!(items2.len() >= 2);
    let _ = item_body_typeck(&db, file, items2[0]);
    let _ = item_body_typeck(&db, file, items2[1]);
    let _ = file_typeck_view(&db, file);

    let execs = rebuild_log.count_executions_matching("item_body_typeck");
    assert!(
        execs <= 1,
        "expected ≤1 item_body_typeck after const B-only edit, got {execs}"
    );
}

fn struct_and_func(n: i32) -> String {
    format!(
        r#"
struct Point {{
    x: int
    y: int
}}

func main(): int {{
    return {n}
}}
"#
    )
}

#[test]
fn func_edit_skips_struct_item() {
    let (mut db, rebuild_log) = DatabaseImpl::with_rebuild_log();
    let file = db.new_file("mix.aru".into(), struct_and_func(1));

    let tc0 = type_check(&db, file);
    assert!(
        tc0.diagnostics.is_empty(),
        "clean typeck expected: {:?}",
        tc0.diagnostics
    );

    let items = body_item_list(&db, file);
    assert!(items.len() >= 2, "need struct+func, got {}", items.len());

    for &id in &items {
        let _ = item_body_typeck(&db, file, id);
    }

    rebuild_log.clear();
    file.set_text(&mut db).to(Arc::from(struct_and_func(2)));

    let items2 = body_item_list(&db, file);
    for &id in &items2 {
        let _ = item_body_typeck(&db, file, id);
    }
    let _ = file_typeck_view(&db, file);

    let execs = rebuild_log.count_executions_matching("item_body_typeck");
    // Only main should re-exec; Point span unchanged → 1 execute.
    assert!(
        execs <= 1,
        "expected ≤1 item_body_typeck after func-only edit, got {execs}"
    );
}
