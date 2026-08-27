//! P7-B: the package cache must coordinate independent OS processes.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use arandu_package::cache::{CachePublish, CacheScanLimits, CacheStore};
use arandu_query::{CacheDigest, CacheLayout};

const CHILD_MODE: &str = "ARANDU_P7_CACHE_CHILD";
const CACHE_ROOT: &str = "ARANDU_P7_CACHE_ROOT";

#[test]
fn child_publish_archive() {
    if std::env::var_os(CHILD_MODE).is_none() {
        return;
    }
    let root = std::env::var_os(CACHE_ROOT).expect("child cache root");
    let bytes = b"p7-b immutable archive shared by independent processes\n";
    let digest = CacheDigest::sha256(bytes);
    let layout = CacheLayout::new(root.into()).expect("valid child cache layout");
    let result = CacheStore::new(layout)
        .publish_archive(digest, bytes)
        .expect("child publication should converge");
    assert!(matches!(
        result,
        CachePublish::Added | CachePublish::AlreadyPresent
    ));
}

#[test]
fn independent_publishers_recover_from_interrupted_staging() {
    let root = temp_dir("arandu-p7-cache-process");
    let layout = CacheLayout::new(root.clone()).unwrap();
    fs::create_dir_all(layout.staging()).unwrap();
    fs::write(
        layout.staging().join("interrupted-process.partial"),
        b"not a cache object",
    )
    .unwrap();

    let mut children = Vec::new();
    for _ in 0..6 {
        children.push(
            Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "child_publish_archive", "--nocapture"])
                .env(CHILD_MODE, "1")
                .env(CACHE_ROOT, &root)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("spawn cache publisher"),
        );
    }
    for child in children {
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "cache publisher failed:\nstdout={}\nstderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let store = CacheStore::new(layout.clone());
    let bytes = b"p7-b immutable archive shared by independent processes\n";
    let digest = CacheDigest::sha256(bytes);
    assert_eq!(fs::read(layout.archive(digest)).unwrap(), bytes);
    let verification = store.verify(CacheScanLimits::default()).unwrap();
    assert_eq!(verification.verified, 1);
    assert_eq!(verification.corrupt, 0);
    assert_eq!(verification.invalid_entries, 0);
    assert!(
        layout
            .staging()
            .join("interrupted-process.partial")
            .is_file(),
        "recovery must ignore, not mistake, an incomplete staging object"
    );

    fs::remove_dir_all(root).unwrap();
}

fn temp_dir(prefix: &str) -> std::path::PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!("{prefix}-{}-{nonce}", std::process::id()));
    fs::create_dir_all(&path).unwrap();
    path
}
