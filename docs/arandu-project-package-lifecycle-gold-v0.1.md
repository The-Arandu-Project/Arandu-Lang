# Arandu — Project & Package Lifecycle Gold v0.1

**Status:** active campaign  
**Owner:** `arandu_cli` orchestrates filesystem/network effects;
`arandu_query` owns deterministic manifest, package-graph and directory inputs.  
**Prerequisite:** compiler, distribution and installed-SDK gates are Gold in their
published scope.

## Goal

Turn the existing project-mode CLI into a reproducible package lifecycle that
works outside the monorepo on Windows, Linux and macOS. A user must be able to
create, inspect, build, test and clean a project without knowing compiler
internals. The result is the prerequisite for the testing/benchmark harness,
the remote compiler service and the public playground.

Gold here does **not** mean a public registry. The first stable package graph is
local/workspace-first. Remote Git and registry resolution remain disabled until
identity, lockfile integrity and cache isolation are proven.

## Current implementation audit

| Area | Current state | Gold gap |
| --- | --- | --- |
| Project creation | `arandu new` writes `Arandu.toml` and `src/main.aru` | no `init`, package kind, README, `.gitignore`, tests or VCS policy |
| Manifest | hand-written string-only parser for `name`, `version`, `entry` | not full TOML; duplicate keys overwrite; unknown keys are silently ignored; no schema/toolchain version |
| Incrementality | raw manifest BLAKE3 and fields are Salsa inputs | package graph, target declarations and resolved dependencies are absent |
| Build | package `check/run/build` works; Cranelift build reports success | no stable on-disk artifact contract or project-local output directory |
| Dependencies | package-local modules and stdlib roots work | no dependency declaration, resolver, lockfile, workspace or global source cache |
| Imports | dotted imports lower deterministically to `.aru` keys; public symbols are cutoff-friendly | package name, logical module and physical file are conflated; bare/quoted paths can bypass future package boundaries |
| Portability | installed SDK smoke is native on three OS families | generated projects are not yet exercised as a complete lifecycle outside checkout |

The existing narrow query boundaries are retained. Parsing a manifest is pure;
reading it, walking directories, resolving VCS and writing files stay in CLI or
dedicated effectful infrastructure before values enter Salsa.

## Decisions informed by existing ecosystems

### Why TOML remains the right manifest format

The comparison was reopened rather than inherited from Cargo:

| Format | Strength | Reason not selected |
| --- | --- | --- |
| JSON | ubiquitous, strict data model and strong schema tooling | no native comments; noisy for hand-maintained dependency tables |
| YAML | concise and expressive | specification and implicit typing are too broad for a compiler control file; parser behavior is harder to constrain |
| custom `mod`-style DSL | could optimize package syntax | creates a second language, parser, formatter and editor ecosystem with no semantic benefit yet |
| executable manifest | arbitrary build logic | reading dependencies becomes host-code execution and harms reproducibility/security |
| TOML 1.0 | typed, UTF-8, comments, unambiguous tables, mature Rust parsers | table/dotted-key rules require good diagnostics and tool edits must preserve user formatting |

**Decision:** both `arandu.toml` and generated `arandu.lock` use TOML 1.0.
The manifest accepts the complete TOML syntax but a strict Arandu schema. The
lockfile uses a canonical machine-written TOML subset so byte determinism is
under our control. We do not maintain a partial TOML parser.

`arandu check/build/run` never rewrite the user manifest. Future `add/remove`
commands must use a lossless TOML document editor or narrowly edit the owned
dependency table; deserializing and serializing the entire file is forbidden
because it would erase comments and create noisy diffs.

This choice follows the same human-editable/static-metadata rationale recorded
by Python packaging, while avoiding Python's later ambiguity between static and
dynamic metadata: Arandu metadata is static unless a future field explicitly
defines a reproducible source. No build backend may silently replace a value
written by the user.

### Declarative, versioned manifest

The canonical filename is **`arandu.toml`** and the generated lockfile is
**`arandu.lock`**. Lowercase avoids case-only ambiguity across filesystems and
matches the spelling used in commands and documentation. Migration from the
legacy `Arandu.toml` is explicit:

