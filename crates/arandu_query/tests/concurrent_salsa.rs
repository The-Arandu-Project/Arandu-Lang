#![allow(clippy::panic)]

use arandu_query::analysis::AnalysisHost;
use arandu_query::file_ide_diagnostics;
use std::sync::{Arc, Barrier};
use std::thread;

#[test]
fn test_concurrent_salsa_queries() {
    let mut host = AnalysisHost::new();
    let f = host.new_file(
        "test_concurrent.aru".to_string(),
        "func main(): int { return 42 }".to_string(),
    );

    // Warm up query caches on main thread
    let _ = arandu_query::passes::type_check(host.db(), f);

    // Spawn concurrent readers using snapshots
    let mut readers = vec![];
    for _ in 0..4 {
        let snap = host.snapshot();
        readers.push(thread::spawn(move || {
            for _ in 0..50 {
                let _tc = arandu_query::passes::type_check(&snap.db, f);
            }
        }));
    }

    // Writer thread updates text, advancing database revision
    for i in 0..10 {
        let code = format!("func main(): int {{ return {} }}", i);
        host.set_text(f, code);
        thread::sleep(std::time::Duration::from_millis(2));
    }

    // Join readers and check errors without panicking the main thread
    for (i, r) in readers.into_iter().enumerate() {
        match r.join() {
            Ok(_) => println!("Thread {} succeeded", i),
            Err(e) => {
                let is_salsa_cancelled = e.is::<salsa::Cancelled>();
                println!(
                    "Thread {} failed: is_salsa_cancelled={}",
                    i, is_salsa_cancelled
                );
                // The test is considered successful if the thread either finished successfully
                // or was cancelled by Salsa when the writer mutated the inputs.
                assert!(
                    is_salsa_cancelled,
                    "Thread failed with non-cancellation panic!"
                );
            }
        }
    }

    assert!(
        file_ide_diagnostics(host.db(), f).is_empty(),
        "latest valid revision must not retain diagnostics from cancelled readers"
    );

    host.set_text(f, "func main(): int { return missing }".to_string());
    assert!(
        !file_ide_diagnostics(host.db(), f).is_empty(),
        "a new invalid revision must still execute after concurrent cancellation"
    );

    host.set_text(f, "func main(): int { return 42 }".to_string());
    assert!(
        file_ide_diagnostics(host.db(), f).is_empty(),
        "fixing the file must discard diagnostics from the previous revision"
    );
}

#[test]
fn concurrent_snapshots_keep_multiple_files_isolated() {
    let mut host = AnalysisHost::new();
    let alpha = host.new_file(
        "alpha.aru".to_string(),
        "func alpha(): int { return 1 }".to_string(),
    );
    let beta = host.new_file(
        "beta.aru".to_string(),
        "func beta(): int { return missing }".to_string(),
    );
    let revision = host.revision();
    let barrier = Arc::new(Barrier::new(3));

    let readers = [(alpha, false), (beta, true)].map(|(file, has_errors)| {
        let snapshot = host.snapshot();
        let barrier = Arc::clone(&barrier);
        thread::spawn(move || {
            assert_eq!(snapshot.revision, revision);
            barrier.wait();
            for _ in 0..50 {
                assert_eq!(
                    !file_ide_diagnostics(&snapshot.db, file).is_empty(),
                    has_errors
                );
            }
            barrier.wait();
        })
    });

    barrier.wait();
    barrier.wait();
    for reader in readers {
        reader.join().expect("snapshot reader must not panic");
    }

    assert!(file_ide_diagnostics(host.db(), alpha).is_empty());
    assert!(!file_ide_diagnostics(host.db(), beta).is_empty());
}
