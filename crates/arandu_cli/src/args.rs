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
    /// Arguments following `--`, forwarded verbatim to an executed program.
    pub program_args: Vec<String>,
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
    let mut program_args = Vec::new();
    let mut z_flags: Vec<String> = Vec::new();
    let mut layout_flags: Vec<String> = Vec::new();
    let mut raw_project_flags: Vec<String> = Vec::new();

    let mut after_separator = false;
    for arg in raw_args {
        if after_separator {
            program_args.push(arg);
            continue;
        }
        if arg == "--" {
            after_separator = true;
            continue;
        }
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
        program_args,
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
        "The Arandu Programming Language Compiler\n\n",
        "usage:\n",
        "  arandu <command> [options] [package-path | file]\n",
        "  arandu_cli <command> [options] [package-path | file]\n\n",
        "Build & Execution Commands:\n",
        "  run        Compile and execute a package or file via Cranelift JIT\n",
        "  build      Compile package to native executable or library\n",
        "  check      Type-check and validate without code generation\n",
        "  test       Execute unit and integration test suites\n",
        "  bench      Run benchmarks and compare baseline metrics\n\n",
        "Project & Package Management:\n",
        "  new        Create a new Arandu project directory [--bin|--lib] [--vcs=auto|git|none]\n",
        "  init       Initialize an Arandu package in current directory [--bin|--lib]\n",
        "  watch      Watch filesystem and re-check package incrementally\n",
        "  clean      Remove project build artifacts and scratch cache\n",
        "  doc        Generate package documentation [--format=html|json|md] [--open]\n\n",
        "Dependencies & Supply-Chain:\n",
        "  tree       Display canonical resolved dependency graph\n",
        "  audit      Audit locked provenance and security policies\n",
        "  vendor     Create verified offline source snapshot\n",
        "  verify     Verify offline cache integrity against lockfile\n",
        "  update     Review and publish remote graph update (--accept)\n\n",
        "Plumbing & Inspection Commands:\n",
        "  lex        Dump concrete syntax tokens\n",
        "  parse      Dump concrete syntax tree (Rowan CST / AST)\n",
        "  hir        Dump High-Level Intermediate Representation\n",
        "  amir       Dump Arandu Mid-Level IR (SSA/OSSA)\n",
        "  graph      Emit module dependency graph in Graphviz DOT format\n",
        "  emit-c     Emit portable C source code\n",
        "  fmt        Format source files according to canonical style rules\n",
        "  doctor     Inspect compiler toolchain, environment, and stdlib paths\n",
        "  cache      Inspect, prune, and verify compiler cache <dir|inspect|verify|prune>\n",
        "  hash-file  Compute BLAKE3 checksum for packaging\n\n",
        "Target & Toolchain Options:\n",
        "  --release                  Build with speed optimizations (Cranelift + AMIR O2)\n",
        "  --stdlib-path <dir>        Override path to standard library\n",
        "  --cache-dir <dir>          Override compiler cache directory\n",
        "  --layout=host|ptr4|ptr8|i686  (default: host)\n",
        "                             layout model only; cross compiler/sysroot are external\n",
        "  --vcs=auto|git|none        VCS initialization mode for new projects\n",
        "  -v, --verbose              Enable detailed progress and timing logs\n",
        "  -V, --version              Print compiler version and exit\n",
        "  -h, --help                 Print this help message\n\n",
        "Generational Memory Safety (GenRef):\n",
        "  --no-generational-fallback Reject runtime generational promotion (promote O004 to error)\n",
        "  --genref-report            Print per-module/function promotion and check counts on stderr\n\n",
        "Developer & Unstable Debug Flags (-Z):\n",
        "  -Ztime-passes              Display execution timings for compilation passes\n",
        "  -Zprofile-queries          Profile Salsa incremental semantic query costs\n",
        "  -Zprint-alloc-stats        Print scratch arena allocation statistics\n",
        "  -Zdump-mir                 Dump intermediate AMIR between optimization passes\n",
        "  -Zdebug-parser             Trace Rowan CST parsing steps\n",
        "  -Zdebug-typeck             Trace bidirectional type inference & constraints\n",
        "  -Zdebug-ossa               Trace ownership SSA generation and joins\n",
        "  -Zdebug-layout             Trace memory layout computation\n",
        "  -Zdebug-backend            Trace backend machine code generation\n",
        "  -Zdebug-all                Enable all compiler debug traces\n",
        "  -Zself-profile=<path>      Record detailed execution profile\n",
        "  -Zexplain-rebuild          Explain reason for Salsa incremental rebuild\n",
        "  -Zno-generational-fallback Synonym for --no-generational-fallback\n\n",
        "Environment & Defaults:\n",
        "  backend: build → Cranelift baseline; build --release → Cranelift speed + AMIR O2\n",
        "  stdlib:  --stdlib-path > ARANDU_STDLIB > relative to binary (never cwd)\n",
        "  cache:   --cache-dir > ARANDU_CACHE_DIR > platform-native user cache"
    );
    finish(Err(CliFailure::usage(message)))
}