1. discovery prefers `arandu.toml`;
2. a legacy-only project loads with one structured migration warning;
3. if two distinct files exist, commands fail instead of guessing;
4. `arandu init/new` only write the canonical name;
5. legacy discovery is removed only at a declared release boundary.

The manifest is data, never executable code. Unlike programmable build
manifests, loading an untrusted package cannot run arbitrary host code. Unknown
keys in Arandu-owned tables are errors with suggestions; a namespaced
`[metadata]` table is reserved for forward-compatible third-party data.

Initial schema:

```toml
schema = 1

[package]
name = "hello"
version = "0.1.0"
edition = "2026"

[toolchain]
arandu = ">=0.1.0-rc.4, <0.2.0"

[targets.bin]
name = "hello"
root = "src/main.aru"

[dependencies]
# util = { path = "../util" }
```

Paths are UTF-8 manifest data resolved relative to the manifest directory,
lexically normalized and then containment-checked where containment is part of
the contract. Package names reject Windows device names and other spellings
that cannot round-trip on supported filesystems.

### Lockfile is resolution, not a cache

`arandu.lock` is generated by the tool, deterministic and committed for root
applications and workspaces. It contains a format version, package identity,
exact source, exact revision/version, content digest and dependency edges. It
must not contain absolute local paths, timestamps, host separators or map
iteration order.

- normal local commands may create/update it only when resolution is required;
- `--locked` fails if it is missing or would change;
- `--offline` forbids all network access but may resolve from verified cache;
- `--frozen` means `--locked --offline`;
- writes use temporary sibling + flush + atomic replace;
- parse or integrity failure never falls back to an unlocked build.

Path dependencies participate by canonical package identity and manifest
fingerprint but are not copied into the global cache. The lockfile records a
portable relative path only when the dependency is inside the workspace.

### Three storage domains

Do not overload one `target` directory with unrelated trust and lifetime rules:

| Domain | Location | Contract |
| --- | --- | --- |
| project artifacts | `<workspace>/target/` | disposable; ignored by Git; `arandu clean` owns only this verified directory |
| project metadata | `<workspace>/.arandu/` | disposable resolution/query metadata; no downloaded executable code |
| global package cache | platform cache directory | content-addressed immutable sources; shared safely; explicit verify/prune commands |

Project output is separated by profile and target triple:

```text
target/<profile>/<target-triple>/bin/
target/<profile>/<target-triple>/deps/
target/<profile>/<target-triple>/incremental/
target/<profile>/<target-triple>/build-state.json
```

Final artifacts are published through staging plus atomic rename. A failed
build cannot replace the last valid binary. `clean` resolves and validates the
exact project root and refuses symlink/junction escapes or broad targets.

### Package identity and resolution

Identity is not a display name alone. The internal key is `(canonical source,
package name)`; a resolved node adds exact version/revision and content digest.
This prevents registry/Git/path aliases from silently becoming the same or two
spellings of the same source from entering the graph twice.

Resolution v0.1 is deliberately small:

1. workspace members;
2. relative path dependencies;
3. exact Git revisions only after the local graph is Gold;
4. registry and broad semantic-version solving later.

The graph is sorted before diagnostics or serialization. Cycles, duplicate
identities, missing manifests, root escapes and source collisions are hard
errors. Dependency manifests cannot select root profiles or mutate the root
workspace.

### Package, target and module are different identities

Arandu adopts four explicit layers instead of treating a source path as all of
them at once:

| Layer | Meaning | Example |
| --- | --- | --- |
| package | versioned/distributed unit from one manifest | `acme_math@1.2.0` |
| target | independently compiled product inside a package | library `math`, binary `calc`, tests |
| module | namespace/analysis unit inside one target | `self.geometry.vector` |
| file | current physical source of a module | `src/geometry/vector.aru` |

`PackageId`, `TargetId` and `ModuleId` must be typed identities. None is a raw
path or a `SymbolId`. A file rename may change the module mapping, but package
resolution never manufactures a `FileId`; CLI/LSP register sources and provide
the resulting immutable package/module map as Salsa inputs.

The manifest target shape replaces the MVP `kind`/`entry` pair before it becomes
a compatibility burden:

```toml
schema = 1

[package]
name = "calculator"
version = "0.1.0"
edition = "2026"

[targets.lib]
name = "calculator"
root = "src/lib.aru"

[targets.bin]
name = "calc"
root = "src/main.aru"

[dependencies]
math = { path = "../math" }
```

