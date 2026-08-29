//! Project manifest (`arandu.toml`) — pure parsing, validation, and Salsa input.
//!
//! Salsa input from day 1: the manifest is a `#[salsa::input]` whose **content hash**
//! participates in the invalidation key.
//!
//! Pure query crate invariant: this module performs NO direct filesystem I/O.
//! Frontends (CLI/LSP) read bytes from disk and call `parse_manifest_bytes`.

pub mod fingerprint;
pub mod input;
pub mod model;
pub mod parse;
pub mod schema;
pub mod validate;

pub use fingerprint::{hash_manifest_bytes, manifest_fingerprint};
pub use input::{register_manifest, ProjectManifest};
pub use model::{
    CapabilityPolicy, EffectPolicy, ManifestData, ManifestDependency, ManifestDiscovery,
    ManifestEdition, ManifestError, ManifestSpelling, ManifestTarget, ManifestWorkspace,
    PackageKind, LEGACY_MANIFEST_FILENAME, MANIFEST_FILENAME,
};
pub use parse::{parse_manifest_bytes, parse_manifest_str};
pub use validate::{
    ensure_toolchain_compatible, validate_git_dependency_identity, validate_git_origin,
};

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn parse_happy_path() {
        let text = r#"
# comment
name = "hello"
version = "0.0.1"
entry = "src/main.aru"
"#;
        let data = parse_manifest_str(Path::new("Arandu.toml"), text).unwrap();
        assert_eq!(data.name, "hello");
        assert_eq!(data.version, "0.0.1");
        assert_eq!(data.entry, "src/main.aru");
    }

    #[test]
    fn parse_bytes_identical_to_str() {
        let text = r#"
schema = 1
[package]
name = "hello"
version = "1.0.0"
edition = "2026"
[targets.bin]
name = "hello"
root = "src/main.aru"
"#;
        let from_str = parse_manifest_str(Path::new("arandu.toml"), text).unwrap();
        let from_bytes = parse_manifest_bytes(Path::new("arandu.toml"), text.as_bytes()).unwrap();
        assert_eq!(from_str, from_bytes);
    }

    #[test]
    fn parse_schema_one_with_path_dependency() {
        let text = r#"
schema = 1

[package]
name = "hello"
version = "1.2.3"
edition = "2026"

[toolchain]
arandu = ">=0.1.0-rc.4, <0.2.0"

[targets.bin]
name = "hello"
root = "src/main.aru"

[dependencies]
math = { path = "../math" }

[metadata.example]
note = "preserved for third-party tools"
"#;
        let data = parse_manifest_str(Path::new("arandu.toml"), text).unwrap();
        assert_eq!(data.name, "hello");
        assert_eq!(data.version, "1.2.3");
        assert_eq!(data.entry, "src/main.aru");
        assert_eq!(data.schema, 1);
        assert_eq!(data.edition, ManifestEdition::V2026);
        assert_eq!(data.kind, PackageKind::Binary);
        assert_eq!(
            data.toolchain_requirement.as_deref(),
            Some(">=0.1.0-rc.4, <0.2.0")
        );
        assert_eq!(
            data.dependencies["math"],
            ManifestDependency::Path {
                path: "../math".into()
            }
        );
    }

    #[test]
    fn git_dependency_requires_canonical_https_origin_and_full_commit() {
        let manifest = |dependency: &str| {
            format!(
                "schema=1\n[package]\nname='app'\nversion='1.0.0'\nedition='2026'\n[targets.bin]\nname='app'\nroot='src/main.aru'\n[dependencies]\nmath={dependency}\n"
            )
        };
        let commit = "0123456789abcdef0123456789abcdef01234567";
        let valid = manifest(&format!(
            "{{ git='https://github.com/example/math.git', rev='{commit}' }}"
        ));
        let data = parse_manifest_str(Path::new("arandu.toml"), &valid).unwrap();
        assert_eq!(
            data.dependencies["math"],
            ManifestDependency::Git {
                origin: "https://github.com/example/math.git".into(),
                commit: commit.into(),
            }
        );

        for dependency in [
            "{ git='http://github.com/example/math.git', rev='0123456789abcdef0123456789abcdef01234567' }",
            "{ git='https://token@github.com/example/math.git', rev='0123456789abcdef0123456789abcdef01234567' }",
            "{ git='https://github.com/example/math', rev='0123456789abcdef0123456789abcdef01234567' }",
            "{ git='https://github.com/example/math.git', rev='main' }",
            "{ git='https://github.com/example/math.git', rev='0123456' }",
            "{ git='https://github.com/example/math.git', rev='0123456789ABCDEF0123456789ABCDEF01234567' }",
            "{ git='https://github.com/example/math.git', rev='0123456789abcdef0123456789abcdef01234567', branch='main' }",
            "{ path='../math', git='https://github.com/example/math.git', rev='0123456789abcdef0123456789abcdef01234567' }",
        ] {
            assert!(
                parse_manifest_str(Path::new("arandu.toml"), &manifest(dependency)).is_err(),
                "unsafe or ambiguous dependency was accepted: {dependency}"
            );
        }
    }

    #[test]
    fn library_exports_are_typed_and_reject_deep_import_ambiguity() {
        let text = r#"
schema = 1
[package]
name = "math"
version = "1.0.0"
edition = "2026"
[targets.lib]
name = "math"
root = "src/lib.aru"
[targets.lib.exports]
"." = "src/lib.aru"
"geometry.vector" = "src/geometry/vector.aru"
"#;
        let data = parse_manifest_str(Path::new("arandu.toml"), text).unwrap();
        assert_eq!(
            data.library_target.unwrap().exports["geometry.vector"],
            "src/geometry/vector.aru"
        );

        let collision = text.replace(
            "\"geometry.vector\" = \"src/geometry/vector.aru\"",
            "\"Geometry\" = \"src/geometry.aru\"\n\"geometry\" = \"src/other.aru\"",
        );
        assert!(parse_manifest_str(Path::new("arandu.toml"), &collision)
            .unwrap_err()
            .to_string()
            .contains("case-fold collision"));
    }

    #[test]
    fn binary_target_cannot_publish_module_exports() {
        let text = r#"
schema = 1
[package]
name = "app"
version = "1.0.0"
edition = "2026"
[targets.bin]
name = "app"
root = "src/main.aru"
[targets.bin.exports]
"." = "src/main.aru"
"#;
        assert!(parse_manifest_str(Path::new("arandu.toml"), text)
            .unwrap_err()
            .to_string()
            .contains("only a library target"));
    }

    #[test]
    fn mixed_package_prefers_binary_entry_and_preserves_library_target() {
        let text = r#"
schema = 1
[package]
name = "mixed"
version = "1.0.0"
edition = "2026"
[targets.lib]
name = "mixed"
root = "src/lib.aru"
[targets.bin]
name = "mixed-cli"
root = "src/main.aru"
"#;
        let data = parse_manifest_str(Path::new("arandu.toml"), text).unwrap();
        assert_eq!(data.kind, PackageKind::Mixed);
        assert_eq!(data.entry, "src/main.aru");
        assert_eq!(data.library_target.unwrap().root, "src/lib.aru");
    }

    #[test]
    fn toolchain_requirement_is_enforced_against_explicit_version() {
        let text = r#"
schema = 1
[package]
name = "future"
version = "1.0.0"
edition = "2026"
[toolchain]
arandu = ">=9.0.0"
[targets.lib]
name = "future"
root = "src/lib.aru"
"#;
        let data = parse_manifest_str(Path::new("arandu.toml"), text).unwrap();
        let error =
            ensure_toolchain_compatible(Path::new("arandu.toml"), &data, "0.1.0-rc.4").unwrap_err();
        assert!(matches!(error, ManifestError::IncompatibleToolchain { .. }));
        assert!(ensure_toolchain_compatible(Path::new("arandu.toml"), &data, "9.1.0").is_ok());
    }

    #[test]
    fn capability_and_effect_policy_are_typed_but_not_claimed_as_inferred() {
        let text = r#"
schema = 1
[package]
name = "server"
version = "1.0.0"
edition = "2026"
[targets.bin]
name = "server"
root = "src/main.aru"
[capabilities]
network = ["api.example.com:443"]
filesystem_read = ["assets/**"]
foreign = false
[policy.effects]
deny_new_authority = true
warn_new_resources = true
deny = ["UnknownCapability"]
"#;
        let data = parse_manifest_str(Path::new("arandu.toml"), text).unwrap();
        assert_eq!(data.capabilities.network, ["api.example.com:443"]);
        assert_eq!(data.capabilities.filesystem_read, ["assets/**"]);
        assert!(!data.capabilities.foreign);
        assert_eq!(data.effect_policy.deny, ["UnknownCapability"]);
    }

    #[test]
    fn parse_rejects_unknown_owned_field() {
        let text = r#"
schema = 1
[package]
name = "hello"
version = "1.2.3"
edition = "2026"
surprise = true
[targets.bin]
name = "hello"
root = "src/main.aru"
"#;
        let err = parse_manifest_str(Path::new("arandu.toml"), text).unwrap_err();
        assert!(err.to_string().contains("unknown field `surprise`"));
    }

    #[test]
    fn parse_rejects_duplicate_keys_and_host_foreign_absolute_paths() {
        let duplicate = r#"
schema = 1
schema = 1
[package]
name = "hello"
version = "1.0.0"
edition = "2026"
[targets.bin]
name = "hello"
root = "src/main.aru"
"#;
        assert!(matches!(
            parse_manifest_str(Path::new("arandu.toml"), duplicate),
            Err(ManifestError::Parse { .. })
        ));

        let windows_absolute = duplicate
            .replacen("schema = 1\nschema = 1", "schema = 1", 1)
            .replace("src/main.aru", "C:/project/main.aru");
        assert!(
            parse_manifest_str(Path::new("arandu.toml"), &windows_absolute)
                .unwrap_err()
                .to_string()
                .contains("unsafe `target root`")
        );
    }

    #[test]
    fn parse_rejects_invalid_semver_and_unsafe_target_path() {
        let invalid_version = r#"
schema = 1
[package]
name = "hello"
version = "latest"
edition = "2026"
[targets.bin]
name = "hello"
root = "src/main.aru"
"#;
        assert!(
            parse_manifest_str(Path::new("arandu.toml"), invalid_version)
                .unwrap_err()
                .to_string()
                .contains("not valid SemVer")
        );

        let escaping_root = invalid_version
            .replace("latest", "1.0.0")
            .replace("src/main.aru", "../main.aru");
        assert!(parse_manifest_str(Path::new("arandu.toml"), &escaping_root)
            .unwrap_err()
            .to_string()
            .contains("unsafe `target root`"));
    }

    #[test]
    fn parse_missing_entry_errors() {
        let text = r#"name = "x"
version = "1.0.0"
"#;
        let err = parse_manifest_str(Path::new("Arandu.toml"), text).unwrap_err();
        assert!(matches!(
            err,
            ManifestError::MissingField { field: "entry", .. }
        ));
    }

    #[test]
    fn parse_malformed_line_errors() {
        let text = "name = hello\n";
        let err = parse_manifest_str(Path::new("Arandu.toml"), text).unwrap_err();
        assert!(matches!(err, ManifestError::Parse { .. }));
    }

    #[test]
    fn parse_rejects_tables() {
        let text = r#"
name = "x"
version = "0.0.1"
entry = "src/main.aru"
[dependencies]
"#;
        let err = parse_manifest_str(Path::new("Arandu.toml"), text).unwrap_err();
        assert!(matches!(err, ManifestError::Parse { .. }));
    }

    #[test]
    fn content_hash_stable() {
        let a = hash_manifest_bytes(b"name = \"a\"\n");
        let b = hash_manifest_bytes(b"name = \"a\"\n");
        let c = hash_manifest_bytes(b"name = \"b\"\n");
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_eq!(a.len(), 64);
    }
}
