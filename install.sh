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
  Linux-aarch64) target="aarch64-unknown-linux-gnu" ;;
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
stage=""
lock="$INSTALL_DIR/.codex-install.lock"
lock_owned=0
commit_pending=0
keep_stage=0
new_package=""
cleanup_install() {
  install_status=$?
  trap - EXIT
  trap '' HUP INT TERM
  if [ "$commit_pending" = 1 ]; then
    rollback_failed=0
    for binary in $required; do
      if [ -f "$stage/backup/$binary" ] || [ -L "$stage/backup/$binary" ]; then
        # Keep the backup intact in case another restore fails.
        if ! cp -Pp "$stage/backup/$binary" "$stage/.restore-$binary" ||
           ! mv -f "$stage/.restore-$binary" "$INSTALL_DIR/$binary"; then
          rollback_failed=1
        fi
      elif ! rm -f "$INSTALL_DIR/$binary"; then
        rollback_failed=1
      fi
    done
    if [ "$rollback_failed" = 1 ]; then
      keep_stage=1
      warn "Installation rollback failed. Backups were kept at $stage/backup."
      warn "Restore those backups into $INSTALL_DIR; remove these bundle files if they have no backup: $required."
    fi
  fi
  if [ "$install_status" != 0 ] && [ "$keep_stage" = 0 ] && [ -n "$new_package" ]; then
    rm -rf "$new_package"
  fi
  rm -rf "$tmp"
  if [ -n "$stage" ] && [ "$keep_stage" = 0 ]; then
    rm -rf "$stage"
  fi
  if [ "$lock_owned" = 1 ]; then
    rmdir "$lock" || warn "Could not remove the installation lock: $lock"
  fi
  exit "$install_status"
}
trap cleanup_install EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

say "Downloading $tag ($asset)..."
curl -fsSL "$url" -o "$tmp/$asset"
# New fork releases carry checksums; older releases remain installable.
if curl -fsSL "https://github.com/$REPO/releases/download/$tag/SHA256SUMS" -o "$tmp/SHA256SUMS" 2>/dev/null; then
  expected="$(awk -v asset="$asset" '$2 == asset { print $1 }' "$tmp/SHA256SUMS")"
  if [ -z "$expected" ]; then
    warn "Release checksum is missing for $asset"
    exit 1
  fi
  if command -v sha256sum >/dev/null 2>&1; then
    actual="$(sha256sum "$tmp/$asset" | awk '{print $1}')"
  else
    actual="$(shasum -a 256 "$tmp/$asset" | awk '{print $1}')"
  fi
  if [ "$actual" != "$expected" ]; then
    warn "Release checksum verification failed; your installation was kept."
    exit 1
  fi
fi
# Validate the whole archive before touching an existing installation.
tar tzf "$tmp/$asset" > "$tmp/members"
canonical=0
if grep -Eq '^(\./)?codex-package.json$' "$tmp/members"; then
  canonical=1
  command -v python3 >/dev/null 2>&1 || { warn "Python 3.9 or newer is required to validate this release package."; exit 1; }
  mkdir -p "$INSTALL_DIR"
  stage="$(mktemp -d "$INSTALL_DIR/.codex-install.XXXXXX")"
  python3 - "$tmp/$asset" "$stage/package" "$target" "$tag" <<'PY_PACKAGE'
import hashlib
import json
from pathlib import Path, PurePosixPath
import re
import shutil
import sys
import tarfile

if sys.version_info < (3, 9):
    raise SystemExit("Python 3.9 or newer is required to validate this release package")
archive, output, target, tag = sys.argv[1:]
root = Path(output)
version = tag.removeprefix("rust-v").replace("-ma.", "+ma.")
if not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+\+ma\.[1-9][0-9]*", version):
    raise SystemExit("Invalid fork release version")
with tarfile.open(archive, "r:gz") as bundle:
    members = bundle.getmembers()
    if len(members) > 4096 or sum(item.size for item in members) > 4 * 1024**3:
        raise SystemExit("Release package exceeds size limits")
    names = set()
    for item in members:
        name = item.name.removeprefix("./").rstrip("/")
        if not name or name == ".":
            continue
        path = PurePosixPath(name)
        if (path.is_absolute() or ".." in path.parts or "\\" in name
                or any(ord(char) < 32 for char in name)
                or path.parts[0] not in {"bin", "codex-resources", "codex-path", "codex-package.json"}
                or not (item.isdir() or item.isfile()) or name in names):
            raise SystemExit("Unsafe or duplicate release archive member")
        names.add(name)
    root.mkdir()
    for item in members:
        name = item.name.removeprefix("./").rstrip("/")
        if not name or name == ".":
            continue
        destination = root / name
        if item.isdir():
            destination.mkdir(parents=True, exist_ok=True)
        else:
            destination.parent.mkdir(parents=True, exist_ok=True)
            with bundle.extractfile(item) as source, destination.open("xb") as sink:
                shutil.copyfileobj(source, sink)
            destination.chmod(0o755 if item.mode & 0o111 else 0o644)
