use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use arandu_middle::Severity;
use arandu_query::{
    parse_manifest_bytes, scan_aru_entries, DatabaseImpl, DirectoryListing, ModuleRoots, SourceFile,
};
use salsa::Setter;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expectation {
    Ok,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CorpusCase {
    path: String,
    expectation: Expectation,
    features: BTreeSet<String>,
    codes: BTreeSet<String>,
    crlf: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Observation {
    diagnostics: Vec<String>,
    amir: Option<String>,
}

struct CorpusSession {
    db: DatabaseImpl,
    package_name: String,
    entry_rel: String,
    files: BTreeMap<String, Vec<SourceFile>>,
}

pub fn cmd_check_project_corpus(workspace_root: &Path) -> i32 {
    match check_project_corpus(workspace_root) {
        Ok(summary) => {
            println!(
                "check-project-corpus: ok ({} cases, {} revisions, {} files, {} lines, {} bytes, {} modules)",
                summary.cases,
                summary.revisions,
                summary.files,
                summary.lines,
                summary.bytes,
                summary.modules
            );
            0
        }
        Err(error) => {
            eprintln!("check-project-corpus: error: {error}");
            1
        }
    }
}

#[derive(Default)]
struct CorpusSummary {
    cases: usize,
    revisions: usize,
    files: usize,
    lines: usize,
    bytes: usize,
    modules: usize,
}

fn check_project_corpus(workspace_root: &Path) -> Result<CorpusSummary, String> {
    let root = workspace_root.join("tests/projects");
    let manifest_path = root.join("corpus.txt");
    let manifest_text = fs::read_to_string(&manifest_path)
        .map_err(|error| format!("failed to read {}: {error}", manifest_path.display()))?;
    let cases = parse_corpus_manifest(&manifest_text)?;
    validate_discovery(&root, &cases)?;

    let mut summary = CorpusSummary {
        cases: cases.len(),
        ..CorpusSummary::default()
    };
    let mut all_features = BTreeSet::new();

    for case in &cases {
        all_features.extend(case.features.iter().cloned());
        let project_root = root.join(&case.path);
        let mut state = load_source_state(&project_root, case.crlf)?;
        update_summary(&mut summary, &state);

        let ascending = analyze_clean(&project_root, &state, false)?;
        let descending = analyze_clean(&project_root, &state, true)?;
        if ascending != descending {
            return Err(format!(
                "{} depends on source registration order\nascending={ascending:#?}\ndescending={descending:#?}",
                case.path
            ));
        }
        validate_expectation(case, &ascending, "base")?;

        let mut session = CorpusSession::new(&project_root, &state)?;
        let incremental_base = session.observe()?;
        if incremental_base != ascending {
            return Err(format!(
                "{} initial incremental DB differs from clean DB",
                case.path
            ));
        }

        for revision in revision_dirs(&project_root)? {
            summary.revisions += 1;
            apply_revision(&revision, case.crlf, &mut state, &mut session)?;
            let incremental = session.observe()?;
            let clean = analyze_clean(&project_root, &state, false)?;
            if incremental != clean {
                return Err(format!(
                    "{} revision {} differs between incremental and clean DB\nincremental={incremental:#?}\nclean={clean:#?}",
                    case.path,
                    revision.file_name().unwrap_or_default().to_string_lossy()
                ));
            }
            validate_expectation(
                case,
                &incremental,
                &revision.file_name().unwrap_or_default().to_string_lossy(),
            )?;
        }
    }

    let required = [
        "async",
        "crlf",
        "cycles",
        "generics",
        "multi-file",
        "ownership",
        "unicode",
    ];
    for feature in required {
        if !all_features.contains(feature) {
            return Err(format!(
                "manifest does not cover required S2-A feature `{feature}`"
            ));
        }
    }
    Ok(summary)
}

fn parse_corpus_manifest(text: &str) -> Result<Vec<CorpusCase>, String> {
    let mut cases = Vec::new();
    let mut seen = BTreeSet::new();
    for (index, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let fields: Vec<_> = line.split('|').map(str::trim).collect();
        if fields.len() != 5 {
            return Err(format!(
                "corpus.txt line {}: expected path|expectation|features|codes|newline",
                index + 1
            ));
        }
        let path = normalize_relative(fields[0])?;
        if !seen.insert(path.clone()) {
            return Err(format!(
                "corpus.txt line {}: duplicate case `{path}`",
                index + 1
            ));
        }
        let expectation = match fields[1] {
            "ok" => Expectation::Ok,
            "error" => Expectation::Error,
            value => {
                return Err(format!(
                    "corpus.txt line {}: invalid expectation `{value}`",
                    index + 1
                ))
            }
        };
        let features = parse_set(fields[2]);
        if features.is_empty() {
            return Err(format!(
                "corpus.txt line {}: features must not be empty",
                index + 1
            ));
        }
        let codes = if fields[3] == "-" {
            BTreeSet::new()
        } else {
            parse_set(fields[3])
        };
        let crlf = match fields[4] {
            "lf" => false,
            "crlf" => true,
            value => {
                return Err(format!(
                    "corpus.txt line {}: invalid newline `{value}`",
                    index + 1
                ))
            }
        };
        cases.push(CorpusCase {
            path,
            expectation,
            features,
            codes,
            crlf,
        });
    }
    if cases.is_empty() {
        return Err("corpus.txt contains no cases".into());
    }
    cases.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(cases)
}

fn parse_set(value: &str) -> BTreeSet<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(str::to_owned)
        .collect()
}

fn normalize_relative(value: &str) -> Result<String, String> {
    let normalized = value.replace('\\', "/");
    let path = Path::new(&normalized);
    if normalized.is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
    {
        return Err(format!("invalid relative corpus path `{value}`"));
    }
    Ok(normalized)
}

fn validate_discovery(root: &Path, cases: &[CorpusCase]) -> Result<(), String> {
    let listed: BTreeSet<_> = cases.iter().map(|case| case.path.clone()).collect();
    let mut discovered = BTreeSet::new();
    for class in sorted_dirs(root)? {
        for project in sorted_dirs(&class)? {
            if project.join("Arandu.toml").is_file() {
                let relative = project
                    .strip_prefix(root)
                    .map_err(|error| error.to_string())?;
                discovered.insert(relative.to_string_lossy().replace('\\', "/"));
            }
        }
    }
    let missing: Vec<_> = listed.difference(&discovered).cloned().collect();
    let orphaned: Vec<_> = discovered.difference(&listed).cloned().collect();
    if missing.is_empty() && orphaned.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "corpus manifest mismatch: missing={missing:?}, orphaned={orphaned:?}"
        ))
    }
}

