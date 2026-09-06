//! Platform-native cache-root discovery and artifact management for packages.

pub mod discovery;
pub mod ops;
pub mod store;
pub mod tree;
pub mod types;

pub use discovery::resolve_cache_layout;
pub use store::CacheStore;
pub use types::{
    CACHE_DIR_ENV, COPY_BUFFER_SIZE, CacheInspect, CachePrune, CachePublish, CacheScanLimits,
    CacheStoreError, CacheVerify, DEFAULT_SCAN_BYTES, DEFAULT_SCAN_ENTRIES, TreeLimits,
    TreeVerification, parse_scan_flags,
};

#[cfg(test)]
mod tests {
    use super::discovery::*;
    use super::store::*;
    use super::tree::*;
    use super::types::*;
    use arandu_query::{CacheDigest, CacheLayout};
    use std::env;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Barrier};
    use std::thread;

    fn temp_layout(name: &str) -> CacheLayout {
        let root = env::temp_dir().join(format!(
            "arandu-p5-{name}-{}-{}",
            std::process::id(),
            nonce()
        ));
        CacheLayout::new(root).unwrap()
    }

    #[test]
    fn explicit_absolute_override_wins() {
        let root = "/explicit";
        let resolved = resolve_cache_root_for(
            HostPlatform::Unix,
            Some(PathBuf::from(root)),
            Some(PathBuf::from("/environment")),
            CacheEnvironment::default(),
        )
        .unwrap();
        assert_eq!(resolved, Path::new(root));
    }

    #[test]
    fn relative_arandu_override_is_an_error() {
        let error = resolve_cache_root_for(
            HostPlatform::Unix,
            None,
            Some(PathBuf::from("relative-cache")),
            CacheEnvironment::default(),
        )
        .unwrap_err();
        assert!(error.contains("must be absolute"));
    }

    #[test]
    fn windows_uses_local_application_data() {
        let resolved = resolve_cache_root_for(
            HostPlatform::Windows,
            None,
            None,
            CacheEnvironment {
                local_app_data: Some(PathBuf::from(r"C:\Users\test\AppData\Local")),
                ..CacheEnvironment::default()
            },
        )
        .unwrap();
        let mut expected = PathBuf::from(r"C:\Users\test\AppData\Local");
        expected.push("Arandu");
        expected.push("Cache");
        assert_eq!(resolved, expected);
    }

    #[test]
    fn macos_uses_library_caches() {
        let resolved = resolve_cache_root_for(
            HostPlatform::MacOs,
            None,
            None,
            CacheEnvironment {
                home: Some(PathBuf::from("/Users/test")),
                ..CacheEnvironment::default()
            },
        )
        .unwrap();
        assert_eq!(resolved, Path::new("/Users/test/Library/Caches/Arandu"));
    }

    #[test]
    fn unix_uses_absolute_xdg_then_home_fallback() {
        let xdg = resolve_cache_root_for(
            HostPlatform::Unix,
            None,
            None,
            CacheEnvironment {
                xdg_cache_home: Some(PathBuf::from("/var/cache/user")),
                home: Some(PathBuf::from("/home/test")),
                ..CacheEnvironment::default()
            },
        )
        .unwrap();
        assert_eq!(xdg, Path::new("/var/cache/user/arandu"));

        let fallback = resolve_cache_root_for(
            HostPlatform::Unix,
            None,
            None,
            CacheEnvironment {
                xdg_cache_home: Some(PathBuf::from("relative")),
                home: Some(PathBuf::from("/home/test")),
                ..CacheEnvironment::default()
            },
        )
        .unwrap();
        assert_eq!(fallback, Path::new("/home/test/.cache/arandu"));
    }

    fn temp_store_fixture(name: &str) -> (CacheStore, CacheLayout) {
        let layout = temp_layout(name);
        let store = CacheStore::new(layout.clone());
        (store, layout)
    }

    #[test]
    fn archive_publish_is_verified_and_immutable() {
        let (store, layout) = temp_store_fixture("immutable");
        let bytes = b"canonical package archive";
        let digest = CacheDigest::sha256(bytes);

        assert_eq!(
            store.publish_archive(digest, bytes).unwrap(),
            CachePublish::Added
        );
        assert_eq!(
            store.publish_archive(digest, bytes).unwrap(),
            CachePublish::AlreadyPresent
        );
        assert_eq!(fs::read(layout.archive(digest)).unwrap(), bytes);
        assert!(store.publish_archive(digest, b"substitution").is_err());
        assert_eq!(fs::read(layout.archive(digest)).unwrap(), bytes);

        fs::remove_dir_all(layout.root()).unwrap();
    }

    #[test]
    fn corrupt_entry_is_quarantined_and_repaired_under_lock() {
        let (store, layout) = temp_store_fixture("repair");
        let bytes = b"verified package";
        let digest = CacheDigest::sha256(bytes);
        let archive = layout.archive(digest);
        create_parent(&archive).unwrap();
        fs::write(&archive, b"tampered").unwrap();

        assert_eq!(
            store.publish_archive(digest, bytes).unwrap(),
            CachePublish::Repaired
        );
        assert_eq!(fs::read(&archive).unwrap(), bytes);
        assert_eq!(fs::read_dir(layout.quarantine()).unwrap().count(), 1);

        fs::remove_dir_all(layout.root()).unwrap();
    }

    #[test]
    fn concurrent_publishers_converge_on_one_verified_object() {
        let (store, layout) = temp_store_fixture("concurrent");
        let store = Arc::new(store);
        let bytes: Arc<[u8]> = Arc::from(&b"shared package archive"[..]);
        let digest = CacheDigest::sha256(&bytes);
        let barrier = Arc::new(Barrier::new(8));
        let mut workers = Vec::new();
        for _ in 0..8 {
            let store = Arc::clone(&store);
            let bytes = Arc::clone(&bytes);
            let barrier = Arc::clone(&barrier);
            workers.push(thread::spawn(move || {
                barrier.wait();
                store.publish_archive(digest, &bytes)
            }));
        }

        let results = workers
            .into_iter()
            .map(|worker| worker.join().unwrap().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            results
                .iter()
                .filter(|result| **result == CachePublish::Added)
                .count(),
            1
        );
        assert_eq!(fs::read(layout.archive(digest)).unwrap(), &*bytes);
        assert_eq!(fs::read_dir(layout.staging()).unwrap().count(), 0);

        fs::remove_dir_all(layout.root()).unwrap();
    }

    #[test]
    fn stale_staging_file_is_not_a_cache_hit() {
        let (store, layout) = temp_store_fixture("stale");
        fs::create_dir_all(layout.staging()).unwrap();
        fs::write(layout.staging().join("interrupted.tmp"), b"partial").unwrap();
        let bytes = b"complete archive";
        let digest = CacheDigest::sha256(bytes);

        assert_eq!(
            store.publish_archive(digest, bytes).unwrap(),
            CachePublish::Added
        );
        assert_eq!(fs::read(layout.archive(digest)).unwrap(), bytes);

        fs::remove_dir_all(layout.root()).unwrap();
    }

    #[test]
    fn tree_publish_is_content_addressed_revalidated_and_repaired() {
        let (store, layout) = temp_store_fixture("tree-publish");
        fs::create_dir_all(layout.staging()).unwrap();
        let source = "public func value(): int { return 1 }\n";

        let first = layout.staging().join("first");
        fs::create_dir_all(first.join("src")).unwrap();
        fs::write(first.join("src/lib.aru"), source).unwrap();
        let (published, verification, tree) =
            store.publish_tree(&first, TreeLimits::default()).unwrap();
        assert_eq!(published, CachePublish::Added);
        assert_eq!(tree, layout.tree(verification.digest));
        assert_eq!(
            store
                .trusted_tree(verification.digest, TreeLimits::default())
                .unwrap(),
            tree
        );

        fs::write(tree.join("src/lib.aru"), "tampered\n").unwrap();
        let replacement = layout.staging().join("replacement");
        fs::create_dir_all(replacement.join("src")).unwrap();
        fs::write(replacement.join("src/lib.aru"), source).unwrap();
        let (published, repaired, _) = store
            .publish_tree(&replacement, TreeLimits::default())
            .unwrap();
        assert_eq!(published, CachePublish::Repaired);
        assert_eq!(repaired.digest, verification.digest);
        store
            .trusted_tree(verification.digest, TreeLimits::default())
            .unwrap();

        fs::remove_dir_all(layout.root()).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn archive_publication_quarantines_symlink_without_reading_its_target() {
        use std::os::unix::fs::symlink;

        let layout = temp_layout("archive-symlink");
        let store = CacheStore::new(layout.clone());
        let bytes = b"verified archive bytes";
        let digest = CacheDigest::sha256(bytes);
        let outside = layout.root().with_extension("outside");
        fs::write(&outside, bytes).unwrap();
        let archive = layout.archive(digest);
        create_parent(&archive).unwrap();
        symlink(&outside, &archive).unwrap();

        assert_eq!(
            store.publish_archive(digest, bytes).unwrap(),
            CachePublish::Repaired
        );
        assert_eq!(fs::read(&archive).unwrap(), bytes);
        assert_eq!(fs::read(&outside).unwrap(), bytes);
        assert!(
            !fs::symlink_metadata(&archive)
                .unwrap()
                .file_type()
                .is_symlink()
        );

        fs::remove_dir_all(layout.root()).unwrap();
        fs::remove_file(outside).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn extracted_tree_rejects_nested_symlink_escape() {
        use std::os::unix::fs::symlink;

        let layout = temp_layout("tree-symlink");
        let tree = layout.staging().join("tree");
        let outside = layout.root().join("outside");
        fs::create_dir_all(&tree).unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("secret.aru"), b"secret").unwrap();
        symlink(&outside, tree.join("escaped")).unwrap();

        assert!(matches!(
            hash_tree(&tree, TreeLimits::default()),
            Err(CacheStoreError::MalformedCache(message)) if message.contains("symlink")
        ));
        fs::remove_dir_all(layout.root()).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn extracted_tree_rejects_nested_junction_escape() {
        use std::process::Command;

        let layout = temp_layout("tree-junction");
        let tree = layout.staging().join("tree");
        let outside = layout.root().join("outside");
        let junction = tree.join("escaped");
        fs::create_dir_all(&tree).unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("secret.aru"), b"secret").unwrap();
        let output = Command::new("cmd")
            .args(["/c", "mklink", "/J"])
            .arg(&junction)
            .arg(&outside)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "could not create test junction: {}",
            String::from_utf8_lossy(&output.stderr)
        );

        assert!(matches!(
            hash_tree(&tree, TreeLimits::default()),
            Err(CacheStoreError::MalformedCache(message)) if message.contains("symlink")
        ));
        fs::remove_dir(&junction).unwrap();
        fs::remove_dir_all(layout.root()).unwrap();
    }

    #[test]
    fn inspect_and_verify_report_tampering_deterministically() {
        let layout = temp_layout("verify-command");
        let store = CacheStore::new(layout.clone());
        let first = b"first archive";
        let second = b"second archive";
        let first_digest = CacheDigest::sha256(first);
        let second_digest = CacheDigest::sha256(second);
        store.publish_archive(first_digest, first).unwrap();
        store.publish_archive(second_digest, second).unwrap();

        let inspect = store.inspect(CacheScanLimits::default()).unwrap();
        assert_eq!(inspect.archives, 2);
        assert_eq!(
            inspect.archive_bytes,
            u64::try_from(first.len() + second.len()).unwrap()
        );
        assert_eq!(inspect.invalid_entries, 0);

        fs::write(layout.archive(second_digest), b"tampered archive").unwrap();
        let verify = store.verify(CacheScanLimits::default()).unwrap();
        assert_eq!(verify.verified, 1);
        assert_eq!(verify.corrupt, 1);
        assert_eq!(verify.invalid_entries, 0);

        fs::remove_dir_all(layout.root()).unwrap();
    }

    #[test]
    fn prune_is_dry_run_capable_and_never_removes_archives() {
        let layout = temp_layout("prune-command");
        let store = CacheStore::new(layout.clone());
        let bytes = b"keep this archive";
        let digest = CacheDigest::sha256(bytes);
        store.publish_archive(digest, bytes).unwrap();
        fs::write(layout.staging().join("stale.tmp"), b"partial").unwrap();
        fs::create_dir_all(layout.quarantine()).unwrap();
        fs::write(layout.quarantine().join("bad.corrupt"), b"bad").unwrap();

        let preview = store.prune(CacheScanLimits::default(), true).unwrap();
        assert_eq!(preview.files, 2);
        assert!(layout.staging().join("stale.tmp").exists());
        assert!(layout.quarantine().join("bad.corrupt").exists());

        let removed = store.prune(CacheScanLimits::default(), false).unwrap();
        assert_eq!(removed.files, 2);
        assert!(layout.archive(digest).exists());
        assert!(!layout.staging().join("stale.tmp").exists());
        assert!(!layout.quarantine().join("bad.corrupt").exists());

        fs::remove_dir_all(layout.root()).unwrap();
    }

    #[test]
    fn scans_fail_closed_at_entry_and_byte_limits() {
        let layout = temp_layout("limits");
        let store = CacheStore::new(layout.clone());
        let bytes = b"bounded archive";
        let digest = CacheDigest::sha256(bytes);
        store.publish_archive(digest, bytes).unwrap();

        assert!(
            store
                .verify(CacheScanLimits {
                    max_entries: 1,
                    max_bytes: u64::MAX,
                })
                .is_err()
        );
        assert!(
            store
                .verify(CacheScanLimits {
                    max_entries: 10,
                    max_bytes: 1,
                })
                .is_err()
        );

        fs::remove_dir_all(layout.root()).unwrap();
    }

    #[test]
    fn extracted_tree_digest_is_order_independent_and_detects_tampering() {
        let layout = temp_layout("tree-verify");
        let store = CacheStore::new(layout.clone());
        let archive_digest = CacheDigest::sha256(b"archive");
        let tree = layout.tree(archive_digest);
        fs::create_dir_all(tree.join("src/nested")).unwrap();
        fs::write(tree.join("src/z.aru"), b"z").unwrap();
        fs::write(tree.join("src/nested/a.aru"), b"a").unwrap();

        let first = hash_tree(&tree, TreeLimits::default()).unwrap();
        let verified = store
            .verify_tree(archive_digest, first.digest, TreeLimits::default())
            .unwrap();
        assert_eq!(verified, first);

        fs::write(tree.join("src/z.aru"), b"tampered").unwrap();
        assert!(
            store
                .verify_tree(archive_digest, first.digest, TreeLimits::default())
                .is_err()
        );
        fs::remove_dir_all(layout.root()).unwrap();
    }

    #[test]
    fn extracted_tree_rejects_expansion_bombs() {
        let layout = temp_layout("tree-limits");
        let archive_digest = CacheDigest::sha256(b"archive");
        let tree = layout.tree(archive_digest);
        fs::create_dir_all(&tree).unwrap();
        fs::write(tree.join("large.aru"), b"123456789").unwrap();
        assert!(matches!(
            hash_tree(
                &tree,
                TreeLimits {
                    max_files: 1,
                    max_bytes: 4,
                    max_depth: 2
                }
            ),
            Err(CacheStoreError::LimitExceeded(_))
        ));
        fs::remove_dir_all(layout.root()).unwrap();
    }
}
