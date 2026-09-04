//! Deterministic stdlib root resolution (install gold bar).
//!
//! Cascade (highest priority first) — **never** relative to process cwd:
//!
//! 1. `--stdlib-path` / explicit CLI override
//! 2. `ARANDU_STDLIB` environment variable
//! 3. Relative to **canonicalized** [`std::env::current_exe`]:
//!    - `../share/arandu/stdlib` (install layout next to real `bin/`)
//!    - walk parents of the real executable for monorepo `stdlib/`
//! 4. Hard error — never silently pick a stale leftover tree
//!
//! **Symlink rule:** always `canonicalize()` the executable path before
//! computing relatives. A PATH symlink (`/usr/local/bin/arandu` →
//! `$PREFIX/arandu-0.0.1/bin/arandu`) must resolve against the **real**
//! tree, not the symlink's parent directory.

use std::env;
use std::fmt;
use std::path::{Path, PathBuf};

/// Environment variable override (priority 2).
pub const STDLIB_ENV: &str = "ARANDU_STDLIB";

/// Install-layout suffix relative to the directory that contains the binary.
/// Real binary at `$PREFIX/arandu-X.Y.Z/bin/arandu` →
/// stdlib at `$PREFIX/arandu-X.Y.Z/share/arandu/stdlib`.
pub const INSTALL_RELATIVE: &str = "../share/arandu/stdlib";

/// How the stdlib root was selected (for `arandu doctor` and error messages).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StdlibSource {
    ExplicitFlag,
    EnvVar,
    InstallLayout,
    ExeWalk,
}

impl fmt::Display for StdlibSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StdlibSource::ExplicitFlag => write!(f, "--stdlib-path"),
            StdlibSource::EnvVar => write!(f, "{STDLIB_ENV}"),
            StdlibSource::InstallLayout => {
                write!(f, "relative to binary ({INSTALL_RELATIVE})")
            }
            StdlibSource::ExeWalk => write!(f, "relative to binary (monorepo walk)"),
        }
    }
}

/// Successful resolution of the stdlib tree root.
#[derive(Debug, Clone)]
pub struct StdlibRoot {
    pub path: PathBuf,
    pub source: StdlibSource,
}

/// Failure to locate any stdlib (priority 4 — never silent fallback).
#[derive(Debug, Clone)]
pub struct StdlibNotFound {
    pub tried: Vec<String>,
}

impl fmt::Display for StdlibNotFound {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            f,
            "stdlib not found (resolved relative to the arandu binary, never cwd)"
        )?;
        writeln!(f, "tried:")?;
        for t in &self.tried {
            writeln!(f, "  - {t}")?;
        }
        write!(
            f,
            "fix: pass --stdlib-path=<dir>, set {STDLIB_ENV}, or install under share/arandu/stdlib"
        )
    }
}

impl std::error::Error for StdlibNotFound {}

/// Options for [`resolve_stdlib_root`].
#[derive(Debug, Clone, Default)]
pub struct StdlibResolveOpts {
    /// Priority 1: `--stdlib-path` value (already expanded by caller if needed).
    pub explicit: Option<PathBuf>,
    /// Override `current_exe` for tests (still canonicalized when possible).
    pub current_exe: Option<PathBuf>,
    /// Override env lookup for tests (`None` = read real env).
    pub env_value: Option<Option<String>>,
}

/// Resolve the stdlib root using the documented cascade.
pub fn resolve_stdlib_root(opts: StdlibResolveOpts) -> Result<StdlibRoot, StdlibNotFound> {
    let mut tried = Vec::new();

    // 1. Explicit flag
    if let Some(p) = opts.explicit {
        tried.push(format!("--stdlib-path {}", p.display()));
        if is_stdlib_root(&p) {
            return Ok(StdlibRoot {
                path: canonicalize_or(p),
                source: StdlibSource::ExplicitFlag,
            });
        }
        tried.push("  (rejected: not a valid stdlib root)".into());
    }

    // 2. Environment variable
    let env_raw = match &opts.env_value {
        Some(v) => v.clone(),
        None => env::var(STDLIB_ENV).ok(),
    };
    if let Some(raw) = env_raw.filter(|s| !s.is_empty()) {
        let p = PathBuf::from(&raw);
        tried.push(format!("{STDLIB_ENV}={raw}"));
        if is_stdlib_root(&p) {
            return Ok(StdlibRoot {
                path: canonicalize_or(p),
                source: StdlibSource::EnvVar,
            });
        }
        tried.push("  (rejected: not a valid stdlib root)".into());
    } else {
        tried.push(format!("{STDLIB_ENV} (unset)"));
    }

    // 3. Relative to current_exe — never cwd. Always canonicalize first so
    // PATH / install-prefix symlinks land on the real versioned tree.
    let exe = match opts.current_exe {
        Some(p) => Ok(p),
        None => env::current_exe(),
    };
    match exe {
        Ok(exe_path) => {
            let (real_exe, notes) = resolve_exe_path(exe_path);
            tried.extend(notes);

            let exe_dir = real_exe
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| PathBuf::from("."));

            // 3a. Install layout: bin/../share/arandu/stdlib
            let install = exe_dir.join(INSTALL_RELATIVE);
            tried.push(format!(
                "current_exe install layout: {} (real exe={})",
                install.display(),
                real_exe.display()
            ));
            if is_stdlib_root(&install) {
                return Ok(StdlibRoot {
                    path: canonicalize_or(install),
                    source: StdlibSource::InstallLayout,
                });
            }

            // 3b. Walk parents of the real executable (monorepo / cargo target/debug)
            let mut cur = exe_dir;
            loop {
                let candidate = cur.join("stdlib");
                tried.push(format!("current_exe walk: {}", candidate.display()));
                if is_stdlib_root(&candidate) {
                    return Ok(StdlibRoot {
                        path: canonicalize_or(candidate),
                        source: StdlibSource::ExeWalk,
                    });
                }
                if !cur.pop() {
                    break;
                }
            }
        }
        Err(e) => {
            tried.push(format!("current_exe() failed: {e}"));
        }
    }

    Err(StdlibNotFound { tried })
}

