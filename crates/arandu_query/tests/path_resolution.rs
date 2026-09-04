#![allow(clippy::unwrap_used, clippy::expect_used)]
use arandu_middle::db::SourceDatabase;
use arandu_query::db::DatabaseImpl;

#[test]
fn test_database_module_path_resolution() {
    let mut db = DatabaseImpl::default();

    // We create a few files in the DB to represent a virtual filesystem
    // A module path like ["std", "core", "math"] should resolve to std/core/math.aru or something similar.

    let path1 = "std/core/math.aru".to_string();
    let file1 = db.new_file(path1, "fn add() {}".to_string());

    let path2 = "app/utils.aru".to_string();
    let file2 = db.new_file(path2, "fn log() {}".to_string());

    let path3 = "app/models/user.aru".to_string();
    let file3 = db.new_file(path3, "struct User {}".to_string());

    // 1. Resolve flat path
    let resolved = db.resolve_module_path("app/utils.aru");
    assert!(resolved.is_some());
    assert!(resolved.unwrap() == file2);

    // 2. Resolve nested path
    let resolved = db.resolve_module_path("app/models/user.aru");
    assert!(resolved.is_some());
    assert!(resolved.unwrap() == file3);

    // 3. Resolve stdlib path
    let resolved = db.resolve_module_path("std/core/math.aru");
    assert!(resolved.is_some());
    assert!(resolved.unwrap() == file1);

    // 4. Resolve nonexistent path
    let resolved = db.resolve_module_path("app/nonexistent.aru");
    assert!(resolved.is_none());
}
