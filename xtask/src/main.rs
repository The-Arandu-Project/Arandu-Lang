//! Arandu workspace automation (xtask pattern).
//!
//! ```text
//! cargo run -p xtask -- check-diag-docs
//! cargo run -p xtask -- check-project-corpus
//! cargo run -p xtask -- check-project-churn
//! cargo run -p xtask -- check-project-performance
//! cargo run -p xtask -- help
//! ```

mod churn;
mod corpus;
mod fuzz_regressions;
mod performance;
mod release_contract;
mod slt6;

use std::env;
use std::path::PathBuf;
use std::process;

fn main() {
    let mut args = env::args().skip(1);
    let cmd = args.next().unwrap_or_else(|| "help".into());
    let code = match cmd.as_str() {
        "check-diag-docs" => cmd_check_diag_docs(),
        "check-project-corpus" => corpus::cmd_check_project_corpus(&workspace_root()),
        "check-project-churn" => churn::cmd_check_project_churn(&workspace_root()),
        "check-project-performance" => {
            performance::cmd_check_project_performance(&workspace_root())
        }
        "check-fuzz-regressions" => fuzz_regressions::check(&workspace_root()),
        "run-fuzz-seed" => fuzz_regressions::run_one(args),
        "check-release-contract" => release_contract::check(&workspace_root(), args.next()),
        "prepare-release" => release_contract::prepare(&workspace_root(), args.next()),
        "check-slt6-sdk" => slt6::check(&workspace_root(), args),
        "help" | "-h" | "--help" => {
            print_help();
            0
        }
        other => {
            eprintln!("unknown xtask command: {other}");
            print_help();
            2
        }
    };
    process::exit(code);
}

fn print_help() {
    eprintln!(
        "\
xtask — Arandu workspace tasks

Commands:
  check-diag-docs   Bijection: DiagCode (user-facing) ↔ docs/errors/*.md
  check-project-corpus  Validate S2 projects and incremental ↔ clean equivalence
  check-project-churn   Run deterministic S2 module and identity churn
  check-project-performance  Measure S2 cold/noop/edit and retention budgets
  check-fuzz-regressions  Run the versioned adversarial corpus with isolation
  check-release-contract  Validate component versions and an optional v* tag
  prepare-release    Update every Arandu component to one version atomically
  check-slt6-sdk     Exercise an installed SDK outside the repository
  help              This message

Examples:
  cargo run -p xtask -- check-diag-docs
  cargo run -p xtask -- check-project-corpus
  cargo run -p xtask -- check-project-churn
  cargo run -p xtask -- check-project-performance
  cargo run -p xtask -- check-fuzz-regressions
  cargo run -p xtask -- check-release-contract [vX.Y.Z[-rc.N]]
  cargo run -p xtask -- prepare-release X.Y.Z[-rc.N]
  cargo run -p xtask -- check-slt6-sdk --arandu PATH --work-dir DIR --evidence-dir DIR
  ./scripts/check-diag-docs.sh
"
    );
}

/// Source of truth = `DiagCode` enum; docs must match exactly (no manual code list).
fn cmd_check_diag_docs() -> i32 {
    let root = workspace_root();
    let docs_dir = root.join("docs/errors");
    let (missing, orphaned) = arandu_diagnostics::diag_doc_diff(&docs_dir);

    if missing.is_empty() && orphaned.is_empty() {
        let n = arandu_diagnostics::DiagCode::ALL
            .iter()
            .filter(|c| c.requires_error_doc())
            .count();
        println!("check-diag-docs: ok ({n} user-facing DiagCode(s) ↔ docs/errors)");
        return 0;
    }

    if !missing.is_empty() {
        eprintln!("error: missing docs/errors/{{CODE}}.md for DiagCode variants:");
        for code in &missing {
            eprintln!("  - docs/errors/{code}.md   (add doc when declaring DiagCode)");
        }
    }
    if !orphaned.is_empty() {
        eprintln!("error: orphaned docs (no matching DiagCode / not user-facing):");
        for doc in &orphaned {
            eprintln!("  - docs/errors/{doc}.md   (remove or rename after code change)");
        }
    }
    eprintln!();
    eprintln!("DiagCode is the single source of truth — do not maintain a parallel list.");
    1
}

fn workspace_root() -> PathBuf {
    // xtask lives at $ROOT/xtask — walk up from CARGO_MANIFEST_DIR.
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .expect("xtask parent = workspace root")
        .to_path_buf()
}