The dependency table key (`math`) is the **source-level binding**, while the
resolved package keeps its own declared name and canonical source identity.
Consumers therefore survive an upstream repository rename and two packages
with similar display names can be bound under distinct aliases. Two aliases
that resolve to the same package identity are rejected initially rather than
compiling the same package twice under ambiguous namespaces.

This deliberately combines the strongest parts of Go and Deno without making
source imports a transport protocol. Like Go, a package path is resolved from
an already selected module graph. Like Deno import maps, a short source-level
name is bound centrally. Unlike Go's missing-package lookup and Deno's direct
URL specifiers, an Arandu `import` never searches a registry, contacts a proxy,
or embeds a version/URL. Only an explicit manifest dependency can introduce an
external package; `check`, `build` and `run` do not mutate that graph.

### Canonical import roots

Package mode has three unambiguous roots:

```text
std.path                    standard library
self.geometry.vector        another module in the current target
math.geometry.vector        exported module from direct dependency alias `math`
```

Existing source forms remain useful:

```aru
import self.geometry.vector as vector
import math.geometry as geometry
from math.geometry import { Point, distance }
```

Rules:

1. only a manifest-declared **direct** dependency alias may occupy the first
   external segment; transitive dependencies never leak into source lookup;
2. `self` never means a filesystem current directory and cannot escape its
   target;
3. `std` is a reserved toolchain root and cannot be shadowed by a package;
4. source imports never contain versions, URLs, Git revisions or cache paths;
5. dotted paths are case-sensitive logical identifiers but creation rejects
   case-fold collisions that would break Windows/macOS checkouts;
6. extensionless dotted syntax has exactly one resolution; no probing
   `foo.aru` versus `foo/index.aru` or several source roots;
7. import aliases are file-scoped, matching the current resolver and avoiding
   one file silently changing another file's namespace;
8. package dependency cycles are rejected. Existing deterministic module-cycle
   recovery remains a compiler diagnostic, not permission for cyclic packages.

The current bare form (`import util`) is ambiguous between a local module and a
future dependency named `util`. In package mode it migrates deterministically:

- if `util` is a declared direct dependency alias, it means that dependency's
  root export;
- otherwise the legacy local interpretation is accepted with a structured fix
  to `import self.util` during the migration edition;
- a later edition removes implicit local roots.

Quoted imports such as `import "vendor/file.aru" as vendor` are not a package
dependency mechanism. In package mode, arbitrary quoted filesystem sources are
rejected unless a future explicit foreign-source manifest contract authorizes
them. Single-file CLI mode may retain them for compatibility without allowing
them to enter a resolved package graph.

### Public module surface, no deep imports

An external package cannot import every `.aru` file merely because it exists.
The library target declares an export map; everything else is package-internal:

```toml
[targets.lib]
name = "math"
root = "src/lib.aru"

[targets.lib.exports]
"." = "src/lib.aru"
"geometry" = "src/geometry.aru"
"geometry.vector" = "src/geometry/vector.aru"
```

Thus `import math.geometry` is stable API while
`import math.internal.fast_inverse_sqrt` fails even if that file is present.
The public path is decoupled from layout, preventing consumers from freezing a
dependency's private directory structure. Export targets must be unique,
inside the package, part of the declared library target and case-fold unique.

Inside an exported module, only existing `public` declarations cross the
module boundary. Both gates are required:

```text
package export map permits module
            AND
exported_symbols permits declaration
```

This preserves the current `exported_symbols` early cutoff: editing a private
body does not invalidate dependants, changing the export map invalidates only
the affected package surface, and changing a public signature invalidates its
importers. A future source-level re-export can add convenience, but it must feed
the same explicit export surface rather than creating a second resolver.

### Package graph and module graph stay separate

Resolution occurs in two pure deterministic stages after effectful discovery:

1. **Package graph:** manifest requirements → exact `PackageId`s, targets and
   direct dependency aliases; serialized in `arandu.lock`.
2. **Module graph:** source import paths → `ModuleId`s using the already resolved
   target/module maps; tracked by Salsa and never performs network or filesystem
   discovery.

