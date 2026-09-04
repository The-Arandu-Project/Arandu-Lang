//! Performance instrumentation for the Arandu compiler.
//!
//! Controlled by `-Z` flags initialised once via [`init_z_flags`].
//! All hot-path checks are plain `Relaxed` atomic loads — free on modern CPUs.
//!
//! Output format (colours when stderr is a TTY):
//!
//! ```text
//! [21:13:21] [arandu][perf] parse+check                       1.3ms
//! [21:13:21] [arandu][stat] Cache hits: 2,450  misses: 52  rate: 97.9%
//! [21:13:21] [arandu][mem]  Arena allocated: 14.2 MB  allocs: 3,812
//! [21:13:21] [arandu][info] Compilation finished successfully
//! ```

use std::{
    io::IsTerminal,
    path::PathBuf,
    sync::OnceLock,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::Instant,
};

// ── ANSI colour palette ───────────────────────────────────────────────────────

const RESET: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";
/// Bright white — timestamp
const WHITE: &str = "\x1b[97m";
/// Dark grey — `[arandu]` host tag
const GREY: &str = "\x1b[90m";
/// Bright green — `[perf]` + pass timings
const GREEN: &str = "\x1b[92m";
/// Bright cyan — `[stat]` + query stats
const CYAN: &str = "\x1b[96m";
/// Bright yellow — `[mem]` + memory stats
const YELLOW: &str = "\x1b[93m";
/// Bright red — warnings / errors in perf context
const RED: &str = "\x1b[91m";

// ── Global flag atomics (written once at startup, read-only afterwards) ───────

/// `-Ztime-passes` — print elapsed time for each compiler pass.
pub static TIME_PASSES: AtomicBool = AtomicBool::new(false);
/// `-Zprofile-queries` — track `TyCtx` binding cache hit/miss counts.
pub static PROFILE_QUERIES: AtomicBool = AtomicBool::new(false);
/// `-Zprint-alloc-stats` — print `BumpArena` allocation statistics.
pub static PRINT_ALLOC_STATS: AtomicBool = AtomicBool::new(false);
/// `-Zdump-mir` — dump MIR after each pass.
pub static DUMP_MIR: AtomicBool = AtomicBool::new(false);
/// `--no-generational-fallback` / `-Zno-generational-fallback` (G2 / F2.3.3):
/// promote O004 escape notes to hard errors. Opt-in; never a silent default.
pub static NO_GENERATIONAL_FALLBACK: AtomicBool = AtomicBool::new(false);

/// `-Zdebug-parser` — enable trace-level logging for the parser.
pub static DEBUG_PARSER: AtomicBool = AtomicBool::new(false);
/// `-Zdebug-typeck` — enable trace-level logging for type checking & unification.
pub static DEBUG_TYPECK: AtomicBool = AtomicBool::new(false);
/// `-Zdebug-ossa` — enable trace-level logging for move checker, OSSA & AMIR passes.
pub static DEBUG_OSSA: AtomicBool = AtomicBool::new(false);
/// `-Zdebug-layout` — enable trace-level logging for the layout engine.
pub static DEBUG_LAYOUT: AtomicBool = AtomicBool::new(false);
/// `-Zdebug-backend` — enable trace-level logging for Cranelift codegen.
pub static DEBUG_BACKEND: AtomicBool = AtomicBool::new(false);
/// `-Zdebug-all` — enable all debug categories above.
pub static DEBUG_ALL: AtomicBool = AtomicBool::new(false);

/// `-Zself-profile=<path>` — path for Trace Event JSON output.
pub static SELF_PROFILE_PATH: OnceLock<String> = OnceLock::new();
/// `-Zexplain-rebuild` — DX.5: log Salsa WillExecute / validate chain.
pub static EXPLAIN_REBUILD: AtomicBool = AtomicBool::new(false);

// ── Global metric counters ────────────────────────────────────────────────────

/// Total `TyCtx` binding lookups that found a result.
pub static QUERY_HITS: AtomicU64 = AtomicU64::new(0);
/// Total `TyCtx` binding lookups that returned `None`.
pub static QUERY_MISSES: AtomicU64 = AtomicU64::new(0);
/// Total bytes handed out by all `BumpArena` instances.
pub static ALLOCATED_BYTES: AtomicU64 = AtomicU64::new(0);
/// Total individual `BumpArena` allocation requests.
pub static ALLOC_COUNT: AtomicU64 = AtomicU64::new(0);

// ── Colour detection (cached) ─────────────────────────────────────────────────

static COLOR_ENABLED: AtomicBool = AtomicBool::new(false);
static COLOR_CHECKED: AtomicBool = AtomicBool::new(false);

