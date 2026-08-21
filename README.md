# Arandu

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE-MIT)
[![License: Apache 2.0](https://img.shields.io/badge/License-Apache%202.0-blue.svg)](LICENSE-APACHE)

Arandu is an experimental Brazilian systems programming language focused on memory safety, clean syntax, explicit errors, and native tooling.

## Current Status

**Solidification gate (S5) closed** — foundation (DoD AMIR `TypeId`, spans, `DataLayout`, host C↔Cranelift parity, unified imports) is stable enough to resume language-level Fase 3 work. Details: [docs/arandu-solidification-matrix-v0.1.md](docs/arandu-solidification-matrix-v0.1.md).

**Product freeze:** [Arandu Minimal 0.1](docs/arandu-minimal-0.1-freeze.md) — language surface green; **project CLI gold** (`new` / `check` / `run` / `build` / `doctor`, stdlib via `current_exe`) in [docs/arandu-project-cli-gold-v0.1.md](docs/arandu-project-cli-gold-v0.1.md). Install tarballs ship from **GitHub Releases** on `v*` tags (see below).

**Current execution roadmaps:** [compiler stabilization gold](docs/arandu-stability-gold-roadmap-v0.1.md) and [LSP/editor gold](docs/arandu-lsp-editor-gold-roadmap-v0.1.md). Implemented milestones are not called `gold` unless their published scope and gates are complete.

Documentation map: [docs/README.md](docs/README.md).

Implemented:

- Rust workspace.
- Lexer crate.
- Token stream CLI.
- Golden lexer tests.
- Smoke lexing for official stable and invalid examples.
- Parser crate with AST debug output for the current parser slice.
- Parser golden tests for declarations, generics, extern, match, interpolation, places, and expressions.
- Semantics crate with v0.2 name resolution, hierarchical symbol tables, namespace imports, builtin prelude (`io` / `err` on the CLI path), doc comment mapping, diagnostics, and CLI `check`.
- Official `examples/stable/**` type-check via `arandu_cli check` (prelude + current semantics).
- Type checker v0.1 core with primitive types, assignments, returns, fields, indexing, generics constraints, interface satisfaction, `Result<T,E>`, `Option<T>`, nullable/safe operations, and diagnostics.
- AHIR lowering and pretty-printing with golden tests (`tests/hir/`).
- AMIR lowering v0.1 (experimental) with CFG, locals, match, defer/errdefer, `?`/safe ops, for-in, alloc/free, and golden tests (`tests/codegen/`).
- Dense AMIR types (`TypeId` on locals/temps), use-site spans on ownership diags, shared rvalue visitor.
- Method receivers with `shared self`, `mut self`, and `own self`.
- Definite initialization analysis with O008 diagnostics.
- OSSA foundation in AMIR: move/copy operands, storage lifetime markers, and destroy statements.
- Intraprocedural move checker with O001/O005/O007 diagnostics.
- Opt-in AMIR optimizer (`amir --opt`) with constant folding and DCE.
- Type interning, `DataLayout` (host / 32-bit / i686), and monomorphization graph infrastructure.
- Cranelift JIT backend (experimental, **host** dev/debug) with `run` CLI support.
- GNU C emit path (`emit-c --layout=host|ptr4|ptr8|i686`) — layout-aware source;
  cross compilation still requires a matching external target toolchain and sysroot.
- **ToStr v0.1** — auto-format `bool`, integers (incl. fixed-width), floats, `char`, and `str` in:
  - string interpolation (`"n=${n}"`)
  - call args whose formal type is `str` (e.g. `io.println(42)`)
  - method form `value.to_str()`
  - Prelude stays `(str) -> void` for `io.println`; host/C provide a debug `println` stub.
  - Formatted buffers use `malloc` (process-lifetime leak OK for debug; free/ownership later).
  - User `Display` / custom formatting for structs is later.
- **Salsa query DB** (`arandu_query`) — incremental `parse` → `resolve` → `type_check` → `lower_amir`; DX.5 `-Zexplain-rebuild` / run `[cached]`/`[rebuilt]`.
- **LSP gold** (`arandu-lsp`) — diagnostics, goto/hover/complete/signatureHelp/refs/rename/symbols, **type-aware semantic tokens**, **format**, **code actions** (quickfix `;`).  
  Failure and snapshot behavior is specified in the
  [CLI/LSP contract](docs/arandu-cli-lsp-contract-v0.1.md).
- **CST-first** (rowan): `syntax_tree` → lower AST; reparse de subtree por ITEM; crate `arandu_fmt` + CLI `fmt`.
- **Project CLI (P2 gold)** — `arandu_cli new|doctor|check|run|build`; `Arandu.toml` as Salsa input; stdlib cascade (`--stdlib-path` > `ARANDU_STDLIB` > relative to binary).

Not gold / still partial or experimental:

- Full typed/self-hosted generational fallback beyond the current i64 GenRef MVP
- Full user `Display` trait / custom `to_str` for structs/enums
- Full ownership surface syntax
- Production C polish / freestanding RT; LLVM release backend

**Compiler roadmap (single source of truth):** [docs/arandu-compiler-roadmap-v0.1.md](docs/arandu-compiler-roadmap-v0.1.md)

## Style Guide

Arandu has strong idiomatic casing rules, largely driven by the parser which can differentiate between value identifiers and type identifiers based on casing:

- **Values & Functions**: `camelCase` (e.g. `userName`, `totalPrice`, `buscarUsuario`, `parseJson`). This includes variables, parameters, functions, and struct fields.
- **Types & Structs**: `PascalCase` (e.g. `User`, `HttpClient`, `LoadState`). This includes structs, enums, interfaces, and type aliases.
- **Enum Variants**: `PascalCase` (e.g. `Ok`, `Err`, `Loading`, `NotFound`).
- **Generics**: Short `PascalCase` (e.g. `T`, `K`, `V`, `Item`).
- **Modules**: Lowercase dot-separated (e.g. `net.http`, `app.userService`).
- **Files**: `snake_case.aru` (e.g. `user_service.aru`).
- **Constants**: `SCREAMING_SNAKE_CASE` or `camelCase` (e.g. `MAX_RETRIES`, `maxRetries`).

*Note: `snake_case` is allowed for values but `camelCase` is the officially recommended and preferred style for all Arandu code.*

## Install (release tarball)

Tagged releases (`vX.Y.Z`, matching `crates/arandu_cli` version) build host packages and attach them to the [GitHub Release](https://github.com/BrunoF2P/Arandu-Lang/releases):

| Asset | Host |
|-------|------|
| `arandu-*-x86_64-unknown-linux-gnu.tar.gz` | Linux x86_64 |
| `arandu-*-aarch64-apple-darwin.tar.gz` | macOS Apple Silicon |

> **Note:** GitHub no longer hosts `macos-13` (Intel) runners. Intel Mac users should build from source (`./scripts/install-local.sh`) until we add cross-compile assets.

Each archive has a `.blake3` sidecar. Install from a checkout (or copy `scripts/install-from-tarball.sh`):

```bash
# example: Linux x86_64, version 0.0.1
gh release download v0.0.1 -p 'arandu-*-x86_64-unknown-linux-gnu.tar.gz*'
bash scripts/install-from-tarball.sh ./arandu-0.0.1-x86_64-unknown-linux-gnu.tar.gz
# puts toolchain under ~/.local/arandu and symlinks in ~/.local/arandu/bin
```

From the monorepo without a Release:

```bash
./scripts/install-local.sh          # build + versioned prefix install
./scripts/package-release.sh        # dist/arandu-$VERSION-$TARGET.tar.gz + BLAKE3
```

## Requirements

- `rustup` with the exact verified toolchain from `rust-toolchain.toml`.

Rustup selects and installs Rust 1.97.1 plus `rustfmt` and Clippy automatically
from the repository configuration. Confirm the active toolchain with:

```bash
rustup show active-toolchain
```

Arandu does not currently promise an MSRV. New Rust stable releases are tested
separately and adopted only through a reviewed `rust-toolchain.toml` update.

## Language server

```bash
cargo run -p arandu_lsp --release
# point the editor at the `arandu-lsp` binary (stdio)
```

Architecture: [docs/arandu-salsa-lsp-architecture-v0.1.md](docs/arandu-salsa-lsp-architecture-v0.1.md).

## Run

Run the canonical S0 validation from the workspace root, in this order:

```bash
cargo fmt --all -- --check
cargo check --workspace --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --locked
cargo run --locked -p xtask -- check-diag-docs
```

Print tokens for the hello example:

```bash
cargo run -p arandu_cli -- lex examples/stable/syntax/hello.aru
```

Print the parser AST debug output:

```bash
cargo run -p arandu_cli -- parse examples/stable/syntax/hello.aru
```

Run parse + name resolution + type check:

```bash
cargo run -p arandu_cli -- check examples/stable/syntax/hello.aru
```

Print the AHIR (typed high-level IR):

```bash
cargo run -p arandu_cli -- hir examples/stable/syntax/hello.aru
cargo run -p arandu_cli -- hir examples/stable/syntax/hello.aru --debug
```

Print the AMIR (mid-level IR / CFG):

```bash
cargo run -p arandu_cli -- amir tests/codegen/add.aru
cargo run -p arandu_cli -- amir tests/codegen/add.aru --debug
cargo run -p arandu_cli -- amir tests/codegen/add.aru --opt
```

Run a program via the Cranelift JIT backend (exit code = `main` return value):

```bash
cargo run -p arandu_cli -- run tests/codegen/add.aru
```

Emit GNU C (layout follows [`DataLayout`](docs/arandu-abi-layout-v0.1.md); see the
[backend contract](docs/arandu-backend-contract-v0.1.md) before cross-compiling):

```bash
cargo run -p arandu_cli -- emit-c examples/stable/syntax/fib_main.aru --layout=host
cargo run -p arandu_cli -- emit-c examples/stable/syntax/fib_main.aru --layout=i686
```

### Compiler instrumentation (`-Z` flags)

Unstable developer flags for profiling and debugging the compiler itself. Pass them before the subcommand:

```bash
cargo run -p arandu_cli -- -Ztime-passes check examples/stable/syntax/variables.aru
cargo run -p arandu_cli -- -Ztime-passes -Zprint-alloc-stats run tests/codegen/add.aru
```

| Flag | Effect |
|------|--------|
| `-Ztime-passes` | Print elapsed time per compiler pass (`parse+check`, `lower-hir`, `codegen`, …) |
| `-Zprofile-queries` | Print `TyCtx` binding cache hit/miss summary at the end |
| `-Zprint-alloc-stats` | Print `BumpArena` allocation totals at the end |
| `-Zdump-mir` | Dump MIR after passes (when wired in the pass pipeline) |

Output goes to **stderr** with `[arandu][perf]`, `[stat]`, `[mem]`, and `[info]` tags. See [docs/arandu-compiler-instrumentation-v0.1.md](docs/arandu-compiler-instrumentation-v0.1.md) for details.

Update golden test files (after intentional IR changes):

```bash
$env:UPDATE_GOLDEN=1; cargo test -p arandu_semantics
```

Parser fixtures:

```bash
cargo test -p arandu_parser
cargo run -p arandu_cli -- parse examples/stable/syntax/structs.aru
cargo run -p arandu_cli -- parse examples/stable/syntax/generics.aru
cargo run -p arandu_cli -- parse examples/stable/syntax/match.aru
```

## Project Structure

```text
crates/
  arandu_lexer/              Rust lexer library
  arandu_parser/             Rust parser library
  arandu_semantics/          Name resolution, type checking, HIR, and AMIR
  arandu_backend_cranelift/  Experimental Cranelift JIT backend
  arandu_cli/                Debug CLI for compiler experiments

docs/             Language and compiler design notes
examples/         Official stable, invalid, and draft examples
tests/lexer/      Lexer golden fixtures
tests/parser/     Parser golden fixtures
tests/semantics/  Semantics diagnostic fixtures
tests/hir/        AHIR golden fixtures (.aru → .hir)
tests/codegen/    AMIR golden fixtures (.aru → .amir)
tests/ui/         UI diagnostic fixtures (.aru → .diag)
```

## Next Steps

Close the [compiler stabilization gold roadmap](docs/arandu-stability-gold-roadmap-v0.1.md) and the [LSP/editor gold roadmap](docs/arandu-lsp-editor-gold-roadmap-v0.1.md) before selecting the next major feature phase from the [master roadmap](docs/arandu-compiler-roadmap-v0.1.md).

## License

This project is dual-licensed under both the [MIT License](LICENSE-MIT) and the [Apache License, Version 2.0](LICENSE-APACHE).
