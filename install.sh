#!/usr/bin/env bash
# One-line installer for the multi-account Codex fork.
#
#   curl -fsSL https://raw.githubusercontent.com/gps949/codex/feature/native-multi-account/install.sh | bash
#
# Downloads the latest GitHub release bundle for this platform (codex plus the
# sibling helper binaries it spawns) into one directory. Override the target
# directory with CODEX_INSTALL_DIR, or pass a release tag as the first
# argument to install a specific version.
set -euo pipefail

REPO="gps949/codex"
INSTALL_DIR="${CODEX_INSTALL_DIR:-$HOME/.local/bin}"

case "$(uname -s)-$(uname -m)" in
  Darwin-arm64) target="aarch64-apple-darwin" ;;
  Linux-x86_64) target="x86_64-unknown-linux-gnu" ;;
  *)
    echo "Unsupported platform: $(uname -s)-$(uname -m)." >&2
    echo "Download an asset manually from https://github.com/$REPO/releases (Windows: codex-x86_64-pc-windows-msvc.zip)." >&2
    exit 1
    ;;
esac

tag="${1:-}"
if [ -z "$tag" ]; then
  tag="$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -n 1)"
fi
if [ -z "$tag" ]; then
  echo "Could not determine the latest release tag; pass one explicitly." >&2
  exit 1
fi

asset="codex-$target.tar.gz"
url="https://github.com/$REPO/releases/download/$tag/$asset"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

echo "Downloading $tag ($asset)..."
curl -fsSL "$url" -o "$tmp/$asset"
mkdir -p "$INSTALL_DIR"
tar xzf "$tmp/$asset" -C "$INSTALL_DIR"
chmod +x "$INSTALL_DIR"/codex* "$INSTALL_DIR"/bwrap 2>/dev/null || true

echo "Installed to $INSTALL_DIR:"
tar tzf "$tmp/$asset" | sed 's/^\.\///; s/^/  /'
case ":$PATH:" in
  *":$INSTALL_DIR:"*) ;;
  *)
    echo
    echo "$INSTALL_DIR is not on your PATH. Add this to your shell profile:"
    echo "  export PATH=\"$INSTALL_DIR:\$PATH\""
    ;;
esac
echo
"$INSTALL_DIR/codex" --version
