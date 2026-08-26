#!/usr/bin/env bash
# One-line installer for the multi-account Codex fork.
#
#   curl -fsSL https://raw.githubusercontent.com/gps949/codex/feature/native-multi-account/install.sh | bash
#
# Installs the latest release bundle (codex plus the sibling helper binaries
# it spawns), makes sure it takes precedence over any previously installed
# codex, and cleans up a stale managed app-server daemon. Options:
#   CODEX_INSTALL_DIR=...   target directory (default ~/.local/bin)
#   CODEX_INSTALL_NO_PATH=1 never edit shell profiles, only print guidance
#   first argument           install a specific release tag
set -euo pipefail

REPO="gps949/codex"
BRANCH="feature/native-multi-account"
INSTALL_DIR="${CODEX_INSTALL_DIR:-$HOME/.local/bin}"

say() { printf '%s\n' "$*"; }
warn() { printf 'WARN: %s\n' "$*" >&2; }

case "$(uname -s)-$(uname -m)" in
  Darwin-arm64) target="aarch64-apple-darwin" ;;
  Linux-x86_64) target="x86_64-unknown-linux-gnu" ;;
  *)
    warn "Unsupported platform: $(uname -s)-$(uname -m)."
    warn "Download an asset manually from https://github.com/$REPO/releases (Windows: codex-x86_64-pc-windows-msvc.zip)."
    exit 1
    ;;
esac

tag="${1:-}"
if [ -z "$tag" ]; then
  tag="$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -n 1)"
fi
if [ -z "$tag" ]; then
  warn "Could not determine the latest release tag; pass one explicitly."
  exit 1
fi

asset="codex-$target.tar.gz"
url="https://github.com/$REPO/releases/download/$tag/$asset"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

say "Downloading $tag ($asset)..."
curl -fsSL "$url" -o "$tmp/$asset"
mkdir -p "$INSTALL_DIR"
tar xzf "$tmp/$asset" -C "$INSTALL_DIR"
chmod +x "$INSTALL_DIR"/codex* 2>/dev/null || true
chmod +x "$INSTALL_DIR"/bwrap 2>/dev/null || true
say "Installed into $INSTALL_DIR."

# --- PATH precedence: the fork must win over any previously installed codex.
existing="$(command -v codex 2>/dev/null || true)"
needs_path_entry=1
case ":$PATH:" in
  *":$INSTALL_DIR:"*) needs_path_entry=0 ;;
esac

profile_line="export PATH=\"$INSTALL_DIR:\$PATH\""
maybe_edit_profile() {
  [ "${CODEX_INSTALL_NO_PATH:-0}" = "1" ] && return 1
  case "${SHELL:-}" in
    */zsh) profile="$HOME/.zshrc" ;;
    */bash) profile="$HOME/.bashrc" ;;
    *) return 1 ;;
  esac
  if [ -f "$profile" ] && grep -qF "$INSTALL_DIR" "$profile"; then
    return 0
  fi
  # Prompt via the terminal even when piped through `curl | bash`.
  if [ -r /dev/tty ] && [ -w /dev/tty ]; then
    printf 'Add %s to PATH in %s? [Y/n] ' "$INSTALL_DIR" "$profile" >/dev/tty
    IFS= read -r answer </dev/tty || answer=""
    case "$answer" in
      n* | N*) return 1 ;;
    esac
  fi
  printf '\n# Added by the multi-account Codex fork installer\n%s\n' "$profile_line" >>"$profile"
  say "Added PATH entry to $profile (takes effect in new shells)."
  return 0
}

if [ -z "$existing" ]; then
  if [ "$needs_path_entry" = "1" ]; then
    maybe_edit_profile || say "Add to PATH manually: $profile_line"
  fi
elif [ "$existing" != "$INSTALL_DIR/codex" ]; then
  say ""
  say "Another codex is currently first on PATH: $existing"
  say "  its version: $("$existing" --version 2>/dev/null || echo unknown)"
  case "$existing" in
    *npm* | *node* | *nvm*)
      say "  Looks npm-installed. Remove it with: npm uninstall -g @openai/codex"
      ;;
    *shim* | *cmux*)
      say "  Looks like a tool-managed shim (for example cmux); leave it, the PATH entry below wins in your own shells."
      ;;
  esac
  maybe_edit_profile || say "Make the fork win by putting it first: $profile_line"
fi

# --- Stale managed app-server daemon: an old daemon does not know this
# build's API. Stop it if it is daemon-managed; foreign app-servers (owned by
# other tools) are left alone — this build refuses to reuse mismatched ones.
daemon_output="$("$INSTALL_DIR/codex" app-server daemon stop 2>&1 || true)"
case "$daemon_output" in
  *"not managed"*)
    say "Note: a foreign app-server is running (probably owned by another tool). Leaving it; this build will not reuse it."
    ;;
  *stopped* | *Stopped*)
    say "Stopped a previously running managed app-server daemon."
    say "If you use mobile remote control, start it again with: codex app-server daemon start"
    ;;
esac

say ""
"$INSTALL_DIR/codex" --version
say ""
say "Next steps:"
say "  codex account add --label \"main\"     # add your first ChatGPT subscription"
say "  codex account add --label \"backup\"   # add another; lower priority = preferred"
say "  codex account list"
say "Docs: https://github.com/$REPO/blob/$BRANCH/FORK_MAINTENANCE.md"
say "Note: hooks written for older Codex versions may need updating to the current hook JSON format."
