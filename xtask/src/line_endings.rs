//! Repository line-ending policy validation.

use std::path::Path;
use std::process::Command;

pub fn check(workspace: &Path) -> i32 {
    let output = match Command::new("git")
        .args(["-C"])
        .arg(workspace)
        .args(["ls-files", "--eol", "-z"])
        .output()
    {
        Ok(output) if output.status.success() => output,
        Ok(output) => {
            eprintln!(
                "check-line-endings: git ls-files failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
            return 1;
        }
        Err(error) => {
            eprintln!("check-line-endings: failed to execute git: {error}");
            return 1;
        }
    };

    let violations = index_violations(&output.stdout);
    if violations.is_empty() {
        println!("check-line-endings: ok (tracked text is canonical LF)");
        return 0;
    }

    eprintln!("check-line-endings: CRLF or mixed endings stored in the Git index:");
    for violation in violations {
        eprintln!("  - {violation}");
    }
    eprintln!("fix: review .gitattributes, then run `git add --renormalize .`");
    1
}

fn index_violations(output: &[u8]) -> Vec<String> {
    output
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
        .filter_map(|record| {
            let record = String::from_utf8_lossy(record);
            let (metadata, path) = record.split_once('\t')?;
            let index_eol = metadata.split_whitespace().next()?;
            matches!(index_eol, "i/crlf" | "i/mixed").then(|| format!("{path} ({index_eol})"))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_only_noncanonical_index_endings() {
        let records = b"i/lf    w/crlf  attr/text eol=lf\tclean.rs\0\
i/crlf w/crlf  attr/text eol=lf\tlegacy.toml\0\
i/mixed w/mixed attr/text eol=lf\tmixed.md\0\
i/-text w/-text attr/-text\timage.png\0";
        assert_eq!(
            index_violations(records),
            ["legacy.toml (i/crlf)", "mixed.md (i/mixed)"]
        );
    }
}