# Package directories contain public release resources, never account data.
root.chmod(0o755)
for directory in root.rglob("*"):
    if directory.is_dir():
        directory.chmod(0o755)
metadata = json.loads((root / "codex-package.json").read_text())
for key, value in {"layoutVersion": 1, "version": version, "target": target,
                   "variant": "codex", "entrypoint": "bin/codex",
                   "resourcesDir": "codex-resources", "pathDir": "codex-path"}.items():
    if metadata.get(key) != value:
        raise SystemExit("Release package metadata mismatch: " + key)
required = ["bin/codex", "bin/codex-code-mode-host", "bin/codex-exec-server",
            "bin/codex-responses-api-proxy", "codex-path/rg",
            "codex-resources/voice/bin/codex-voice-host"]
if "linux" in target:
    required.append("codex-resources/bwrap")
for name in required:
    if not (root / name).is_file():
        raise SystemExit("Missing release resource: " + name)
    (root / name).chmod(0o755)
voice = json.loads((root / "codex-resources/voice/manifest.json").read_text())
if voice.get("appVersion") != version or voice.get("appTarget") != target or voice.get("voiceTarget") != target:
    raise SystemExit("Voice package identity mismatch")
if not re.fullmatch(r"[0-9a-f]{40}", voice.get("buildCommit", "")):
    raise SystemExit("Missing Voice build identity")
for name, expected in voice["sha256"].items():
    path = PurePosixPath(name)
    if path.is_absolute() or ".." in path.parts or name not in names:
        raise SystemExit("Unsafe Voice manifest entry")
    if hashlib.sha256((root / name).read_bytes()).hexdigest() != expected:
        raise SystemExit("Voice resource checksum mismatch")
PY_PACKAGE
  "$stage/package/bin/codex" --version
  required="codex"
else
  while IFS= read -r member; do
    case "$member" in
      . | ./ | codex | ./codex | codex-code-mode-host | ./codex-code-mode-host | codex-responses-api-proxy | ./codex-responses-api-proxy | bwrap | ./bwrap) ;;
      *) warn "Unexpected release archive member: $member"; exit 1 ;;
    esac
  done < "$tmp/members"
  mkdir -p "$INSTALL_DIR"
  stage="$(mktemp -d "$INSTALL_DIR/.codex-install.XXXXXX")"
  tar xzf "$tmp/$asset" -C "$stage"
  required="codex codex-code-mode-host codex-responses-api-proxy"
  case "$target" in *linux*) required="$required bwrap" ;; esac
  for binary in $required; do
    if [ ! -f "$stage/$binary" ] || [ -L "$stage/$binary" ]; then
      warn "Release bundle is missing a regular binary: $binary"
      exit 1
    fi
    chmod +x "$stage/$binary"
  done
  "$stage/codex" --version
fi

# Serialize only the commit stage; never remove another installer's lock.
if ! mkdir "$lock" 2>/dev/null; then
  warn "Another installation is active or needs recovery: $lock. Retry after it finishes or is recovered."
  exit 1
fi
lock_owned=1
mkdir "$stage/backup"
for binary in $required; do
  if [ -d "$INSTALL_DIR/$binary" ]; then
    warn "Installation target is a directory: $INSTALL_DIR/$binary"
    exit 1
  fi
  if [ -e "$INSTALL_DIR/$binary" ] || [ -L "$INSTALL_DIR/$binary" ]; then
    if [ ! -f "$INSTALL_DIR/$binary" ] && [ ! -L "$INSTALL_DIR/$binary" ]; then
      warn "Installation target is not a regular file or symlink: $INSTALL_DIR/$binary"
      exit 1
    fi
    cp -Pp "$INSTALL_DIR/$binary" "$stage/backup/$binary"
  fi
done
previous_package=""
if [ "$canonical" = 1 ]; then
  if [ -L "$INSTALL_DIR/codex" ]; then
    prev_target="$(python3 -c 'from pathlib import Path; import sys; p = Path(sys.argv[1]); print(p.resolve())' "$INSTALL_DIR/codex" 2>/dev/null || true)"
    case "$prev_target" in
      "$INSTALL_DIR/.codex-packages/release."*/package/bin/codex)
        previous_package="${prev_target%/package/bin/codex}"
        ;;
    esac
  fi
  mkdir -p "$INSTALL_DIR/.codex-packages"
  chmod 755 "$INSTALL_DIR/.codex-packages"
  package_install="$(mktemp -d "$INSTALL_DIR/.codex-packages/release.XXXXXX")"
  new_package="$package_install"
  chmod 755 "$package_install"
  mv "$stage/package" "$package_install/package"
  package_install="$(cd "$package_install/package" && pwd -P)"
  ln -s "$package_install/bin/codex" "$stage/codex"
fi
commit_pending=1
# Rename on the same filesystem, with the CLI last so helpers are present first.
for binary in $required; do
  [ "$binary" = codex ] || mv -f "$stage/$binary" "$INSTALL_DIR/$binary"