/// Returns `true` if the terminal supports ANSI colour codes.
fn use_color() -> bool {
    if COLOR_CHECKED.load(Ordering::Relaxed) {
        return COLOR_ENABLED.load(Ordering::Relaxed);
    }
    let enabled = std::io::stderr().is_terminal()
        && std::env::var_os("NO_COLOR").is_none()
        && std::env::var("TERM").map_or(true, |t| t != "dumb");
    COLOR_ENABLED.store(enabled, Ordering::Relaxed);
    COLOR_CHECKED.store(true, Ordering::Relaxed);
    enabled
}

// ── Local time ────────────────────────────────────────────────────────────────

/// Returns `(hour, minute, second)` in UTC time.
fn local_hms() -> (u8, u8, u8) {
    // UTC seconds since epoch (portable, no libc needed).
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let s = (secs % 86400) as u32;
    ((s / 3600) as u8, ((s % 3600) / 60) as u8, (s % 60) as u8)
}

// ── Formatting helpers ────────────────────────────────────────────────────────

/// Format a number with thousands separators: `1234567` → `"1,234,567"`.
fn fmt_num(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out.chars().rev().collect()
}

/// Emit one formatted perf line to `stderr`.
///
/// - `tag_color` — ANSI code for the category tag (e.g. `GREEN` for `[perf]`).
/// - `tag`       — short category string, e.g. `"perf"`, `"stat"`, `"mem"`.
/// - `msg`       — the message body (already formatted).
fn emit(tag_color: &str, tag: &str, msg: &str) {
    let (h, m, s) = local_hms();
    if use_color() {
        eprintln!(
            "{WHITE}{DIM}[{h:02}:{m:02}:{s:02}]{RESET} \
             {GREY}[arandu]{RESET}{tag_color}{BOLD}[{tag}]{RESET} \
             {msg}",
        );
    } else {
        eprintln!("[{h:02}:{m:02}:{s:02}] [arandu][{tag}] {msg}");
    }
}

// ── Initialisation ────────────────────────────────────────────────────────────

/// Initialise the `-Z` flags from the CLI argument list.
///
/// Called once in `main` before any compilation begins.
/// Accepts values with or without the `-Z` prefix
/// (e.g. `"time-passes"` or `"-Ztime-passes"`).
pub fn init_z_flags(flags: &[String]) {
    for flag in flags {
        let key = flag.trim_start_matches("-Z");
        // Split flag=value pairs (e.g. self-profile=trace.json).
        let (op, value) = match key.split_once('=') {
            Some((k, v)) => (k, Some(v)),
            None => (key, None),
        };
        match op {
            "time-passes" => TIME_PASSES.store(true, Ordering::Relaxed),
            "profile-queries" => PROFILE_QUERIES.store(true, Ordering::Relaxed),
            "print-alloc-stats" => PRINT_ALLOC_STATS.store(true, Ordering::Relaxed),
            "dump-mir" => DUMP_MIR.store(true, Ordering::Relaxed),
            "debug-parser" => DEBUG_PARSER.store(true, Ordering::Relaxed),
            "debug-typeck" => DEBUG_TYPECK.store(true, Ordering::Relaxed),
            "debug-ossa" => DEBUG_OSSA.store(true, Ordering::Relaxed),
            "debug-layout" => DEBUG_LAYOUT.store(true, Ordering::Relaxed),
            "debug-backend" => DEBUG_BACKEND.store(true, Ordering::Relaxed),
            "debug-all" => DEBUG_ALL.store(true, Ordering::Relaxed),
            "explain-rebuild" => EXPLAIN_REBUILD.store(true, Ordering::Relaxed),
            "no-generational-fallback" => NO_GENERATIONAL_FALLBACK.store(true, Ordering::Relaxed),
            "self-profile" => {
                if let Some(path) = value {
                    let _ = SELF_PROFILE_PATH.set(path.to_string());
                } else if use_color() {
                    eprintln!(
                        "{RED}[perf] warning: -Zself-profile requires a path (e.g. -Zself-profile=trace.json){RESET}"
                    );
                } else {
                    eprintln!(
                        "[perf] warning: -Zself-profile requires a path (e.g. -Zself-profile=trace.json)"
                    );
                }
            }
            other => {
                if use_color() {
                    eprintln!("{RED}[perf] warning: unknown -Z flag '{other}'{RESET}");
                } else {
                    eprintln!("[perf] warning: unknown -Z flag '{other}'");
                }
            }
        }
    }
}

