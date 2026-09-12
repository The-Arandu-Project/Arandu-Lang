#!/usr/bin/env bash
# Arandu SDK standalone installer.
#
# Usage:
#   curl -sSf https://arandu-lang.dev/install | sh
#   curl -sSf https://raw.githubusercontent.com/arandu-lang/arandu/main/scripts/install.sh | sh
#   ARANDU_VERSION=0.1.0-rc.5 sh install.sh
#
set -euo pipefail

REPO="arandu-lang/arandu"
PREFIX="${PREFIX:-$HOME/.local/arandu}"

echo "==> Arandu installer"

# 1. Detect OS and architecture
OS="$(uname -s)"
ARCH="$(uname -m)"

case "$OS" in
  Linux)
    case "$ARCH" in
      x86_64) TARGET="x86_64-unknown-linux-gnu" ;;
      *)
        echo "error: unsupported Linux architecture: $ARCH" >&2
        exit 1
        ;;
    esac
    ;;
  Darwin)
    case "$ARCH" in
      arm64|aarch64) TARGET="aarch64-apple-darwin" ;;
      *)
        echo "error: unsupported macOS architecture: $ARCH (requires Apple Silicon aarch64)" >&2
        exit 1
        ;;
    esac
    ;;
  *)
    echo "error: unsupported operating system: $OS (on Windows, use install-from-zip.ps1)" >&2
    exit 1
    ;;
esac

# 2. Determine version to install
VERSION="${ARANDU_VERSION:-}"
if [[ -z "$VERSION" ]]; then
  VERSION="0.1.0-rc.5"
fi

VERSION="${VERSION#v}"
TAG="v${VERSION}"

ARCHIVE_NAME="arandu-${VERSION}-${TARGET}.tar.gz"
DOWNLOAD_URL="https://github.com/${REPO}/releases/download/${TAG}/${ARCHIVE_NAME}"
SHA256_URL="${DOWNLOAD_URL}.sha256"

TMPDIR="$(mktemp -d)"
trap 'rm -rf "$TMPDIR"' EXIT

echo "==> downloading Arandu ${VERSION} (${TARGET})"
if command -v curl >/dev/null 2>&1; then
  curl -sSLf -o "$TMPDIR/$ARCHIVE_NAME" "$DOWNLOAD_URL"
  curl -sSLf -o "$TMPDIR/${ARCHIVE_NAME}.sha256" "$SHA256_URL"
elif command -v wget >/dev/null 2>&1; then
  wget -q -O "$TMPDIR/$ARCHIVE_NAME" "$DOWNLOAD_URL"
  wget -q -O "$TMPDIR/${ARCHIVE_NAME}.sha256" "$SHA256_URL"
else
  echo "error: curl or wget is required to download Arandu" >&2
  exit 1
fi

echo "==> verifying SHA-256 checksum"
EXPECTED_SHA256="$(awk '{print $1; exit}' "$TMPDIR/${ARCHIVE_NAME}.sha256")"
if command -v sha256sum >/dev/null 2>&1; then
  ACTUAL_SHA256="$(sha256sum "$TMPDIR/$ARCHIVE_NAME" | awk '{print $1}')"
elif command -v shasum >/dev/null 2>&1; then
  ACTUAL_SHA256="$(shasum -a 256 "$TMPDIR/$ARCHIVE_NAME" | awk '{print $1}')"
elif command -v python3 >/dev/null 2>&1; then
  ACTUAL_SHA256="$(python3 -c 'import hashlib,sys; print(hashlib.sha256(open(sys.argv[1], "rb").read()).hexdigest())' "$TMPDIR/$ARCHIVE_NAME")"
else
  echo "warning: sha256sum not found, skipping checksum verification" >&2
  ACTUAL_SHA256="$EXPECTED_SHA256"
fi

if [[ "$EXPECTED_SHA256" != "$ACTUAL_SHA256" ]]; then
  echo "error: SHA-256 checksum mismatch" >&2
  echo "  expected: $EXPECTED_SHA256" >&2
  echo "  actual:   $ACTUAL_SHA256" >&2
  exit 1
fi
echo "    SHA-256 ok ($ACTUAL_SHA256)"

STAGE="$TMPDIR/extracted"
mkdir -p "$STAGE"
tar -xzf "$TMPDIR/$ARCHIVE_NAME" -C "$STAGE"

VERSION_NAME="arandu-${VERSION}"
VERSION_DIR="$PREFIX/$VERSION_NAME"
STAGE_TREE="$STAGE/$VERSION_NAME"

if [[ ! -d "$STAGE_TREE" ]]; then
  STAGE_TREE="$(find "$STAGE" -mindepth 1 -maxdepth 1 -type d | head -1)"
fi

if [[ ! -x "$STAGE_TREE/bin/arandu" && ! -x "$STAGE_TREE/bin/arandu_cli" ]]; then
  echo "error: corrupt package: binary missing from archive" >&2
  exit 1
fi

echo "==> installing into $PREFIX"
mkdir -p "$PREFIX" "$PREFIX/bin"

if [[ -e "$VERSION_DIR" || -L "$VERSION_DIR" ]]; then
  rm -rf "$VERSION_DIR"
fi
mv "$STAGE_TREE" "$VERSION_DIR"

ln -sfn "$VERSION_NAME" "$PREFIX/current"
ln -sfn "../current/bin/arandu" "$PREFIX/bin/arandu"
ln -sfn "../current/bin/arandu_cli" "$PREFIX/bin/arandu_cli"

echo "==> configuring PATH"
PATH_ENTRY="export PATH=\"$PREFIX/bin:\$PATH\" # Arandu SDK"

add_to_profile() {
  local f="$1"
  if [[ -f "$f" ]]; then
    if grep -Fq "$PREFIX/bin" "$f"; then
      return 0
    fi
    printf '\n%s\n' "$PATH_ENTRY" >>"$f"
    echo "    added to $f"
  fi
}

add_to_profile "$HOME/.zshrc"
add_to_profile "$HOME/.bashrc"
add_to_profile "$HOME/.profile"

echo "==> running doctor"
"$PREFIX/bin/arandu" doctor || true

echo ""
echo "Arandu ${VERSION} successfully installed under ${PREFIX}!"
echo ""
echo "To get started in your current terminal session, run:"
echo "  export PATH=\"$PREFIX/bin:\$PATH\""
echo "  arandu --version"
