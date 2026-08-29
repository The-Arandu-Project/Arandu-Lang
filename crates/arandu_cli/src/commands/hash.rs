//! File integrity hash computation (BLAKE3-256 hex).

use std::fs;
use std::path::Path;

use crate::cli_error::{CliFailure, CliResult, CliSuccess};

/// Print BLAKE3-256 hex of a file (packaging / install integrity).
pub fn cmd_hash_file(path: &Path) -> CliResult {
    match fs::read(path) {
        Ok(bytes) => {
            println!("{}", blake3::hash(&bytes).to_hex());
            Ok(CliSuccess::Done)
        }
        Err(error) => Err(CliFailure::operational(
            "failed to read",
            Some(path.to_path_buf()),
            error.to_string(),
        )),
    }
}
