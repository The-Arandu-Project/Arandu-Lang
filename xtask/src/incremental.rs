//! RFC 0011 incremental AOT benchmark and determinism oracle.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Instant, SystemTime};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

pub fn run(root: &Path, args: impl Iterator<Item = String>) -> i32 {
    for argument in args {
        if argument != "--verify-determinism" {
            eprintln!("error: unknown bench-incremental option: {argument}");
            return 2;
        }
    }
    match execute(root) {
        Ok(deterministic) => i32::from(!deterministic),
        Err(error) => {
            eprintln!("bench-incremental: {error}");
            1
        }
    }
}

fn execute(root: &Path) -> Result<bool, String> {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let build = Command::new(&cargo)
        .current_dir(root)
        .args(["build", "--locked", "-p", "arandu_cli"])
        .output()
        .map_err(|error| format!("cannot build benchmark compiler: {error}"))?;
    ensure_success("build benchmark compiler", &build)?;
    let runtime_build = Command::new(&cargo)
        .current_dir(root)
        .args([
            "rustc",
            "--locked",
            "-p",
            "arandu_runtime",
            "--crate-type",
            "staticlib",
        ])
        .output()
        .map_err(|error| format!("cannot build benchmark runtime: {error}"))?;
    ensure_success("build benchmark runtime", &runtime_build)?;

    let cli = root.join("target/debug").join(if cfg!(windows) {
        "arandu_cli.exe"
    } else {
        "arandu_cli"
    });
    if !cli.is_file() {
        return Err(format!(
            "compiler executable not found at {}",
            cli.display()
        ));
    }
    let runtime = root.join("target/debug").join(if cfg!(windows) {
        "arandu_runtime.lib"
    } else {
        "libarandu_runtime.a"
    });
    if !runtime.is_file() {
        return Err(format!("AOT runtime not found at {}", runtime.display()));
    }

    let work = unique_work_dir(&root.join("target/bench-incremental"))?;
    let project = work.join("workload");
    run_cli(
        &cli,
        &runtime,
        &work,
        &["new", "workload", "--bin", "--vcs=none"],
        None,
    )?;
    let source = project.join("src/main.aru");
    let initial = "/// baseline\nfunc helper(): int { return 1; }\nfunc main(): int { return helper() + 40; }\n";
    fs::write(&source, initial).map_err(|error| {
        format!(
            "cannot write benchmark source {}: {error}",
            source.display()
        )
    })?;
    run_cli(&cli, &runtime, &project, &["build", "-v"], None)?;

    let mut rows = Vec::with_capacity(5);
    fs::write(&source, initial).map_err(|error| format!("cannot touch source: {error}"))?;
    rows.push(measure(&cli, &runtime, &project, "noop-touch")?);

    let docs = initial.replace("/// baseline", "/// documentation changed");
    fs::write(&source, &docs).map_err(|error| format!("cannot edit docs: {error}"))?;
    rows.push(measure(&cli, &runtime, &project, "documentation")?);

    let body = docs.replace("return 1", "return 2");
    fs::write(&source, &body).map_err(|error| format!("cannot edit function body: {error}"))?;
    rows.push(measure(&cli, &runtime, &project, "private-body")?);

    let signature = "/// documentation changed\nfunc helper(value: int): int { return value + 2; }\nfunc main(): int { return helper(40); }\n";
    fs::write(&source, signature)
        .map_err(|error| format!("cannot edit public signature: {error}"))?;
    rows.push(measure(&cli, &runtime, &project, "public-signature")?);

    let module = project.join("src/extra.aru");
    fs::write(&module, "public func extra(): int { return 7; }\n")
        .map_err(|error| format!("cannot add module {}: {error}", module.display()))?;
    rows.push(measure(&cli, &runtime, &project, "new-module")?);

    let determinism = verify_determinism(&cli, &runtime, &work)?;
    let report_dir = root.join("target/benchmarks");
    fs::create_dir_all(&report_dir)
        .map_err(|error| format!("cannot create report directory: {error}"))?;
    let report = json!({
        "schema": 2,
        "workload": "rfc-0011-five-edit-classes",
        "measurements": rows,
        "determinism": determinism,
    });
    let mut json_bytes = serde_json::to_vec_pretty(&report)
        .map_err(|error| format!("cannot serialize JSON report: {error}"))?;
    json_bytes.push(b'\n');
    fs::write(report_dir.join("incremental.json"), json_bytes)
        .map_err(|error| format!("cannot write JSON report: {error}"))?;
    fs::write(report_dir.join("incremental.md"), markdown_report(&report))
        .map_err(|error| format!("cannot write Markdown report: {error}"))?;

    println!(
        "bench-incremental: reports written to {}",
        report_dir.display()
    );
    println!(
        "bench-incremental: deterministic across paths and RAYON_NUM_THREADS=1/16: {}",
        determinism["equal"].as_bool().unwrap_or(false)
    );
    let deterministic = determinism["equal"].as_bool().unwrap_or(false);
    fs::remove_dir_all(&work)
        .map_err(|error| format!("cannot remove benchmark work directory: {error}"))?;
    Ok(deterministic)
}

