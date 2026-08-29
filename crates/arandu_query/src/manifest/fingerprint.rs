//! Stable hashing and Salsa tracked fingerprint query for manifests.

use super::input::ProjectManifest;

/// BLAKE3 hex of `bytes` (stable invalidation fingerprint).
#[must_use]
pub fn hash_manifest_bytes(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

/// Tracked helper so dependents can pin work to the manifest fingerprint.
///
/// Exists primarily so the Salsa graph records the input edge from day 1
/// (even while the CLI still drives entry selection).
#[salsa::tracked]
pub fn manifest_fingerprint(db: &dyn crate::db::ArandCompilerDb, m: ProjectManifest) -> String {
    // Include fields + hash so any change shows up in explain-rebuild keys.
    format!(
        "{}@{}:{}#{}",
        m.name(db),
        m.version(db),
        m.entry(db),
        m.content_hash(db)
    )
}
