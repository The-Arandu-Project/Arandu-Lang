# Arandu Compiler Instrumentation v0.1

Status: implemented (dev/debug)
Crate: `arandu_base::perf`
CLI: `arandu_cli` (`-Z` flags)

## Visão Geral e Contexto

A instrumentação fornece observabilidade opt-in para desenvolvimento sem
transformar queries puras em fontes de efeitos observáveis.

## Detalhes Técnicos da Implementação

### Goal

Provide lightweight, opt-in observability for compiler development: pass timings, query cache stats, and arena allocation totals. All flags are **off by default** and use `Relaxed` atomic loads on hot paths so they add near-zero overhead when disabled.

### Usage

Pass `-Z` flags anywhere before the subcommand:

```bash
cargo run -p arandu_cli -- -Ztime-passes check path/to/file.aru
cargo run -p arandu_cli -- -Ztime-passes -Zprofile-queries run path/to/file.aru
```

On PowerShell:

```powershell
cargo run -p arandu_cli -- -Ztime-passes run tests/codegen/add.aru
```

### Flags

| Flag | Global atomic | What it measures |
|------|---------------|------------------|
| `-Ztime-passes` | `TIME_PASSES` | RAII pass timers via `time_pass!` macro |
| `-Zprofile-queries` | `PROFILE_QUERIES` | `TyCtx` binding cache hits/misses |
| `-Zprint-alloc-stats` | `PRINT_ALLOC_STATS` | `BumpArena` bytes and allocation count |
| `-Zdump-mir` | `DUMP_MIR` | MIR dumps after passes (when enabled in pipeline) |

Unknown flags print a warning to stderr and are ignored.

### Output format

All instrumentation writes to **stderr**. When the terminal supports colour and `NO_COLOR` is unset:

```text
[21:13:21] [arandu][perf] parse+check                       1.300ms
[21:13:21] [arandu][info] Syntax analysis and type-check completed
[21:13:21] [arandu][stat] Cache hits: 2,450  misses: 52  total: 2,502
[21:13:21] [arandu][mem]  Arena allocated: 14.2 MB  allocs: 3,812
```

Pass timings slower than 20 ms appear in yellow; slower than 100 ms in red with a warning marker.

### Passes instrumented today

The CLI wires `time_pass!` around:

| Pass name | Commands |
|-----------|----------|
| `parse+check` | `check`, `hir`, `amir`, `run` |
| `lower-hir` | `hir`, `amir`, `run` |
| `lower-amir` | `amir`, `run` |
| `optimize-amir` | `amir --opt`, `run --opt` |
| `codegen` | `run` |

`perf_info!` lines appear only when **any** `-Z` flag is active.

### API for compiler crates

```rust
// In a pass entry point:
arandu_base::time_pass!("my-pass");
// ... work ...

// Hot-path counters (no-op when flag off):
arandu_base::track_query_hit();
arandu_base::track_alloc(layout.size());

// End of CLI command:
arandu_base::print_perf_summary();
```

Initialize once at startup:

```rust
arandu_base::init_z_flags(&z_flags);
```

### Roadmap alignment

This covers the **PERF** milestone (Phase 2) at a minimal level. Future work:

- Wire `-Zdump-mir` through AMIR/HIR dump hooks
- Per-file breakdown in `--parallel` mode
- JSON output mode for CI regression tracking
- Benchmark harness crate comparing pass timings across commits

### Related

- Implementation: `crates/arandu_base/src/perf.rs`
- CLI wiring: `crates/arandu_cli/src/main.rs`
- Roadmap: `docs/arandu-compiler-roadmap-v0.1.md` (PERF item)

## PONTOS DE MELHORIA (O que não está no roadmap)

O buffer de Trace Event pode crescer com campanhas longas e o sink vive em
`arandu_base::tracing_bridge`; ele é a exceção de I/O explicitamente verificada
pelo guardrail arquitetural.

## Futuro e Próximos Passos

Usar esta instrumentação com corpus estável para localizar hot paths; se
necessário, tornar o sink streaming/bounded sem inserir I/O nas queries.
