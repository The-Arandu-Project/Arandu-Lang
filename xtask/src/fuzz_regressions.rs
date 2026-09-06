use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use arandu_fuzz_support::{Target, MAX_INPUT_BYTES};

const SEED_TIMEOUT: Duration = Duration::from_secs(2);

struct Entry {
    path: PathBuf,
    targets: Vec<Target>,
    origin: String,
    risk: String,
}

pub fn check(root: &Path) -> i32 {
    let corpus = root.join("tests/fuzz-regressions");
    let entries = match load_manifest(&corpus) {
        Ok(entries) => entries,
        Err(error) => {
            eprintln!("check-fuzz-regressions: {error}");
            return 1;
        }
    };
    let executable = match env::current_exe() {
        Ok(path) => path,
        Err(error) => {
            eprintln!("check-fuzz-regressions: cannot locate xtask: {error}");
            return 1;
        }
    };

    for entry in &entries {
        for target in &entry.targets {
            let mut child = match Command::new(&executable)
                .arg("run-fuzz-seed")
                .arg(target.name())
                .arg(&entry.path)
                .stdin(Stdio::null())
                .spawn()
            {
                Ok(child) => child,
                Err(error) => {
                    eprintln!(
                        "cannot start {} for {}: {error}",
                        target.name(),
                        entry.path.display()
                    );
                    return 1;
                }
            };
            let started = Instant::now();
            loop {
                match child.try_wait() {
                    Ok(Some(status)) if status.success() => break,
                    Ok(Some(status)) => {
                        eprintln!(
                            "seed failed: {} target={} origin={} risk={} ({status})",
                            entry.path.display(),
                            target.name(),
                            entry.origin,
                            entry.risk
                        );
                        return 1;
                    }
                    Ok(None) if started.elapsed() < SEED_TIMEOUT => {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Ok(None) => {
                        let _ = child.kill();
                        let _ = child.wait();
                        eprintln!(
                            "seed timed out after {:?}: {} target={} origin={} risk={}",
                            SEED_TIMEOUT,
                            entry.path.display(),
                            target.name(),
                            entry.origin,
                            entry.risk
                        );
                        return 1;
                    }
                    Err(error) => {
                        let _ = child.kill();
                        eprintln!("cannot wait for seed worker: {error}");
                        return 1;
                    }
                }
            }
        }
    }
    let executions: usize = entries.iter().map(|entry| entry.targets.len()).sum();
    println!(
        "check-fuzz-regressions: ok ({} seeds, {executions} isolated executions, max={} bytes, timeout={:?})",
        entries.len(), MAX_INPUT_BYTES, SEED_TIMEOUT
    );
    0
}

pub fn run_one(mut args: impl Iterator<Item = String>) -> i32 {
    let Some(target) = args.next().and_then(|name| Target::parse(&name)) else {
        eprintln!("run-fuzz-seed: invalid target");
        return 2;
    };
    let Some(path) = args.next() else {
        eprintln!("run-fuzz-seed: missing seed path");
        return 2;
    };
    let data = match decode_seed(Path::new(&path)) {
        Ok(data) => data,
        Err(error) => {
            eprintln!("run-fuzz-seed: {error}");
            return 2;
        }
    };
    match std::panic::catch_unwind(|| arandu_fuzz_support::run(target, &data)) {
        Ok(()) => 0,
        Err(_) => {
            eprintln!(
                "run-fuzz-seed: target {} panicked for {path}",
                target.name()
            );
            1
        }
    }
}

fn load_manifest(corpus: &Path) -> Result<Vec<Entry>, String> {
    let manifest_path = corpus.join("manifest.tsv");
    let text = fs::read_to_string(&manifest_path)
        .map_err(|error| format!("cannot read {}: {error}", manifest_path.display()))?;
    let mut entries = Vec::new();
    let mut paths = BTreeSet::new();
    let mut contents: BTreeMap<Vec<u8>, PathBuf> = BTreeMap::new();
    let canonical_corpus = corpus
        .canonicalize()
        .map_err(|error| format!("cannot resolve {}: {error}", corpus.display()))?;

    for (line_index, line) in text.lines().enumerate() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let fields: Vec<_> = line.split('\t').collect();
        if fields.len() != 4 || fields.iter().any(|field| field.trim().is_empty()) {
            return Err(format!(
                "manifest line {} must have 4 non-empty TSV fields",
                line_index + 1
            ));
        }
        let relative = Path::new(fields[0]);
        if relative.is_absolute() {
            return Err(format!(
                "absolute seed path on manifest line {}",
                line_index + 1
            ));
        }
        let path = corpus.join(relative);
        let canonical = path
            .canonicalize()
            .map_err(|error| format!("cannot resolve {}: {error}", path.display()))?;
        if !canonical.starts_with(&canonical_corpus) {
            return Err(format!("seed escapes corpus: {}", path.display()));
        }
        if !paths.insert(canonical.clone()) {
            return Err(format!("duplicate seed path: {}", path.display()));
        }
        let data = decode_seed(&canonical)?;
        if data.len() > MAX_INPUT_BYTES {
            return Err(format!(
                "seed exceeds {MAX_INPUT_BYTES} bytes: {}",
                path.display()
            ));
        }
        if let Some(first) = contents.insert(data, canonical.clone()) {
            return Err(format!(
                "duplicate seed content: {} and {}",
                first.display(),
                path.display()
            ));
        }
        let targets = fields[1]
            .split(',')
            .map(|name| {
                Target::parse(name)
                    .ok_or_else(|| format!("unknown target {name:?} on line {}", line_index + 1))
            })
            .collect::<Result<Vec<_>, _>>()?;
        entries.push(Entry {
            path: canonical,
            targets,
            origin: fields[2].into(),
            risk: fields[3].into(),
        });
    }
    if entries.is_empty() {
        return Err("manifest has no seeds".into());
    }
    Ok(entries)
}

