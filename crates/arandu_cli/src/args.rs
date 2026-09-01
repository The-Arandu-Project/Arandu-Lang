//! CLI argument parsing, flags, layout extraction, and usage documentation.

use arandu_middle::layout::DataLayout;

use crate::cli_error::CliFailure;
use crate::pipeline::{fail_usage, finish};
use crate::project::{self, ProjectFlags};

#[derive(Debug, Clone)]
pub struct CliInvocation {
    pub debug: bool,
    pub opt: bool,
    pub parallel: bool,
    pub genref_report: bool,
    pub args: Vec<String>,
    pub z_flags: Vec<String>,
    pub data_layout: DataLayout,
    pub project_flags: ProjectFlags,
}

pub fn parse_invocation(raw_args: impl IntoIterator<Item = String>) -> CliInvocation {
    let mut debug = false;
    let mut opt = false;
    let mut parallel = false;
    let mut genref_report = false;
    let mut args = Vec::new();
    let mut z_flags: Vec<String> = Vec::new();
    let mut layout_flags: Vec<String> = Vec::new();
    let mut raw_project_flags: Vec<String> = Vec::new();

    for arg in raw_args {
        match arg.as_str() {
            "--debug" => debug = true,
            "--opt" => opt = true,
            "--parallel" => parallel = true,
            "--genref-report" => genref_report = true,
            // G2: long form of -Zno-generational-fallback (same atomic).
            "--no-generational-fallback" => {
                z_flags.push("-Zno-generational-fallback".into());
            }
            s if s.starts_with("-Z") => z_flags.push(arg),
            s if s.starts_with("--layout=") => layout_flags.push(arg),
            // Collect project flags even before we know the subcommand.
            s if s.starts_with("--stdlib-path")
                || s.starts_with("--cache-dir")
                || s == "--release"
                || s == "-v"
                || s == "--verbose"
                || s == "--locked"
                || s == "--offline"
                || s == "--frozen"
                || s == "--accept" =>
            {
                raw_project_flags.push(arg);
            }
            _ => args.push(arg),
        }
    }
    let data_layout = parse_data_layout(&layout_flags);
    let (project_flags, extra_positional) = project::parse_project_flags(&raw_project_flags)
        .unwrap_or_else(|message| fail_usage(format!("error: {message}")));
    let _ = extra_positional;

    CliInvocation {
        debug,
        opt,
        parallel,
        genref_report,
        args,
        z_flags,
        data_layout,
        project_flags,
    }
}

pub fn parse_data_layout(flags: &[String]) -> DataLayout {
    for f in flags {
        if let Some(rest) = f.strip_prefix("--layout=") {
            return match rest {
                "host" => DataLayout::host(),
                "ptr4" | "32" => DataLayout::ptr_width(4),
                "i686" | "i686-sysv" => DataLayout::i686_sysv(),
                "ptr8" | "64" => DataLayout::ptr_width(8),
                other => {
                    fail_usage(format!(
                        "unknown --layout={other} (use host|ptr4|ptr8|i686)"
                    ));
                }
            };
        }
    }
    DataLayout::host()
}

pub fn parse_benchmark_seconds(value: Option<&String>, usage: &str) -> u64 {
    let seconds = value
        .and_then(|v| v.parse::<f64>().ok())
        .filter(|v| v.is_finite() && *v > 0.0 && *v <= 3600.0)
        .unwrap_or_else(|| fail_usage(usage));
    let nanos = std::time::Duration::from_secs_f64(seconds).as_nanos();
    u64::try_from(nanos).unwrap_or_else(|_| fail_usage(usage))
}

pub fn parse_benchmark_percentage(value: Option<&String>, usage: &str) -> f64 {
    value
        .and_then(|v| v.trim_end_matches('%').parse::<f64>().ok())
        .filter(|v| v.is_finite() && (0.0..=100.0).contains(v))
        .unwrap_or_else(|| fail_usage(usage))
}

pub fn usage_and_exit() -> ! {
    let message = concat!(
        "usage:\n",
        "  arandu_cli <lex|parse|check|hir|amir|run|emit-c|graph|fmt> <path> [flags]\n",
        "  arandu_cli new <project-name> [--bin|--lib] [--vcs=auto|git|none]\n",
        "  arandu_cli init [--bin|--lib] [--vcs=auto|git|none]\n",
        "  arandu_cli doctor [--stdlib-path=<dir>] [-v]\n",
        "  arandu_cli cache <dir|inspect|verify|verify-tree|prune> [--cache-dir=<absolute-dir>] [limits]\n",
        "  arandu_cli hash-file <path>          # BLAKE3 hex (packaging checksums)\n",
        "  arandu_cli watch [package-path]      # re-check on FS changes (package mode)\n",
        "  arandu_cli test [package-path] [--list|--exact <id>] [--format human|json|junit]\n",
        "  arandu_cli bench [package-path] [--list|--exact <id>] [--save-baseline <name>|--compare <name>]\n",
        "  arandu_cli clean [package-path]      # remove owned project artifacts\n",
        "  arandu_cli tree [package-path]       # canonical resolved dependency graph\n",
        "  arandu_cli audit [package-path]      # locked provenance and policy audit\n",
        "  arandu_cli vendor [package-path]     # verified offline source snapshot\n",
        "  arandu_cli update [package-path] --accept # review and publish remote graph\n",
        "  arandu_cli verify [package-path]     # locked, offline cache verification\n",
        "  arandu_cli check|run|build [--release] [--stdlib-path=<dir>] [package-path]\n\n",
        "  emit-c options: --layout=host|ptr4|ptr8|i686  (default: host)\n",
        "                  layout model only; cross compiler/sysroot are external\n",
        "  G2/F2.3: --no-generational-fallback  (promote O004 notes to errors)\n",
        "  G5: --genref-report  (per-module/function promotion and check counts on stderr)\n",
        "  -Z flags: -Ztime-passes  -Zprofile-queries  -Zprint-alloc-stats  -Zdump-mir\n",
        "           : -Zdebug-parser -Zdebug-typeck -Zdebug-ossa -Zdebug-layout -Zdebug-backend -Zdebug-all\n",
        "           : -Zself-profile=<path>  -Zexplain-rebuild  -Zno-generational-fallback\n\n",
        "  backend: build → Cranelift baseline; build --release → Cranelift speed + AMIR O2\n",
        "  stdlib:  --stdlib-path > ARANDU_STDLIB > relative to binary (never cwd)\n",
        "  cache:   --cache-dir > ARANDU_CACHE_DIR > platform-native user cache"
    );
    finish(Err(CliFailure::usage(message)))
}