done
mv -f "$stage/codex" "$INSTALL_DIR/codex"
commit_pending=0
# The active generation now belongs to the installation, not failure cleanup.
new_package=""
rmdir "$lock"
lock_owned=0
rm -rf "$stage"
stage=""
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
# Report the standalone updater separately: stale PID files are not process identity.
daemon_output="$("$INSTALL_DIR/codex" app-server daemon stop 2>&1 || true)"
case "$daemon_output" in
  *"not managed"*)
    say "Note: a foreign app-server is running (probably owned by another tool). Leaving it; this build will not reuse it."
    ;;
  *stopped* | *Stopped*)
    say "Stopped a previously running managed app-server daemon."
    say "If you use mobile remote control, start it again with: codex remote-control start"
    ;;
esac
updater_pid_file="${CODEX_HOME:-$HOME/.codex}/app-server-daemon/app-server-updater.pid"
if [ -f "$updater_pid_file" ]; then
  warn "A standalone updater PID file exists at $updater_pid_file."
  warn "Stop that updater through its owning app or service before restarting remote control; a PID file alone cannot safely identify a process."
fi

# --- Prune superseded canonical release generations (keep active + 1 rollback + any live-in-use).
if [ "$canonical" = 1 ] && [ -d "$INSTALL_DIR/.codex-packages" ]; then
  pruned_count="$(python3 - "$INSTALL_DIR" "$previous_package" "${CODEX_KEEP_PREVIOUS_RELEASES:-1}" <<'PY_PRUNE' 2>/dev/null || true
import os
from pathlib import Path
import shutil
import subprocess
import sys

install_dir = Path(sys.argv[1]).resolve()
previous_arg = sys.argv[2]
try:
    keep_previous = max(1, int(sys.argv[3]))
except ValueError:
    keep_previous = 1

if (install_dir / ".codex-install.lock").exists():
    raise SystemExit(0)

packages_dir = install_dir / ".codex-packages"
cli = install_dir / "codex"
if not packages_dir.is_dir() or not cli.is_symlink():
    raise SystemExit(0)

active_bin = cli.resolve()
active_gen = None
for parent in active_bin.parents:
    if parent.parent == packages_dir and parent.name.startswith("release."):
        active_gen = parent
        break
if active_gen is None or not active_gen.exists():
    raise SystemExit(0)

active_mtime = active_gen.stat().st_mtime_ns
keep = {active_gen}
if previous_arg:
    prev_path = Path(previous_arg).resolve()
    if prev_path.parent == packages_dir and prev_path.exists():
        keep.add(prev_path)

candidates = []
for entry in packages_dir.iterdir():
    if entry.is_symlink() or not entry.is_dir() or not entry.name.startswith("release."):
        continue
    resolved = entry.resolve()
    if resolved in keep:
        continue
    stat_info = entry.stat()
    # Never touch generations created after this activation.
    if stat_info.st_mtime_ns > active_mtime:
        continue
    candidates.append((stat_info.st_mtime_ns, entry.name, entry))

# Sort newest-first so we retain enough recent generations to satisfy `keep_previous`.
candidates.sort(key=lambda item: (item[0], item[1]), reverse=True)
already_kept_previous = len(keep) - 1
need_more = max(0, keep_previous - already_kept_previous)
to_prune = [entry for _, _, entry in candidates[need_more:]]

def generation_in_use(gen_dir: Path) -> bool:
    prefix = str(gen_dir.resolve()) + os.sep
    proc = Path("/proc")
    if proc.is_dir():
        for pid_dir in proc.iterdir():
            if not pid_dir.name.isdigit():
                continue
            try:
                exe = os.readlink(pid_dir / "exe")
                if exe.startswith(prefix):
                    return True
            except OSError:
                pass
    lsof = shutil.which("lsof") or ("/usr/sbin/lsof" if Path("/usr/sbin/lsof").is_file() else None)
    if lsof:
        try:
            res = subprocess.run(
                [lsof, "-n", "-P", "+D", str(gen_dir)],
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
                timeout=2,
                check=False,
            )
            if res.returncode == 0:
                return True
        except Exception:
            return True
    return False

pruned = 0
for entry in to_prune:
    if generation_in_use(entry):
        continue
    shutil.rmtree(entry, ignore_errors=True)
    if not entry.exists():
        pruned += 1
if pruned:
    print(pruned)
PY_PRUNE
)"
  if [ -n "$pruned_count" ]; then
    say "Pruned $pruned_count superseded release package(s) from $INSTALL_DIR/.codex-packages."
  fi
fi

say ""
"$INSTALL_DIR/codex" --version
say ""
say "Next steps:"
say "  codex account add --label \"main\"     # add your first ChatGPT subscription"
say "  codex account add --label \"backup\"   # add another; lower priority = preferred"
say "  codex account list"
say "Docs: https://github.com/$REPO/blob/$BRANCH/FORK_MAINTENANCE.md"
say "Note: hooks written for older Codex versions may need updating to the current hook JSON format."