fn decode_seed(path: &Path) -> Result<Vec<u8>, String> {
    let bytes =
        fs::read(path).map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    decode_seed_bytes(&bytes, &path.display().to_string())
}

fn decode_seed_bytes(bytes: &[u8], source: &str) -> Result<Vec<u8>, String> {
    let Some(newline) = bytes.iter().position(|byte| *byte == b'\n') else {
        return Err(format!("missing encoding header in {source}"));
    };
    let header = bytes[..newline]
        .strip_suffix(b"\r")
        .unwrap_or(&bytes[..newline]);
    let payload = &bytes[newline + 1..];
    if header == b"encoding=hex" {
        let hex = payload;
        let compact: Vec<u8> = hex
            .iter()
            .copied()
            .filter(|byte| !byte.is_ascii_whitespace())
            .collect();
        if !compact.len().is_multiple_of(2) {
            return Err(format!("odd hex length in {source}"));
        }
        let (chunks, _) = compact.as_chunks::<2>();
        return chunks
            .iter()
            .map(|pair| {
                let text =
                    std::str::from_utf8(pair).map_err(|_| format!("non-ASCII hex in {source}"))?;
                u8::from_str_radix(text, 16).map_err(|_| format!("invalid hex in {source}"))
            })
            .collect();
    }
    if header == b"encoding=utf8" {
        return Ok(payload.to_vec());
    }
    Err(format!("missing encoding header in {source}"))
}

#[cfg(test)]
mod tests {
    use super::decode_seed_bytes;

    #[test]
    fn seed_headers_accept_lf_and_crlf_checkouts() {
        assert_eq!(
            decode_seed_bytes(b"encoding=hex\nf09f92\n", "lf.seed"),
            Ok(vec![0xf0, 0x9f, 0x92])
        );
        assert_eq!(
            decode_seed_bytes(b"encoding=hex\r\nf09f92\r\n", "crlf.seed"),
            Ok(vec![0xf0, 0x9f, 0x92])
        );
        assert_eq!(
            decode_seed_bytes(b"encoding=utf8\r\nfunc main() {}\r\n", "utf8.seed"),
            Ok(b"func main() {}\r\n".to_vec())
        );
    }
}