The lockfile records package edges, not every source import. Module dependencies
remain compiler analysis data and may change on each edit without rewriting the
lockfile. Conversely, changing a dependency alias or resolved package version
replaces the relevant module root as one narrow Salsa input.

`ModuleRoots` therefore evolves from one package plus stdlib into an immutable
`PackageModuleMap` containing:

- current package and target;
- `self` module mapping;
- direct dependency alias → exported module mapping;
- stdlib mapping;
- one deterministic reverse map for diagnostics/LSP.

`canonicalize_import_path` may continue to normalize syntax, but it must return
a logical import (`Std`, `SelfModule`, `DependencyModule`, `LegacyExternal`),
not a guessed filesystem string. Filesystem paths are resolved before query
execution and installed through inputs.

Module/package regressions required before Gold:

- dependency alias root and exported subpath resolve in CLI and LSP;
- undeclared transitive dependency and non-exported deep module fail;
- `self`, dependency and `std` cannot shadow one another;
- two files/exports differing only by case fail portably;
- module/package cycles and duplicate source identities are deterministic;
- path dependency rename/removal refreshes completion, goto and diagnostics;
- dependency body-only edit preserves importer cutoff when exports are stable;
- public signature or export-map edit invalidates exactly the affected importers;
- a malicious module path cannot escape source/cache/workspace roots;
- Windows separators, symlinks/junctions and Unicode normalize to one identity.

### No dependency code execution during resolution

Arandu v0.1 has no install hooks, lifecycle scripts or executable build
manifests. Fetching/resolving a dependency performs data parsing and content
verification only. Future build extensions require a separate RFC with an
explicit capability sandbox; they cannot arrive as an incidental manifest
field.

### Supply-chain threat model

A checksum proves that downloaded bytes equal expected bytes. It does **not**
prove that those bytes are safe: a malicious maintainer can publish malicious
source with a perfectly valid checksum, signature and provenance statement.
Arandu therefore separates four claims:

| Claim | Mechanism | What it does not prove |
| --- | --- | --- |
| integrity | content digest in lockfile/cache | author intent or code safety |
| origin authenticity | canonical source plus signed registry metadata | that a legitimate publisher is uncompromised |
| build provenance | signed source/build attestation and transparency record | absence of malicious source/build instructions |
| policy/audit | explicit review, advisories, source/capability policy | mathematical safety of dependency behavior |

#### Closed-world resolution

- An import never searches a public registry. Only dependencies already named
  in `arandu.toml` enter resolution; missing aliases are errors with no network.
- Every dependency has exactly one explicit source class (`path`, `git`, later
  `registry`) and canonical origin. A private/Git lookup never falls back to a
  public package with the same name.
- The lockfile records the complete transitive graph, canonical origin, exact
  revision/version and normalized source-tree digest. A mirror may replace
  transport only when it serves identical authenticated content.
- CI and release builds use `--frozen`; manifest/lock mismatch, absent cache or
  any attempted network access fails closed.
- Resolver limits bound graph nodes, depth, manifest/archive size and expanded
  file count before untrusted input can exhaust memory or disk.

This blocks dependency-confusion auto-resolution, tag retargeting after lock,
MITM/cache substitution and silent transitive drift. It deliberately gives up
the convenience of discovering a dependency from a source import.

#### Deliberate trust on first addition

The first `arandu add` is the dangerous moment. It must be an explicit command,
never a side effect of `check/build/run`, and must display before mutation:

```text
source identity
selected immutable revision/version
publisher/provenance status when available
new direct and transitive packages
license/advisory status when available
whether any native/binary capability is requested
```

`--yes` is forbidden when trust-relevant information changed unless CI supplies
an explicit policy file. Manifest and lockfile update atomically together only
after resolution succeeds. `arandu update` defaults to a plan/diff and one
named dependency; bulk/latest updates require an explicit flag. Ordinary builds
never opportunistically select a newer version.

For exact Git dependencies, the user specifies a canonical HTTPS/SSH origin and
full commit ID. Arandu hashes the normalized source tree and records both commit
and digest: Git identity aids audit, while the independent digest detects a
different archive/tree. Floating branch/tag requirements are excluded from the
first remote slice.

#### Registry requirements before a registry exists

The first Gold campaign does not launch a registry. A future Arandu registry
must satisfy a separate security gate before `registry = ...` is accepted:

