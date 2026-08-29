//! Enforces the lifecycle documented in `docs/README.md`: one permanent
//! roadmap, specialized catalogs/releases, and a stable shape for technical
//! documents after their temporary campaign plan is removed.

use std::fs;
use std::path::Path;

const REQUIRED_SECTIONS: &[&str] = &[
    "## Visão Geral e Contexto",
    "## Detalhes Técnicos da Implementação",
    "## PONTOS DE MELHORIA (O que não está no roadmap)",
    "## Futuro e Próximos Passos",
];

const SPECIALIZED_TOP_LEVEL: &[&str] = &[
    "README.md",
    "arandu-compiler-roadmap-v0.1.md",
    "arandu-project-package-migration-v0.1.md",
    "release-verification.md",
];

pub fn check(root: &Path) -> i32 {
    match violations(&root.join("docs")) {
        Ok(violations) if violations.is_empty() => {
            println!("check-docs-taxonomy: ok (one roadmap; permanent docs are consolidated)");
            0
        }
        Ok(violations) => {
            eprintln!("check-docs-taxonomy: violation(s):");
            for violation in violations {
                eprintln!("  - {violation}");
            }
            1
        }
        Err(error) => {
            eprintln!("check-docs-taxonomy: {error}");
            1
        }
    }
}

fn violations(docs: &Path) -> Result<Vec<String>, String> {
    let mut entries = fs::read_dir(docs)
        .map_err(|error| format!("read {}: {error}", docs.display()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("read {} entry: {error}", docs.display()))?;
    entries.sort_by_key(fs::DirEntry::file_name);

    let mut violations = Vec::new();
    for entry in entries {
        let path = entry.path();
        if !path.is_file() || path.extension().and_then(|ext| ext.to_str()) != Some("md") {
            continue;
        }
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if SPECIALIZED_TOP_LEVEL.contains(&name) {
            continue;
        }
        let text = fs::read_to_string(&path)
            .map_err(|error| format!("read {} as UTF-8: {error}", path.display()))?;

        let mut previous = 0;
        for section in REQUIRED_SECTIONS {
            let Some(position) = text.find(section) else {
                violations.push(format!("docs/{name} is missing `{section}`"));
                continue;
            };
            if position < previous {
                violations.push(format!("docs/{name} has permanent sections out of order"));
            }
            previous = position;
        }
        if text.lines().any(is_task_checkbox) {
            violations.push(format!(
                "docs/{name} contains a task checkbox; keep open work only in the master roadmap"
            ));
        }
    }
    Ok(violations)
}

fn is_task_checkbox(line: &str) -> bool {
    let line = line.trim_start();
    line.starts_with("- [ ] ") || line.starts_with("- [x] ") || line.starts_with("- [X] ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_checkbox_detection_does_not_reject_plain_brackets() {
        assert!(is_task_checkbox("- [ ] pending"));
        assert!(is_task_checkbox("  - [x] complete"));
        assert!(!is_task_checkbox("- [status] prose"));
    }
}
