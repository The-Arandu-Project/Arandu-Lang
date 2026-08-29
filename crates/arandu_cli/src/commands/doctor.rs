//! Installation, stdlib, and environment diagnosis.

use crate::cli_error::{CliResult, CliSuccess};
use crate::project::{self, ProjectFlags};

pub fn cmd_doctor(flags: &ProjectFlags) -> CliResult {
    let exit_code = project::cmd_doctor(flags);
    Ok(CliSuccess::ProgramExit(exit_code))
}