fn load_source_state(project_root: &Path, crlf: bool) -> Result<BTreeMap<String, String>, String> {
    let src = project_root.join("src");
    let mut state = BTreeMap::new();
    for relative in scan_aru_entries(&src) {
        let text = fs::read_to_string(src.join(&relative))
            .map_err(|error| format!("failed to read {relative}: {error}"))?;
        state.insert(relative.replace('\\', "/"), normalize_newlines(text, crlf));
    }
    if state.is_empty() {
        return Err(format!(
            "{} contains no .aru modules",
            project_root.display()
        ));
    }
    Ok(state)
}

fn normalize_newlines(text: String, crlf: bool) -> String {
    let lf = text.replace("\r\n", "\n").replace('\r', "\n");
    if crlf {
        lf.replace('\n', "\r\n")
    } else {
        lf
    }
}

impl CorpusSession {
    fn new(project_root: &Path, state: &BTreeMap<String, String>) -> Result<Self, String> {
        let manifest_path = project_root.join("Arandu.toml");
        let manifest_bytes = fs::read(&manifest_path).map_err(|error| error.to_string())?;
        let manifest = parse_manifest_bytes(&manifest_path, &manifest_bytes)
            .map_err(|error| error.to_string())?;
        let entry_rel = manifest
            .entry
            .strip_prefix("src/")
            .unwrap_or(&manifest.entry)
            .replace('\\', "/");
        let mut db = DatabaseImpl::new();
        let src = project_root.join("src");
        let entries = state.keys().cloned().collect::<Vec<_>>();
        let listing = DirectoryListing::new(&db, Arc::new(src.clone()), Arc::new(entries));
        let roots = ModuleRoots::new(&db, manifest.name.clone(), Arc::new(src), None, listing);
        db.set_module_roots(roots);

        let mut files = BTreeMap::new();
        for (relative, text) in state {
            let aliases = vec![
                db.new_file(format!("{}/{}", manifest.name, relative), text.clone()),
                db.new_file(relative.clone(), text.clone()),
            ];
            files.insert(relative.clone(), aliases);
        }
        Ok(Self {
            db,
            package_name: manifest.name,
            entry_rel,
            files,
        })
    }

    fn set_text(&mut self, relative: &str, text: &str) -> Result<(), String> {
        let aliases = self
            .files
            .get(relative)
            .ok_or_else(|| format!("revision changes unknown module `{relative}`"))?;
        for file in aliases {
            file.set_text(&mut self.db).to(Arc::from(text));
        }
        Ok(())
    }

    fn observe(&self) -> Result<Observation, String> {
        let key = format!("{}/{}", self.package_name, self.entry_rel);
        let entry = self
            .db
            .source_file_by_path(&key)
            .ok_or_else(|| format!("entry `{key}` is not registered"))?;
        observe(&self.db, entry)
    }
}