fn measure(cli: &Path, runtime: &Path, project: &Path, mutation: &str) -> Result<Value, String> {
    let started = Instant::now();
    let output = run_cli(
        cli,
        runtime,
        project,
        &["build", "-v", "-Ztime-passes"],
        None,
    )?;
    let elapsed_ms = started.elapsed().as_secs_f64() * 1_000.0;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let (cgu_cached, cgu_recompiled) = parse_cgu(&stderr).unwrap_or((0, 0));
    let phase_ms = parse_phase_timings(&stderr);
    for required in ["project-load", "incremental-cutoff"] {
        if !phase_ms.contains_key(required) {
            return Err(format!(
                "{mutation} measurement did not emit required `{required}` phase timing"
            ));
        }
    }
    let artifact = current_artifact(project)?;
    Ok(json!({
        "mutation": mutation,
        "wall_ms": elapsed_ms,
        "queries_reexecuted": parse_queries(&stderr).unwrap_or(0),
        "cgu_cached": cgu_cached,
        "cgu_recompiled": cgu_recompiled,
        "linker": parse_backend(&stdout).unwrap_or("unknown"),
        "phase_ms": phase_ms,
        "sha256": sha256_file(&artifact)?,
    }))
}

fn verify_determinism(cli: &Path, runtime: &Path, work: &Path) -> Result<Value, String> {
    let first_parent = work.join("determinism-a");
    let second_parent = work.join("a-much-longer-determinism-location");
    fs::create_dir_all(&first_parent).map_err(|error| error.to_string())?;
    fs::create_dir_all(&second_parent).map_err(|error| error.to_string())?;
    for parent in [&first_parent, &second_parent] {
        run_cli(
            cli,
            runtime,
            parent,
            &["new", "deterministic", "--bin", "--vcs=none"],
            None,
        )?;
        fs::write(
            parent.join("deterministic/src/main.aru"),
            "func helper(): int { return 7; }\nfunc main(): int { return helper(); }\n",
        )
        .map_err(|error| error.to_string())?;
    }
    let first = first_parent.join("deterministic");
    let second = second_parent.join("deterministic");
    run_cli(cli, runtime, &first, &["build", "-v"], Some("1"))?;
    run_cli(cli, runtime, &second, &["build", "-v"], Some("16"))?;
    let first_digest = sha256_file(&current_artifact(&first)?)?;
    let second_digest = sha256_file(&current_artifact(&second)?)?;
    let equal = first_digest == second_digest;
    Ok(json!({
        "threads_1_sha256": first_digest,
        "threads_16_sha256": second_digest,
        "equal": equal,
    }))
}

fn run_cli(
    cli: &Path,
    runtime: &Path,
    directory: &Path,
    args: &[&str],
    rayon_threads: Option<&str>,
) -> Result<Output, String> {
    let mut command = Command::new(cli);
    command
        .current_dir(directory)
        .env("ARANDU_RUNTIME_LIB", runtime)
        .args(args);
    if let Some(threads) = rayon_threads {
        command.env("RAYON_NUM_THREADS", threads);
    }
    let output = command
        .output()
        .map_err(|error| format!("cannot run {}: {error}", cli.display()))?;
    ensure_success(&format!("{} {}", cli.display(), args.join(" ")), &output)?;
    Ok(output)
}

fn ensure_success(operation: &str, output: &Output) -> Result<(), String> {
    if output.status.success() {
        return Ok(());
    }
    Err(format!(
        "{operation} failed with {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    ))
}

fn current_artifact(project: &Path) -> Result<PathBuf, String> {
    let state_path = find_named(&project.join("target/dev"), OsStr::new("build-state.json"))?
        .ok_or_else(|| "build-state.json not found".to_owned())?;
    let state: Value = serde_json::from_slice(
        &fs::read(&state_path).map_err(|error| format!("cannot read build state: {error}"))?,
    )
    .map_err(|error| format!("cannot parse build state: {error}"))?;
    let relative = state["artifact"]
        .as_str()
        .ok_or_else(|| "build state has no artifact path".to_owned())?;
    Ok(state_path.parent().unwrap_or(project).join(relative))
}

fn find_named(root: &Path, name: &OsStr) -> Result<Option<PathBuf>, String> {
    if !root.is_dir() {
        return Ok(None);
    }
    let mut pending = vec![root.to_path_buf()];
    let mut found = None;
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory)
            .map_err(|error| format!("cannot scan {}: {error}", directory.display()))?
        {
            let path = entry.map_err(|error| error.to_string())?.path();
            if path.is_dir() {
                pending.push(path);
            } else if path.file_name() == Some(name) {
                if found.is_some() {
                    return Err(format!("multiple {} files found", name.to_string_lossy()));
                }
                found = Some(path);
            }
        }
    }
    Ok(found)
}

fn sha256_file(path: &Path) -> Result<String, String> {
    let bytes = fs::read(path)
        .map_err(|error| format!("cannot hash artifact {}: {error}", path.display()))?;
    use std::fmt::Write as _;
    let digest = Sha256::digest(bytes);
    let mut encoded = String::with_capacity(digest.len() * 2);
    for byte in digest {
        write!(&mut encoded, "{byte:02x}").map_err(|error| error.to_string())?;
    }
    Ok(encoded)
}

