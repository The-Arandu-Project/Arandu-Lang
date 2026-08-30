//! Salsa inputs and registration for project manifests.

use std::path::PathBuf;
use std::sync::Arc;

use super::model::ManifestData;

/// Salsa input for the project manifest.
///
/// `content_hash` is the BLAKE3 of the raw file bytes (hex). Any change to the
/// file — including whitespace or comments — updates the hash and invalidates
/// dependents. Field values are also inputs so queries can depend on `entry`
/// without re-parsing.
#[salsa::input]
pub struct ProjectManifest {
    #[returns(ref)]
    pub name: String,
    #[returns(ref)]
    pub version: String,
    #[returns(ref)]
    pub entry: String,
    /// BLAKE3-256 of raw `Arandu.toml` bytes, lowercase hex (64 chars).
    #[returns(ref)]
    pub content_hash: String,
    pub path: Arc<PathBuf>,
}

/// Register a loaded manifest as a Salsa input on `db`.
pub fn register_manifest(
    db: &dyn salsa::Database,
    path: PathBuf,
    data: ManifestData,
    content_hash: String,
) -> ProjectManifest {
    ProjectManifest::new(
        db,
        data.name,
        data.version,
        data.entry,
        content_hash,
        Arc::new(path),
    )
}