/// Build a [`crate::tracing_bridge::TracingConfig`] from the current `-Z` flag atomics.
///
/// Called once after `init_z_flags` to pass configuration into the tracing
/// subsystem.
pub fn build_tracing_config() -> crate::tracing_bridge::TracingConfig {
    crate::tracing_bridge::TracingConfig {
        debug_parser: DEBUG_PARSER.load(Ordering::Relaxed),
        debug_typeck: DEBUG_TYPECK.load(Ordering::Relaxed),
        debug_ossa: DEBUG_OSSA.load(Ordering::Relaxed),
        debug_layout: DEBUG_LAYOUT.load(Ordering::Relaxed),
        debug_backend: DEBUG_BACKEND.load(Ordering::Relaxed),
        debug_all: DEBUG_ALL.load(Ordering::Relaxed),
        self_profile: SELF_PROFILE_PATH.get().map(PathBuf::from),
    }
}

/// Returns `true` if any `-Zdebug-*` flag is currently active.
#[inline(always)]
pub fn any_debug_flag_active() -> bool {
    DEBUG_PARSER.load(Ordering::Relaxed)
        || DEBUG_TYPECK.load(Ordering::Relaxed)
        || DEBUG_OSSA.load(Ordering::Relaxed)
        || DEBUG_LAYOUT.load(Ordering::Relaxed)
        || DEBUG_BACKEND.load(Ordering::Relaxed)
        || DEBUG_ALL.load(Ordering::Relaxed)
}

// ── PassTimer — RAII guard for pass timing ────────────────────────────────────

/// RAII timer for a single compiler pass.
///
/// Create via the `time_pass!` macro. When dropped, prints elapsed time
/// to `stderr` when `-Ztime-passes` is enabled. If disabled, the timer is
/// never created and `Instant::now()` is never called.
pub struct PassTimer {
    name: &'static str,
    start: Instant,
}

impl PassTimer {
    #[inline(always)]
    #[must_use]
    pub fn new(name: &'static str) -> Self {
        Self {
            name,
            start: Instant::now(),
        }
    }
}

impl Drop for PassTimer {
    fn drop(&mut self) {
        if TIME_PASSES.load(Ordering::Relaxed) {
            let ms = self.start.elapsed().as_secs_f64() * 1000.0;
            let (color, suffix) = if ms > 100.0 {
                (RED, " ⚠")
            } else if ms > 20.0 {
                (YELLOW, "")
            } else {
                (GREEN, "")
            };

            if use_color() {
                let (h, m, s) = local_hms();
                eprintln!(
                    "{WHITE}{DIM}[{h:02}:{m:02}:{s:02}]{RESET} \
                     {GREY}[arandu]{RESET}{GREEN}{BOLD}[perf]{RESET} \
                     {color}{:<32}{RESET} {BOLD}{ms:>8.3}ms{RESET}{suffix}",
                    self.name,
                );
            } else {
                let (h, m, s) = local_hms();
                eprintln!(
                    "[{h:02}:{m:02}:{s:02}] [arandu][perf] {:<32} {:>8.3}ms",
                    self.name, ms
                );
            }
        }
    }
}

// ── time_pass! macro ──────────────────────────────────────────────────────────

/// Start a pass timer scoped to the current block.
///
/// The timer is only created when `-Ztime-passes` is active.
///
/// # Example
///
/// ```no_run
/// {
///     arandu_base::time_pass!("lower-hir");
///     // ... work ...
/// } // timer drops and prints elapsed time if -Ztime-passes is set
/// ```
#[macro_export]
macro_rules! time_pass {
    ($name:expr) => {
        let _timer = if $crate::perf::TIME_PASSES.load(::std::sync::atomic::Ordering::Relaxed) {
            Some($crate::perf::PassTimer::new($name))
        } else {
            None
        };
    };
}

// ── Hot-path inline helpers ───────────────────────────────────────────────────

/// Record a `TyCtx` binding cache hit. No-op when `-Zprofile-queries` is off.
#[inline(always)]
pub fn track_query_hit() {
    if PROFILE_QUERIES.load(Ordering::Relaxed) {
        QUERY_HITS.fetch_add(1, Ordering::Relaxed);
    }
}

/// Record a `TyCtx` binding cache miss. No-op when `-Zprofile-queries` is off.
#[inline(always)]
pub fn track_query_miss() {
    if PROFILE_QUERIES.load(Ordering::Relaxed) {
        QUERY_MISSES.fetch_add(1, Ordering::Relaxed);
    }
}

/// Record an arena allocation of `bytes`. No-op when `-Zprint-alloc-stats` is off.
#[inline(always)]
pub fn track_alloc(bytes: usize) {
    if PRINT_ALLOC_STATS.load(Ordering::Relaxed) {
        ALLOC_COUNT.fetch_add(1, Ordering::Relaxed);
        ALLOCATED_BYTES.fetch_add(bytes as u64, Ordering::Relaxed);
    }
}

// ── Summary report ────────────────────────────────────────────────────────────

