//! VCS initialization and probe helpers for project scaffolding.

use std::path::Path;
use std::process::Command;

use crate::cli_error::CliFailure;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VcsChoice {
    Auto,
    Git,
    None,
}

pub fn initialize_git(root: &Path, vcs_probe: &Path, vcs: VcsChoice) -> Result<(), CliFailure> {
    let initialize_git = match vcs {
        VcsChoice::None => false,
        VcsChoice::Git => true,
        VcsChoice::Auto => !has_git_ancestor(vcs_probe),
    };
    if initialize_git {
        let status = Command::new("git")
            .arg("init")
            .arg("--quiet")
            .arg(root)
            .status()
            .map_err(|e| {
                CliFailure::operational(
                    "initialize Git repository",
                    Some(root.to_path_buf()),
                    e.to_string(),
                )
            })?;
        if !status.success() {
            return Err(CliFailure::operational(
                "initialize Git repository",
                Some(root.to_path_buf()),
                format!("git exited with {status}"),
            ));
        }
    }
    Ok(())
}

pub fn has_git_ancestor(start: &Path) -> bool {
    start.ancestors().any(|path| path.join(".git").exists())
}
