# Arandu project/package migration guide

This guide describes the first Gold project contract. A project is rooted at
`arandu.toml`; `src/main.aru` is the default binary entry point and
`arandu.lock` records the exact, content-addressed dependency graph.

## Create or migrate

Run `arandu new <name>` for a new project, or add a manifest with
`arandu init` to an existing source tree. Review the generated manifest and
run `arandu check`. Commit `arandu.lock` whenever the dependency graph
changes.

Use `--locked` in CI and release builds. Use `--offline` only when the
required verified cache entries already exist; `--frozen` combines offline
operation with a read-only lockfile policy.

## Dependencies and trust

The Gold slice accepts local path dependencies and Git dependencies pinned to
an exact commit. `arandu update --accept` is the explicit review boundary for
remote graph changes. Inspect the diff, then run `arandu verify` and
`arandu audit` before merging. Floating branches, arbitrary URLs, registries,
and dependency scripts are intentionally deferred.

The lockfile binds package name, source, commit, manifest fingerprint and
archive digest. A changed origin, rollback, duplicate identity, malformed
archive, path escape, symlink or junction is rejected before compilation.

## Reproducible release workflow

```text
arandu check --locked
arandu build --locked
arandu verify
arandu audit
arandu vendor --locked
```

Builds and installed SDK smoke tests must run on native Windows, Linux and
macOS runners outside the checkout. Caches are disposable: deleting the cache
and repeating the locked build must produce the same graph and artifact
metadata.

## Compatibility boundary

The Gold contract does not promise registry publishing, automatic dependency
scripts, floating version resolution or cross-compiling native artifacts from
an untested host. Those features require a separate threat-model and release
campaign.
