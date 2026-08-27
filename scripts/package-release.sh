#!/usr/bin/env bash
# Build a versioned release tarball + BLAKE3 checksum (install gold bar #4).
#
# Output:
#   dist/arandu-$VERSION-$TARGET.tar.gz
#   dist/arandu-$VERSION-$TARGET.tar.gz.blake3   # single-line hex
#
# Tarball root:
#   arandu-$VERSION/
#     bin/arandu_cli, bin/arandu, bin/arandu-lsp
#     bin/arandu → arandu_cli
#     share/arandu/stdlib/
#     release-manifest.json, LICENSE-MIT, LICENSE-APACHE
#     BLAKE3SUMS
#
# Install with: ./scripts/install-from-tarball.sh dist/arandu-….tar.gz

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
VERSION="${VERSION:-}"
TARGET="${TARGET:-$(rustc -vV | sed -n 's/^host: //p')}"
OUT_DIR="${OUT_DIR:-$ROOT/dist}"
SOURCE_DATE_EPOCH="${SOURCE_DATE_EPOCH:-$(git -C "$ROOT" log -1 --format=%ct)}"

if [[ -z "$VERSION" ]]; then
  VERSION="$(python3 - "$ROOT/crates/arandu_cli/Cargo.toml" <<'PY'
from pathlib import Path
import sys
import tomllib

with Path(sys.argv[1]).open("rb") as manifest:
    sys.stdout.write(tomllib.load(manifest)["package"]["version"])
PY
)"
fi
if [[ ! "$VERSION" =~ ^[0-9][0-9A-Za-z.-]*$ ]]; then
  echo "error: invalid release version: $(printf '%q' "$VERSION")" >&2
  exit 2
fi
if [[ ! "$TARGET" =~ ^[A-Za-z0-9_][A-Za-z0-9_.-]*$ ]]; then
  echo "error: invalid release target: $(printf '%q' "$TARGET")" >&2
  exit 2
fi

NAME="arandu-${VERSION}"
ARCHIVE_BASE="arandu-${VERSION}-${TARGET}"
STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT

echo "==> package-release VERSION=$VERSION TARGET=$TARGET"

cargo build --locked -p arandu_cli -p arandu_lsp -p arandu_runtime --release --manifest-path "$ROOT/Cargo.toml"
BIN="$ROOT/target/release/arandu_cli"
LSP="$ROOT/target/release/arandu-lsp"
RUNTIME="$ROOT/target/release/libarandu_runtime.a"

TREE="$STAGE/$NAME"
mkdir -p "$TREE/bin" "$TREE/lib/$TARGET" "$TREE/share/arandu"
install -m 755 "$BIN" "$TREE/bin/arandu_cli"
install -m 755 "$LSP" "$TREE/bin/arandu-lsp"
install -m 644 "$RUNTIME" "$TREE/lib/$TARGET/libarandu_runtime.a"
ln -sfn arandu_cli "$TREE/bin/arandu"
cp -a "$ROOT/stdlib" "$TREE/share/arandu/stdlib"
install -m 644 "$ROOT/LICENSE-MIT" "$ROOT/LICENSE-APACHE" "$TREE/"
python3 - "$TREE/release-manifest.json" "$VERSION" "$TARGET" <<'PY'
import json
from pathlib import Path
import sys

path, version, target = sys.argv[1:]
manifest = {
    "schema": 1,
    "version": version,
    "target": target,
    "components": ["arandu", "arandu-lsp", "runtime", "stdlib"],
    "archive": "tar.gz",
}
Path(path).write_text(
    json.dumps(manifest, indent=2, separators=(",", ": ")) + "\n",
    encoding="utf-8",
    newline="\n",
)
PY

{
  cd "$TREE"
  # shellcheck disable=SC2044
  for f in LICENSE-APACHE LICENSE-MIT bin/arandu_cli bin/arandu-lsp "lib/$TARGET/libarandu_runtime.a" release-manifest.json $(find share/arandu/stdlib -type f -name '*.aru' | sort); do
    hash="$("$BIN" hash-file "$TREE/$f")"
    printf '%s  %s\n' "$hash" "$f"
  done
} >"$TREE/BLAKE3SUMS"

mkdir -p "$OUT_DIR"
TAR="$OUT_DIR/${ARCHIVE_BASE}.tar.gz"
(
  python3 "$ROOT/scripts/reproducible_tar.py" create \
    "$TREE" "$TAR" --epoch "$SOURCE_DATE_EPOCH"
)
python3 "$ROOT/scripts/reproducible_tar.py" validate "$TAR" \
  --root "$NAME" --target "$TARGET" --version "$VERSION"

# Tarball integrity (BLAKE3 of the archive bytes).
HASH="$("$BIN" hash-file "$TAR")"
printf '%s\n' "$HASH" >"${TAR}.blake3"
# Also a "hash  filename" form for convenience.
printf '%s  %s\n' "$HASH" "$(basename "$TAR")" >"${TAR}.blake3sum"
SHA256="$(python3 -c 'import hashlib,sys; print(hashlib.sha256(open(sys.argv[1], "rb").read()).hexdigest())' "$TAR")"
printf '%s\n' "$SHA256" >"${TAR}.sha256"
printf '%s  %s\n' "$SHA256" "$(basename "$TAR")" >"${TAR}.sha256sum"

echo "==> wrote $TAR"
echo "    blake3 $HASH"
echo "    sidecar ${TAR}.blake3"
