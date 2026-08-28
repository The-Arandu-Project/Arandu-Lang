use std::fs;
use std::path::PathBuf;
use std::process::Command;

pub fn cli_command() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_arandu_cli"));
    command.env("ARANDU_RUNTIME_LIB", runtime_library());
    command
}

fn runtime_library() -> PathBuf {
    if let Some(explicit) = std::env::var_os("ARANDU_RUNTIME_LIB") {
        return PathBuf::from(explicit);
    }
    let deps = std::env::current_exe()
        .expect("resolve integration-test executable")
        .parent()
        .expect("integration-test executable must live in target profile/deps")
        .to_path_buf();
    let (prefix, extension) = if cfg!(windows) {
        ("arandu_runtime-", "lib")
    } else {
        ("libarandu_runtime-", "a")
    };
    let mut candidates = fs::read_dir(&deps)
        .expect("read Cargo dependency artifacts")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_stem()
                .and_then(|stem| stem.to_str())
                .is_some_and(|stem| stem.starts_with(prefix))
                && path.extension().and_then(|value| value.to_str()) == Some(extension)
        })
        .collect::<Vec<_>>();
    // A developer checkout can contain staticlibs from several feature sets
    // and source revisions. Cargo's hash is not chronological, so choosing the
    // lexicographically last filename can silently link a stale runtime.
    candidates.sort_by(|left, right| {
        let modified = |path: &PathBuf| {
            fs::metadata(path)
                .and_then(|metadata| metadata.modified())
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH)
        };
        modified(left)
            .cmp(&modified(right))
            .then_with(|| left.cmp(right))
    });
    candidates
        .pop()
        .expect("build arandu_runtime staticlib before running AOT integration tests")
}