- scoped package identity bound to a publisher/organization, with protected
  names and no public fallback for configured private scopes;
- immutable `(identity, version) → content digest`; yanks hide selection but do
  not delete bytes required by old lockfiles;
- signed, versioned and expiring root/snapshot/targets metadata with rollback,
  freeze and mix-and-match protection (TUF-style roles/thresholds);
- transparency log for publication metadata and provenance attestations;
- trusted publishing with short-lived OIDC credentials instead of long-lived
  upload tokens when supported;
- publisher-key/identity rotation and compromise recovery defined before use;
- staged publication, malware response, advisories and retraction metadata;
- registry mirrors cannot alter identity, digest or provenance policy.

Provenance is exposed to users and policy, but never presented as a “safe code”
badge. npm explicitly documents this limitation: provenance connects an
artifact to source/build context; consumers still have to decide whether to
trust the code.

#### Build-time and runtime authority

- Resolving, fetching, parsing and compiling a dependency does not execute its
  code. Arandu has no `preinstall`, `postinstall`, `build.rs` equivalent or
  programmable manifest in v0.1.
- Native libraries, proc-macro-like extensions and prebuilt binaries are not
  ordinary source dependencies. Each needs a future capability contract,
  explicit allowlist and sandbox/provenance policy.
- A source dependency is eventually linked into the application and can be
  malicious when the application runs. No package manager can hash this risk
  away. `arandu audit`, review, least-authority stdlib APIs and application
  sandboxing remain necessary.
- Dependency tests are not trusted build hooks and are not run automatically
  while consuming the package.

#### Verification and incident response

Planned user-visible tools:

```text
arandu tree                 exact direct/transitive graph and origins
arandu metadata             machine-readable package/target/module graph
arandu verify               lockfile, cached trees, signatures/provenance
arandu audit                advisories, yanks/retractions and policy violations
arandu update <package>     explicit resolution diff
arandu vendor               verified offline source snapshot
```

The lockfile remains buildable after a version is retracted, but `audit` and
updates warn or fail according to policy. A compromised version is never
silently replaced, because doing so would destroy reproducibility and could
substitute a different attack. Security response is an explicit reviewed
lockfile transition.

### VCS behavior is conservative

`arandu new` generates `.gitignore`, but does not require Git. Git initialization
is explicit (`--vcs=git`) or disabled (`--vcs=none`); in `auto`, an enclosing
repository is reused and a nested repository is never created. `arandu init`
operates on an existing directory and refuses to overwrite non-generated files.

Generated ignore entries initially cover:

```gitignore
/target/
/.arandu/
```

The lockfile is not ignored for applications/workspaces.

## Failures observed elsewhere that Arandu must avoid

| Failure class | Arandu guardrail |
| --- | --- |
| dependency install/build hooks execute arbitrary code | no executable manifest or lifecycle hook in v0.1 |
| offline mode silently resolves a different graph | `--offline` and `--locked` are independent; `--frozen` requires both |
| lockfile changes unexpectedly during ordinary commands | resolution policy is explicit and atomic; CI uses `--locked` |
| different origins produce duplicate logical packages | canonical source identity and collision diagnostics |
| mutable/shared cache contaminates builds | immutable content-addressed entries, digest verification and per-entry locks |
| global cache grows forever | inspect, verify and bounded prune commands; never implicit destructive cleanup |
| case-insensitive filesystems collapse distinct names | portable name validation and canonical lowercase control files |
| workspace child configuration shadows the root | one root lockfile and metadata directory; nested roots are diagnosed |
| path dependencies escape a sandbox or package boundary | normalized paths, explicit workspace membership and containment checks |
| failed build leaves a plausible partial artifact | staging and atomic publication of final outputs |
| arbitrary unknown manifest keys appear accepted | strict owned schema with a namespaced metadata escape hatch |
| consumers deep-import private source files | explicit target export map; file existence does not imply public API |
| transitive dependency accidentally becomes importable | only direct dependency aliases enter the module map |
| package rename rewrites every consumer import | manifest dependency key is the stable source-level alias |
| module resolution probes several filesystem layouts | one canonical logical mapping; no extension/index probing |
| source imports smuggle URLs, versions or host paths | dependency source belongs only to manifest/lockfile |
| package and module graphs invalidate each other wholesale | separate immutable inputs and tracked module edges |
| dependency confusion/private-name takeover | imports never discover packages; source/scopes are explicit and never fall back |
| tag or archive changes after resolution | exact commit plus independent normalized tree digest |
| malicious legitimate release | no automatic updates; provenance is not treated as safety; audit/review/policy remain required |
| compromised registry or mirror | signed versioned metadata, immutable digests and future transparency log |
| rollback/freeze/mix-and-match metadata attack | future registry must implement expiring TUF-style role metadata |
| install-time dependency code execution | no lifecycle hooks, executable manifests or build scripts in v0.1 |
| private package name leaks to public service | source routing is explicit; private origin has no public fallback |
| dependency graph/archive resource exhaustion | strict graph, archive, expanded-size and file-count limits |