fn parse_queries(stderr: &str) -> Option<u64> {
    let line = stderr.lines().find(|line| line.starts_with("[rebuilt: "))?;
    line.strip_prefix("[rebuilt: ")?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

fn parse_cgu(stderr: &str) -> Option<(u64, u64)> {
    let line = stderr.lines().find(|line| line.starts_with("[cgu] "))?;
    let (_, stats) = line.split_once("units: ")?;
    let mut parts = stats.split(',');
    let cached = parts.next()?.split_whitespace().next()?.parse().ok()?;
    let recompiled = parts.next()?.split_whitespace().next()?.parse().ok()?;
    Some((cached, recompiled))
}

fn parse_backend(stdout: &str) -> Option<&str> {
    let (_, rest) = stdout.split_once("backend=")?;
    rest.split([',', ')']).next()
}

fn parse_phase_timings(stderr: &str) -> BTreeMap<String, f64> {
    let mut timings = BTreeMap::new();
    for line in stderr.lines() {
        let Some((_, payload)) = line.split_once("[arandu][perf]") else {
            continue;
        };
        let mut fields = payload.split_whitespace();
        let (Some(phase), Some(duration)) = (fields.next(), fields.next()) else {
            continue;
        };
        if fields.next().is_some() {
            continue;
        }
        let Some(milliseconds) = duration.strip_suffix("ms") else {
            continue;
        };
        let Ok(milliseconds) = milliseconds.parse::<f64>() else {
            continue;
        };
        if !milliseconds.is_finite() || milliseconds < 0.0 {
            continue;
        }
        *timings.entry(phase.to_owned()).or_insert(0.0) += milliseconds;
    }
    timings
}

fn markdown_report(report: &Value) -> String {
    let mut output = String::from(
        "# RFC 0011 incremental benchmark\n\n| Mutation | Wall ms | Queries | CGU cached | CGU rebuilt | Linker | SHA-256 |\n| --- | ---: | ---: | ---: | ---: | --- | --- |\n",
    );
    if let Some(rows) = report["measurements"].as_array() {
        for row in rows {
            output.push_str(&format!(
                "| {} | {:.3} | {} | {} | {} | {} | `{}` |\n",
                row["mutation"].as_str().unwrap_or("?"),
                row["wall_ms"].as_f64().unwrap_or(0.0),
                row["queries_reexecuted"].as_u64().unwrap_or(0),
                row["cgu_cached"].as_u64().unwrap_or(0),
                row["cgu_recompiled"].as_u64().unwrap_or(0),
                row["linker"].as_str().unwrap_or("?"),
                row["sha256"].as_str().unwrap_or("?"),
            ));
        }
    }
    output.push_str(&format!(
        "\nDeterministic across different paths and `RAYON_NUM_THREADS=1/16`: **{}**.\n",
        report["determinism"]["equal"].as_bool().unwrap_or(false)
    ));
    output.push_str("\n## Phase timings\n\n| Mutation | Phase | ms |\n| --- | --- | ---: |\n");
    if let Some(rows) = report["measurements"].as_array() {
        for row in rows {
            let mutation = row["mutation"].as_str().unwrap_or("?");
            if let Some(phases) = row["phase_ms"].as_object() {
                for (phase, milliseconds) in phases {
                    output.push_str(&format!(
                        "| {mutation} | {phase} | {:.3} |\n",
                        milliseconds.as_f64().unwrap_or(0.0),
                    ));
                }
            }
        }
    }
    output
}

fn unique_work_dir(parent: &Path) -> Result<PathBuf, String> {
    fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let nonce = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_err(|error| format!("system clock is before Unix epoch: {error}"))?
        .as_nanos();
    let directory = parent.join(format!("work-{}-{nonce}", std::process::id()));
    fs::create_dir(&directory).map_err(|error| {
        format!(
            "cannot reserve benchmark directory {}: {error}",
            directory.display()
        )
    })?;
    Ok(directory)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_accumulates_non_colored_phase_timings() {
        let stderr = "\
[12:34:56] [arandu][perf] project-load                        4.250ms\n\
unrelated output\n\
[12:34:56] [arandu][perf] lower-amir                         10.500ms\n\
[12:34:57] [arandu][perf] project-load                        0.750ms\n";

        let timings = parse_phase_timings(stderr);
        assert_eq!(timings.len(), 2);
        assert_eq!(timings.get("project-load"), Some(&5.0));
        assert_eq!(timings.get("lower-amir"), Some(&10.5));
    }

    #[test]
    fn ignores_malformed_or_non_finite_phase_timings() {
        let stderr = "\
[12:34:56] [arandu][perf] project-load nope\n\
[12:34:56] [arandu][perf] project-load NaNms\n\
[12:34:56] [arandu][perf] too many fields 1.000ms\n";

        assert!(parse_phase_timings(stderr).is_empty());
    }
}
