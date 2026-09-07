//! Compare live package diagnostics after structural edits with a cold DB.
use arandu_query::{
    file_ide_diagnostics, AnalysisHost, DirectoryListing, IdeDiagnostic, ManifestData, SourceFile,
};

const PATHS: [&str; 3] = ["main.aru", "dep.aru", "other.aru"];
const MAIN: &str = "import graph.dep as dep\nfunc main(): int { return dep.value() }\n";
const DEP: &str = "public func value(): int { return 1 }\n";
type Sources = [Option<String>; 3];
type Files = [Option<SourceFile>; 3];

fn path(index: usize) -> String {
    std::path::Path::new("graph")
        .join(PATHS[index])
        .to_string_lossy()
        .into_owned()
}

fn entries(sources: &Sources) -> Vec<String> {
    sources
        .iter()
        .enumerate()
        .filter_map(|(index, text)| text.as_ref().map(|_| PATHS[index].into()))
        .collect()
}

fn cold(sources: &Sources) -> (AnalysisHost, Files, DirectoryListing) {
    let mut host = AnalysisHost::new();
    let files = std::array::from_fn(|index| {
        sources[index]
            .as_ref()
            .map(|text| host.new_file(path(index), text.clone()))
    });
    let (_, listing, _) = host.configure_package(
        "Arandu.toml".into(),
        ManifestData::legacy("graph".into(), "0.1.0".into(), "main.aru".into()),
        "fixture".into(),
        "graph".into(),
        entries(sources),
        None,
    );
    (host, files, listing)
}

fn diagnostics(
    host: &AnalysisHost,
    files: &Files,
    file: SourceFile,
    operations: &[u8],
) -> Vec<IdeDiagnostic> {
    // FileIds are monotonic and deliberately differ after deletion/recreation.
    // Normalize only file identity, retaining diagnostic order and all contents.
    let canonical_id = |id| {
        // Signature-cycle recovery currently emits a source-less (0, 0, 0)
        // diagnostic. Preserve that sentinel independently of live identities.
        if id == 0 {
            return 0;
        }
        let index = files
            .iter()
            .position(|file| file.is_some_and(|file| *file.file_id(host.db()) == id))
            .unwrap_or_else(|| panic!("diagnostic references missing file {id}: operations={operations:?}, live={:?}, diagnostics={:?}", files.map(|f| f.map(|f| *f.file_id(host.db()))), **file_ide_diagnostics(host.db(), file)));
        u32::try_from(index + 1).expect("three fixture files")
    };
    let mut diagnostics = file_ide_diagnostics(host.db(), file).to_vec();
    for diagnostic in &mut diagnostics {
        diagnostic.file_id = canonical_id(diagnostic.file_id);
        for label in &mut diagnostic.labels {
            label.file_id = canonical_id(label.file_id);
        }
        for hint in &mut diagnostic.hints {
            if let Some(replacement) = &mut hint.replacement {
                replacement.file_id = canonical_id(replacement.file_id);
            }
        }
        if let Some(function) = &mut diagnostic.func {
            function.file_id = canonical_id(function.file_id);
        }
        assert!(
            !diagnostic.code.starts_with("ICE"),
            "generated graph must recover: {diagnostic:?}"
        );
    }
    diagnostics
}

pub(super) fn run(data: &[u8]) {
    let mut sources: Sources = [Some(MAIN.into()), Some(DEP.into()), None];
    let (mut warm, mut files, listing) = cold(&sources);
    for file in files.iter().flatten() {
        assert!(
            diagnostics(&warm, &files, *file, &[]).is_empty(),
            "valid baseline"
        );
    }
    for (step, operation) in data.iter().take(24).enumerate() {
        match operation % 10 {
            0 => sources[1] = None,
            1 | 9 => sources[1] = Some(DEP.into()),
            2 => sources[2] = sources[1].take(),
            3 => {
                sources[0] = Some(
                    "import graph.other as dep\nfunc main(): int { return dep.value() }\n".into(),
                )
            }
            4 => sources[0] = Some(MAIN.into()),
            5 => sources[1] = Some("public func value(): str { return \"text\" }\n".into()),
            6 => sources[2] = Some(DEP.into()),
            7 => sources[2] = None,
            8 => {
                sources[1] = Some(
                    "import graph.main as root\npublic func value(): int { return root.main() }\n"
                        .into(),
                )
            }
            _ => unreachable!("modulo ten"),
        }
        for index in 0..3 {
            match (files[index], sources[index].as_ref()) {
                (Some(file), Some(text)) => {
                    if file.text(warm.db()).as_ref() != text {
                        warm.set_text(file, text.as_str());
                    }
                }
                (None, Some(text)) => files[index] = Some(warm.new_file(path(index), text.clone())),
                (Some(_), None) => {
                    warm.unregister_source_file(&path(index));
                    files[index] = None;
                }
                (None, None) => {}
            }
        }
        warm.set_directory_entries(listing, entries(&sources));
        let (fresh, fresh_files, _) = cold(&sources);
        for index in if step % 2 == 0 { [2, 1, 0] } else { [0, 1, 2] } {
            if let (Some(actual), Some(expected)) = (files[index], fresh_files[index]) {
                assert_eq!(
                    diagnostics(&warm, &files, actual, &data[..=step]),
                    diagnostics(&fresh, &fresh_files, expected, &data[..=step]),
                    "graph mismatch: step={step}, operations={:?}, file={}, sources={sources:?}",
                    &data[..=step],
                    PATHS[index]
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn all_structural_edit_pairs_match_cold_analysis() {
        for first in 0..10 {
            for second in 0..10 {
                super::run(&[first, second, 1, 4, 7]);
            }
        }
    }

    #[test]
    fn longer_structural_edit_sequences_match_cold_analysis() {
        for seed in 1..=32u64 {
            let mut state = seed;
            let operations: [u8; 24] = std::array::from_fn(|_| {
                state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
                state.to_le_bytes()[7]
            });
            super::run(&operations);
        }
    }
}