## Campaign

### P0 — Contract and compatibility

- [ ] Make `arandu.toml`/`arandu.lock` canonical and implement legacy discovery.
- [ ] Replace the ad-hoc parser with a complete, deterministic TOML decoder.
- [ ] Add schema, package kind, edition and toolchain compatibility.
- [ ] Reject duplicates, unknown owned fields, invalid SemVer and unsafe paths.
- [ ] Keep manifest and directory data as narrow Salsa inputs.
- [ ] Reserve versioned capability-policy and compiler-produced effect-summary
  metadata without claiming A2 inference before it exists.

### P1 — Project creation and VCS

- [ ] Implement `arandu init` and `new --bin/--lib`.
- [ ] Generate README, `.gitignore`, `src/` and `tests/` without partial trees.
- [ ] Add `--vcs=auto|git|none`, detecting enclosing repositories.
- [ ] Test reserved names, Unicode, spaces, case collisions and interrupted creation.

### P2 — Artifact lifecycle

- [ ] Define profiles, target triples and `target/` layout.
- [ ] Make `build` produce a real stable artifact outside the monorepo.
- [ ] Publish through staging/atomic rename and retain the last valid artifact.
- [ ] Implement safe `arandu clean` and artifact provenance metadata.

### P3 — Lockfile core

- [ ] Define a versioned deterministic lock format and canonical serializer.
- [ ] Implement atomic generation plus `--locked`, `--offline`, `--frozen`.
- [ ] Reject corruption, stale manifest fingerprints and nonportable fields.
- [ ] Prove byte-identical output across Windows, Linux and macOS.

### P4 — Local packages and workspaces

- [ ] Introduce typed `PackageId`, `TargetId`, `ModuleId` and logical import roots.
- [ ] Support `bin` and `lib` targets plus relative path dependencies.
- [ ] Bind source imports through direct dependency aliases, `self` and `std`.
- [ ] Add explicit library export maps and reject dependency deep imports.
- [ ] Migrate bare local and quoted filesystem imports without ambiguous lookup.
- [ ] Add a single-root workspace with members and shared output/lockfile.
- [ ] Resolve a deterministic package DAG with cycle/collision diagnostics.
- [ ] Feed `PackageModuleMap` through Salsa inputs without filesystem reads in queries.
- [ ] Prove body-edit/export-surface/package-version invalidation boundaries.

### P5 — Verified global cache

- [ ] Specify platform-native cache/config locations and override flags.
- [ ] Store immutable content-addressed package sources with per-entry locking.
- [ ] Add `arandu cache inspect|verify|prune` with bounded, recoverable behavior.
- [ ] Never trust an extracted directory without rechecking its recorded digest.
- [ ] Bound archive bytes, expanded bytes, file count, graph depth and graph size.

### P6 — Remote Git, intentionally narrow

- [ ] Accept secure Git/HTTPS sources pinned to an exact commit.
- [ ] Record canonical origin, commit and content digest in the lockfile.
- [ ] Disable network in `--offline`; never fall back from private to public origins.
- [ ] Make first trust and every update an explicit reviewable graph diff.
- [ ] Add `tree`, `verify`, `audit` and verified `vendor` foundations.
- [ ] Defer floating branches, arbitrary URLs, registries and dependency scripts.

### P7 — Gold promotion