fn analyze_clean(
    project_root: &Path,
    state: &BTreeMap<String, String>,
    reverse: bool,
) -> Result<Observation, String> {
    let manifest_path = project_root.join("Arandu.toml");
    let manifest_bytes = fs::read(&manifest_path).map_err(|error| error.to_string())?;
    let manifest =
        parse_manifest_bytes(&manifest_path, &manifest_bytes).map_err(|error| error.to_string())?;
    let entry_rel = manifest
        .entry
        .strip_prefix("src/")
        .unwrap_or(&manifest.entry)
        .replace('\\', "/");
    let src = project_root.join("src");
    let mut db = DatabaseImpl::new();
    let entries = state.keys().cloned().collect::<Vec<_>>();
    let listing = DirectoryListing::new(&db, Arc::new(src.clone()), Arc::new(entries));
    let roots = ModuleRoots::new(&db, manifest.name.clone(), Arc::new(src), None, listing);
    db.set_module_roots(roots);

    let mut sources: Vec<_> = state.iter().collect();
    if reverse {
        sources.reverse();
    }
    for (relative, text) in sources {
        db.new_file(format!("{}/{}", manifest.name, relative), text.clone());
        db.new_file(relative.clone(), text.clone());
    }
    let key = format!("{}/{}", manifest.name, entry_rel);
    let entry = db
        .source_file_by_path(&key)
        .ok_or_else(|| format!("entry `{key}` is not registered"))?;
    observe(&db, entry)
}

fn observe(db: &DatabaseImpl, entry: SourceFile) -> Result<Observation, String> {
    let parsed = arandu_query::passes::parse(db, entry);
    if let Err(error) = &**parsed {
        return Ok(Observation {
            diagnostics: vec![format!("PARSE|{error}")],
            amir: None,
        });
    }
    let checked = arandu_query::passes::type_check(db, entry);
    let mut diagnostics = checked
        .diagnostics
        .iter()
        .map(diagnostic_key)
        .collect::<Vec<_>>();
    diagnostics.sort();
    let has_error = checked
        .diagnostics
        .iter()
        .any(|diag| matches!(diag.severity, Severity::Error));
    let amir = if has_error {
        None
    } else {
        let artifacts = arandu_query::passes::lower_amir(db, entry);
        Some(artifacts.amir.pretty_print(
            &artifacts.type_check.symbols,
            &artifacts.type_check.type_info.type_interner,
        ))
    };
    Ok(Observation { diagnostics, amir })
}

fn diagnostic_key(diag: &arandu_middle::Diagnostic) -> String {
    format!(
        "{}|{:?}|{}|{}..{}",
        diag.code, diag.severity, diag.message, diag.span.start, diag.span.end
    )
}

fn validate_expectation(
    case: &CorpusCase,
    observation: &Observation,
    revision: &str,
) -> Result<(), String> {
    let actual_codes: BTreeSet<_> = observation
        .diagnostics
        .iter()
        .filter_map(|diag| diag.split('|').next())
        .map(str::to_owned)
        .collect();
    match case.expectation {
        Expectation::Ok if !observation.diagnostics.is_empty() => {
            return Err(format!(
                "{} {revision}: expected ok, got {:?}",
                case.path, observation.diagnostics
            ))
        }
        Expectation::Error if observation.diagnostics.is_empty() => {
            return Err(format!("{} {revision}: expected diagnostics", case.path))
        }
        _ => {}
    }
    if !case.codes.is_subset(&actual_codes) {
        return Err(format!(
            "{} {revision}: expected codes {:?}, got {:?}",
            case.path, case.codes, actual_codes
        ));
    }
    Ok(())
}

fn revision_dirs(project_root: &Path) -> Result<Vec<PathBuf>, String> {
    let root = project_root.join("revisions");
    if !root.exists() {
        return Ok(Vec::new());
    }
    sorted_dirs(&root)
}

fn apply_revision(
    revision: &Path,
    crlf: bool,
    state: &mut BTreeMap<String, String>,
    session: &mut CorpusSession,
) -> Result<(), String> {
    let src = revision.join("src");
    for relative in scan_aru_entries(&src) {
        let normalized = relative.replace('\\', "/");
        let text = fs::read_to_string(src.join(&relative)).map_err(|error| {
            format!(
                "failed to read revision {}: {error}",
                src.join(&relative).display()
            )
        })?;
        let text = normalize_newlines(text, crlf);
        session.set_text(&normalized, &text)?;
        state.insert(normalized, text);
    }
    Ok(())
}

fn sorted_dirs(root: &Path) -> Result<Vec<PathBuf>, String> {
    let mut dirs = Vec::new();
    for entry in
        fs::read_dir(root).map_err(|error| format!("failed to list {}: {error}", root.display()))?
    {
        let path = entry.map_err(|error| error.to_string())?.path();
        if path.is_dir() {
            dirs.push(path);
        }
    }
    dirs.sort();
    Ok(dirs)
}

fn update_summary(summary: &mut CorpusSummary, state: &BTreeMap<String, String>) {
    summary.files += state.len();
    summary.modules += state.len();
    for text in state.values() {
        summary.lines += text.lines().count();
        summary.bytes += text.len();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_is_sorted_and_rejects_duplicates() {
        let parsed =
            parse_corpus_manifest("small/b|ok|multi-file|-|lf\nsmall/a|error|cycles|N006|crlf\n")
                .unwrap();
        assert_eq!(parsed[0].path, "small/a");
        assert!(parse_corpus_manifest("small/a|ok|x|-|lf\nsmall/a|ok|x|-|lf").is_err());
    }

    #[test]
    fn relative_paths_cannot_escape_corpus() {
        assert!(normalize_relative("../outside").is_err());
        assert!(normalize_relative("small/inside").is_ok());
    }
}
