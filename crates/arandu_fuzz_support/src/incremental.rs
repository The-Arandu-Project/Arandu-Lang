//! Differential edit sequences: a warm database must agree with a fresh one.
//! The oracle compares diagnostic contents, not implementation hashes.

use arandu_query::{file_ide_diagnostics, AnalysisHost, SourceFile};

const PATHS: [&str; 2] = ["dependency.aru", "consumer.aru"];
const LIBRARY: &str = "module dependency\npublic func value(): int { return 1 }\n";
const CONSUMER: &str =
    "module consumer\nimport dependency\nfunc main(): int { return dependency.value() }\n";
const MAX_EDITS: usize = 24;

fn register(host: &mut AnalysisHost, sources: &[String; 2]) -> [SourceFile; 2] {
    std::array::from_fn(|index| host.new_file(PATHS[index].into(), sources[index].clone()))
}

pub(super) fn run(data: &[u8]) {
    let mut sources = [LIBRARY.to_owned(), CONSUMER.to_owned()];
    let mut warm = AnalysisHost::new();
    let files = register(&mut warm, &sources);
    for file in files {
        assert!(
            file_ide_diagnostics(warm.db(), file).is_empty(),
            "generator baseline must be valid"
        );
    }
    for (step, &operation) in data.iter().take(MAX_EDITS).enumerate() {
        let (index, replacement) = match operation % 10 {
            0 => (
                0,
                format!("module dependency\npublic func value(): int {{ return {operation} }}\n"),
            ),
            1 => (
                0,
                "module dependency\npublic func value(): str { return \"text\" }\n".into(),
            ),
            2 => (
                0,
                "module dependency\npublic func renamed(): int { return 1 }\n".into(),
            ),
            3 => (
                0,
                "module dependency\npublic func value(): int { let 1 = 2; return 0 }\n".into(),
            ),
            4 => (0, LIBRARY.into()),
            5 => (
                1,
                "module consumer\nimport dependency\nfunc main(): int { return unknown }\n".into(),
            ),
            6 => (1, format!("// ação 🦀\n{CONSUMER}")),
            7 => (
                1,
                "module consumer\nimport dependency\nfunc main(): int { let 1 = 2; let 3 = 4; }\n"
                    .into(),
            ),
            8 => (1, CONSUMER.into()),
            _ => (
                1,
                format!("{CONSUMER}\nfunc sibling(): int {{ return {operation} }}\n"),
            ),
        };
        sources[index] = replacement;
        warm.set_text(files[index], sources[index].as_str());

        let mut fresh = AnalysisHost::new();
        let fresh_files = register(&mut fresh, &sources);
        // Alternate demand order without changing registration identities.
        for index in if step % 2 == 0 { [1, 0] } else { [0, 1] } {
            let actual = file_ide_diagnostics(warm.db(), files[index]);
            let expected = file_ide_diagnostics(fresh.db(), fresh_files[index]);
            assert_eq!(
                **actual, **expected,
                "incremental/cold mismatch: step={step}, operations={:?}, file={}\n--- dependency ---\n{}\n--- consumer ---\n{}",
                &data[..=step], PATHS[index], sources[0], sources[1]
            );
            assert!(actual.iter().all(|diagnostic| !diagnostic.code.starts_with("ICE")),
                "generated source must recover without ICE: step={step}, operations={:?}, diagnostics={:?}", &data[..=step], **actual);
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn dependency_and_syntax_edits_match_cold_analysis() {
        super::run(&[1, 4, 2, 4, 3, 4, 5, 8, 6, 7, 8, 9, 0, 4, 8]);
    }

    #[test]
    fn all_pairs_of_edit_operations_match_cold_analysis() {
        for first in 0..10 {
            for second in 0..10 {
                super::run(&[first, second, 4, 8]);
            }
        }
    }
}