- [ ] Native lifecycle E2E on Windows, Linux and macOS outside the checkout.
- [ ] Concurrent build/cache tests and crash-interrupted atomic-write recovery.
- [ ] Determinism campaign for manifests, graphs, lockfiles and artifact metadata.
- [ ] Adversarial path, symlink/junction, malformed archive and cache-tamper tests.
- [ ] Dependency-confusion, origin-substitution, rollback and graph/archive-bomb tests.
- [ ] Installed SDK smoke: `new → check → build → run → clean`.
- [ ] Documentation and migration guide complete; no known P0/P1 defect.

## Initial command surface

```text
arandu new <path> [--bin|--lib] [--vcs=auto|git|none]
arandu init [--bin|--lib] [--vcs=auto|git|none]
arandu check [--locked|--offline|--frozen]
arandu build [--profile <name>] [--target <triple>] [--locked|--offline|--frozen]
arandu run [--profile <name>] [--target <triple>] [--locked|--offline|--frozen]
arandu clean
arandu metadata
arandu cache inspect|verify|prune
```

`add/remove/update`, a registry, publishing and general build scripts are not in
the first Gold slice. The schema reserves room for them without pretending they
already exist.

## References

- [Cargo manifests](https://doc.rust-lang.org/cargo/reference/manifest.html),
  [workspaces](https://doc.rust-lang.org/cargo/reference/workspaces.html),
  [build cache](https://doc.rust-lang.org/cargo/reference/build-cache.html),
  [`cargo new`](https://doc.rust-lang.org/cargo/commands/cargo-new.html) and
  [locked/offline/frozen semantics](https://doc.rust-lang.org/cargo/commands/cargo-generate-lockfile.html)
- [Go Modules reference](https://go.dev/ref/mod): module identity, workspaces,
  immutable cache, checksums, private-origin behavior and portable path rules
- [Dart package layout](https://dart.dev/tools/pub/package-layout),
  [lockfile behavior](https://dart.dev/tools/pub/versioning) and
  [`--enforce-lockfile`](https://dart.dev/tools/pub/cmd/pub-get)
- [Zig build system](https://ziglang.org/learn/build-system/) and
  [`build.zig.zon` contract](https://github.com/ziglang/zig/blob/master/doc/build.zig.zon.md)
- [SwiftPM dependency resolution](https://github.com/swiftlang/swift-package-manager/blob/main/Sources/PackageManagerDocs/Documentation.docc/ResolvingPackageVersions.md)
  and [registry integrity model](https://github.com/swiftlang/swift-package-manager/blob/main/Documentation/PackageRegistry/PackageRegistryUsage.md)
- [Cargo build scripts](https://doc.rust-lang.org/cargo/reference/build-scripts.html),
  used here as a warning against ambient dependency code execution
- [Go module authentication and checksum transparency](https://go.dev/ref/mod#authenticating-modules),
  including its private-module fallback/privacy constraints
- [npm provenance](https://docs.npmjs.com/generating-provenance-statements/),
  [trusted publishing](https://docs.npmjs.com/trusted-publishers/) and
  [script policy](https://docs.npmjs.com/cli/update.html#ignore-scripts)
- [Cargo source replacement](https://doc.rust-lang.org/cargo/reference/source-replacement.html)
- [The Update Framework](https://theupdateframework.io/), used as the future
  registry baseline for rollback, freeze and mix-and-match resistance
- [TOML 1.0 specification](https://toml.io/en/v1.0.0) and
  [PEP 518 format comparison](https://peps.python.org/pep-0518/#other-file-formats)
- [Python static project metadata contract](https://packaging.python.org/specifications/declaring-project-metadata/)
- [Go module/package identity](https://go.dev/ref/mod) and
  [file-scoped import bindings](https://go.dev/ref/spec)
- [Swift package products, targets and modules](https://docs.swift.org/swiftpm/documentation/packagemanagerdocs/introducingpackages/)
  plus [package/module visibility](https://docs.swift.org/swift-book/documentation/the-swift-programming-language/accesscontrol/)
- [Rust visibility and public re-exports](https://doc.rust-lang.org/reference/visibility-and-privacy.html)
- [Dart libraries and package imports](https://dart.dev/language/libraries)
- [Node package exports](https://nodejs.org/api/packages.html#package-entry-points),
  used for the explicit public-subpath boundary rather than its runtime loader