/// Print the consolidated performance report to `stderr`.
///
/// Call once at the end of `main`. Each section is printed only when its
/// corresponding `-Z` flag is active.
pub fn print_perf_summary() {
    if PROFILE_QUERIES.load(Ordering::Relaxed) {
        let hits = QUERY_HITS.load(Ordering::Relaxed);
        let misses = QUERY_MISSES.load(Ordering::Relaxed);
        let total = hits + misses;
        let rate = if total > 0 {
            (hits as f64 / total as f64) * 100.0
        } else {
            0.0
        };

        emit(
            CYAN,
            "stat",
            &format!(
                "Cache hits: {}  misses: {}  total: {}",
                fmt_num(hits),
                fmt_num(misses),
                fmt_num(total)
            ),
        );
        let rate_color = if rate >= 95.0 {
            GREEN
        } else if rate >= 80.0 {
            YELLOW
        } else {
            RED
        };
        if use_color() {
            let (h, m, s) = local_hms();
            eprintln!(
                "{WHITE}{DIM}[{h:02}:{m:02}:{s:02}]{RESET} \
                 {GREY}[arandu]{RESET}{CYAN}{BOLD}[stat]{RESET} \
                 Hit rate: {rate_color}{BOLD}{rate:.1}%{RESET}",
            );
            eprintln!(
                "{WHITE}{DIM}[{h:02}:{m:02}:{s:02}]{RESET} \
                 {GREY}[arandu]{RESET}{CYAN}{BOLD}[stat]{RESET} \
                 Incremental queries processed: {}",
                fmt_num(total),
            );
        } else {
            let (h, m, s) = local_hms();
            eprintln!("[{h:02}:{m:02}:{s:02}] [arandu][stat] Hit rate: {rate:.1}%");
        }
    }

    if PRINT_ALLOC_STATS.load(Ordering::Relaxed) {
        let bytes = ALLOCATED_BYTES.load(Ordering::Relaxed);
        let count = ALLOC_COUNT.load(Ordering::Relaxed);
        let mb = bytes as f64 / (1024.0 * 1024.0);

        // Estimate internal fragmentation (alignment waste).
        // Average alignment overhead ≈ half the typical alignment (8B → 3.5%).
        let frag_pct = if bytes > 0 {
            // count * avg_align_waste / bytes  (rough approximation)
            (count as f64 * 3.5) / bytes as f64 * 100.0
        } else {
            0.0
        };

        emit(
            YELLOW,
            "mem",
            &format!("Arena allocated: {mb:.1} MB  requests: {}", fmt_num(count)),
        );
        if use_color() {
            let (h, m, s) = local_hms();
            eprintln!(
                "{WHITE}{DIM}[{h:02}:{m:02}:{s:02}]{RESET} \
                 {GREY}[arandu]{RESET}{YELLOW}{BOLD}[mem]{RESET} \
                 Fragmentation estimate: {:.2}%",
                frag_pct.min(99.99),
            );
        } else {
            let (h, m, s) = local_hms();
            eprintln!(
                "[{h:02}:{m:02}:{s:02}] [arandu][mem] Fragmentation estimate: {:.2}%",
                frag_pct.min(99.99)
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    fn reset_flags() {
        TIME_PASSES.store(false, Ordering::Relaxed);
        PROFILE_QUERIES.store(false, Ordering::Relaxed);
        PRINT_ALLOC_STATS.store(false, Ordering::Relaxed);
        DUMP_MIR.store(false, Ordering::Relaxed);
        DEBUG_PARSER.store(false, Ordering::Relaxed);
        DEBUG_TYPECK.store(false, Ordering::Relaxed);
        DEBUG_OSSA.store(false, Ordering::Relaxed);
        DEBUG_LAYOUT.store(false, Ordering::Relaxed);
        DEBUG_BACKEND.store(false, Ordering::Relaxed);
        DEBUG_ALL.store(false, Ordering::Relaxed);
        EXPLAIN_REBUILD.store(false, Ordering::Relaxed);
        NO_GENERATIONAL_FALLBACK.store(false, Ordering::Relaxed);
    }

    #[test]
    fn test_init_z_flags() {
        reset_flags();

        init_z_flags(&[
            "-Ztime-passes".to_string(),
            "print-alloc-stats".to_string(),
            "-Zexplain-rebuild".to_string(),
        ]);

        assert!(TIME_PASSES.load(Ordering::Relaxed));
        assert!(PRINT_ALLOC_STATS.load(Ordering::Relaxed));
        assert!(EXPLAIN_REBUILD.load(Ordering::Relaxed));

        assert!(!DEBUG_PARSER.load(Ordering::Relaxed));
        assert!(!DEBUG_TYPECK.load(Ordering::Relaxed));

        init_z_flags(&["debug-parser".to_string(), "-Zdebug-typeck".to_string()]);

        assert!(DEBUG_PARSER.load(Ordering::Relaxed));
        assert!(DEBUG_TYPECK.load(Ordering::Relaxed));

        reset_flags();
    }
}
