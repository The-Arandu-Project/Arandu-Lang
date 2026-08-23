use std::sync::Arc;

use arandu_query::{passes, DatabaseImpl};
use salsa::Setter;

#[test]
fn same_shaped_symbol_rename_is_not_backdated_to_stale_result() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file(
        "rename.aru".into(),
        "func disk_symbol(): int { return 1 }\n".into(),
    );
    let before = passes::type_check(&db, file);
    assert!(before
        .symbols
        .iter()
        .any(|symbol| symbol.name == "disk_symbol"));

    file.set_text(&mut db)
        .to(Arc::from("func file_symbol(): int { return 1 }\n"));
    let after = passes::type_check(&db, file);
    assert!(
        after
            .symbols
            .iter()
            .any(|symbol| symbol.name == "file_symbol"),
        "HashEq must retain every symbol-table field observable by IDE clients"
    );
    assert!(!after
        .symbols
        .iter()
        .any(|symbol| symbol.name == "disk_symbol"));
}

#[test]
fn doc_only_edit_invalidates_resolve_presentation_data() {
    let mut db = DatabaseImpl::new();
    let file = db.new_file(
        "docs.aru".into(),
        "/// Old docs.\nfunc value(): int { return 1 }\n".into(),
    );
    let before = passes::resolve(&db, file);
    assert!(before
        .docs
        .values()
        .flatten()
        .any(|line| line.contains("Old docs")));

    file.set_text(&mut db)
        .to(Arc::from("/// New docs.\nfunc value(): int { return 1 }\n"));
    let after = passes::resolve(&db, file);
    assert!(after
        .docs
        .values()
        .flatten()
        .any(|line| line.contains("New docs")));
    assert!(!after
        .docs
        .values()
        .flatten()
        .any(|line| line.contains("Old docs")));
}