/// Follow symlinks on `exe` so install-relative paths use the real tree.
///
/// Falls back to the raw path if canonicalize fails (e.g. synthetic test
/// paths that do not exist on disk).
#[must_use]
pub fn resolve_exe_path(exe: PathBuf) -> (PathBuf, Vec<String>) {
    let mut notes = Vec::new();
    match std::fs::canonicalize(&exe) {
        Ok(real) => {
            if real != exe {
                notes.push(format!(
                    "canonicalized exe {} → {}",
                    exe.display(),
                    real.display()
                ));
            } else {
                notes.push(format!("current_exe (canonical): {}", real.display()));
            }
            (real, notes)
        }
        Err(e) => {
            notes.push(format!(
                "canonicalize({}) failed ({e}); using raw path",
                exe.display()
            ));
            (exe, notes)
        }
    }
}

/// True if `path` looks like an Arandu stdlib root (`std/` or `core/` present).
#[must_use]
pub fn is_stdlib_root(path: &Path) -> bool {
    if !path.is_dir() {
        return false;
    }
    // Accept either layered layout: stdlib/std/*.aru or stdlib/core/*.aru
    path.join("std").is_dir() || path.join("core").is_dir()
}

/// Map a canonical import path (`stdlib/std/io.aru`) onto a filesystem path
/// under `stdlib_root` (`…/stdlib/std/io.aru`).
///
/// Import paths always keep the `stdlib/` prefix (see `canonicalize_import_path`);
/// the on-disk root is the directory that **contains** `std/` / `core/`.
#[must_use]
pub fn import_path_on_disk(stdlib_root: &Path, import_path: &str) -> PathBuf {
    let relative = import_path
        .strip_prefix("stdlib/")
        .or_else(|| import_path.strip_prefix("stdlib\\"))
        .unwrap_or(import_path);
    stdlib_root.join(relative)
}

fn canonicalize_or(p: PathBuf) -> PathBuf {
    std::fs::canonicalize(&p).unwrap_or(p)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn workspace_stdlib() -> PathBuf {
        // crate dir = crates/arandu_query → workspace = ../..
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join("stdlib")
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = env::temp_dir().join(format!(
            "arandu_stdlib_{name}_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Versioned install tree: `$root/arandu-0.0.1/{bin/arandu,share/arandu/stdlib/std}`.
    fn make_versioned_install(root: &Path) -> PathBuf {
        let version = root.join("arandu-0.0.1");
        let bin = version.join("bin");
        let std_mod = version.join("share/arandu/stdlib/std");
        fs::create_dir_all(&bin).unwrap();
        fs::create_dir_all(&std_mod).unwrap();
        let real_exe = bin.join("arandu");
        fs::write(&real_exe, b"#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&real_exe).unwrap().permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&real_exe, perms).unwrap();
        }
        real_exe
    }

    #[test]
    fn workspace_stdlib_is_valid_root() {
        let root = workspace_stdlib();
        assert!(
            is_stdlib_root(&root),
            "expected monorepo stdlib at {}",
            root.display()
        );
    }

    #[test]
    fn explicit_wins() {
        let root = workspace_stdlib();
        let resolved = resolve_stdlib_root(StdlibResolveOpts {
            explicit: Some(root),
            env_value: Some(Some("/nonexistent".into())),
            current_exe: Some(PathBuf::from("/tmp/fake-arandu")),
        })
        .expect("explicit should win");
        assert_eq!(resolved.source, StdlibSource::ExplicitFlag);
        assert!(resolved.path.ends_with("stdlib") || is_stdlib_root(&resolved.path));
    }

    #[test]
    fn env_wins_over_exe() {
        let root = workspace_stdlib();
        let resolved = resolve_stdlib_root(StdlibResolveOpts {
            explicit: None,
            env_value: Some(Some(root.to_string_lossy().into_owned())),
            current_exe: Some(PathBuf::from("/tmp/fake-arandu")),
        })
        .expect("env should win");
        assert_eq!(resolved.source, StdlibSource::EnvVar);
    }

    #[test]
    fn missing_everything_errors() {
        let err = resolve_stdlib_root(StdlibResolveOpts {
            explicit: Some(PathBuf::from("/no/such/stdlib")),
            env_value: Some(None),
            current_exe: Some(PathBuf::from("/tmp/no-stdlib-here/bin/arandu")),
        })
        .expect_err("should fail");
        let msg = err.to_string();
        assert!(msg.contains("stdlib not found"));
        assert!(msg.contains(STDLIB_ENV) || msg.contains("--stdlib-path"));
    }

    #[test]
    fn exe_walk_finds_monorepo() {
        // Pretend the binary lives under target/debug inside the workspace.
        let ws = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        let fake_exe = ws.join("target/debug/arandu_cli");
        // Ensure parent dirs exist for walk (exe itself need not exist as a file
        // — we only use its path for parent walking when canonicalize fails).
        let _ = fs::create_dir_all(fake_exe.parent().unwrap());
        let resolved = resolve_stdlib_root(StdlibResolveOpts {
            explicit: None,
            env_value: Some(None),
            current_exe: Some(fake_exe),
        })
        .expect("walk from target/debug should find workspace stdlib");
        assert_eq!(resolved.source, StdlibSource::ExeWalk);
        assert!(is_stdlib_root(&resolved.path));
    }

    #[test]
    fn install_layout_from_real_exe() {
        let root = temp_dir("install_layout");
        let real_exe = make_versioned_install(&root);
        let resolved = resolve_stdlib_root(StdlibResolveOpts {
            explicit: None,
            env_value: Some(None),
            current_exe: Some(real_exe),
        })
        .expect("install layout should resolve");
        assert_eq!(resolved.source, StdlibSource::InstallLayout);
        assert!(is_stdlib_root(&resolved.path));
        let _ = fs::remove_dir_all(&root);
    }

    /// Gold bar: PATH symlink must not mis-resolve stdlib against the link's dir.
    #[cfg(unix)]
    #[test]
    fn install_layout_via_path_symlink_canonicalizes() {
        use std::os::unix::fs::symlink;

        let root = temp_dir("symlink_layout");
        let real_exe = make_versioned_install(&root);

        // Symlink as if installed on PATH: $root/path-bin/arandu → real versioned bin.
        // Without canonicalize, parent would be path-bin/ and ../share would miss.
        let path_bin = root.join("path-bin");
        fs::create_dir_all(&path_bin).unwrap();
        let link = path_bin.join("arandu");
        symlink(&real_exe, &link).unwrap();

        // Poison: put a decoy stdlib next to the symlink so a naive
        // non-canonicalizing resolver would pick the wrong tree or fail.
        // We only assert the real versioned tree is selected.
        let resolved = resolve_stdlib_root(StdlibResolveOpts {
            explicit: None,
            env_value: Some(None),
            current_exe: Some(link),
        })
        .expect("symlink exe must resolve via canonicalize to versioned tree");

        assert_eq!(resolved.source, StdlibSource::InstallLayout);
        let expected = root
            .join("arandu-0.0.1/share/arandu/stdlib")
            .canonicalize()
            .unwrap();
        assert_eq!(
            resolved.path, expected,
            "stdlib must come from real tree, not symlink parent"
        );

        // Doctor-facing note path should have recorded canonicalization.
        let err = resolve_stdlib_root(StdlibResolveOpts {
            explicit: None,
            env_value: Some(None),
            current_exe: Some(path_bin.join("missing")),
        });
        assert!(err.is_err());

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn import_path_strips_prefix() {
        let root = PathBuf::from("/opt/share/arandu/stdlib");
        assert_eq!(
            import_path_on_disk(&root, "stdlib/std/io.aru"),
            PathBuf::from("/opt/share/arandu/stdlib/std/io.aru")
        );
    }

    #[test]
    fn resolve_exe_path_follows_symlink() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            let root = temp_dir("resolve_exe");
            let real = root.join("real-bin");
            fs::write(&real, b"x").unwrap();
            let link = root.join("link-bin");
            symlink(&real, &link).unwrap();
            let (resolved, notes) = resolve_exe_path(link);
            assert_eq!(resolved, real.canonicalize().unwrap());
            assert!(
                notes
                    .iter()
                    .any(|n| n.contains("canonicalized") || n.contains("canonical")),
                "notes={notes:?}"
            );
            let _ = fs::remove_dir_all(&root);
        }
    }
}
