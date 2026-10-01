"""Exercise the fork installer with local archives and isolated homes."""

from contextlib import contextmanager
import hashlib
import io
import os
from pathlib import Path
import stat
import subprocess
import tarfile
import tempfile
import time
import unittest

INSTALLER = Path(__file__).resolve().parents[1] / "install.sh"


class InstallerTests(unittest.TestCase):
    @contextmanager
    def fixture(
        self, *, damaged=False, executable=True, bad_checksum=False, old_bundle="codex"
    ):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            install = root / "bin"
            mock = root / "mock"
            install.mkdir()
            mock.mkdir()
            old_binaries = {
                "codex": b"#!/bin/sh\necho old-version\n",
                "codex-code-mode-host": b"old-code-mode-host",
                "codex-responses-api-proxy": b"old-responses-api-proxy",
            }
            for name, data in old_binaries.items():
                if old_bundle == "empty" or (old_bundle == "codex" and name != "codex"):
                    continue
                path = install / name
                if old_bundle == "symlinks" and name != "codex-code-mode-host":
                    original = root / f"old-{name}"
                    if name == "codex":
                        original.write_bytes(data)
                        original.chmod(0o751)
                    path.symlink_to(original)
                else:
                    path.write_bytes(data)
                    path.chmod(0o751)
            archive = root / "bundle.tar.gz"
            binary = (
                b"#!/bin/sh\necho new-version\n"
                if executable
                else b"#!/bin/sh\nexit 1\n"
            )
            with tarfile.open(archive, "w:gz") as bundle:
                for name, data in [
                    ("codex", binary),
                    ("codex-code-mode-host", b"#!/bin/sh\nexit 0\n"),
                    ("codex-responses-api-proxy", os.urandom(65536)),
                ]:
                    entry = tarfile.TarInfo(name)
                    entry.size = len(data)
                    entry.mode = 0o755
                    bundle.addfile(entry, io.BytesIO(data))
            if damaged:
                archive.write_bytes(archive.read_bytes()[:-4096])
            checksum = root / "SHA256SUMS"
            digest = (
                "0" * 64
                if bad_checksum
                else hashlib.sha256(archive.read_bytes()).hexdigest()
            )
            checksum.write_text(f"{digest}  codex-aarch64-apple-darwin.tar.gz\n")
            (mock / "uname").write_text(
                '#!/bin/sh\ncase "$1" in -s) echo Darwin;; -m) echo arm64;; esac\n'
            )
            (mock / "curl").write_text(
                '#!/bin/sh\nsource="$TEST_ARCHIVE"\nfor arg in "$@"; do case "$arg" in */SHA256SUMS) source="$TEST_CHECKSUM";; esac; done\nwhile [ "$1" != "-o" ]; do shift; done\ncp "$source" "$2"\n'
            )
            (mock / "mv").write_text(
                "#!/bin/bash\n"
                'args=("$@")\n[ "$1" != "-f" ] || shift\n'
                'binary="${1##*/}"\n'
                'if [ "$binary" = "${TEST_BLOCK_BINARY:-}" ]; then\n'
                '  touch "$TEST_MOVE_REACHED"\n'
                '  while [ ! -f "$TEST_MOVE_RELEASE" ]; do sleep 0.01; done\n'
                "fi\n"
                'if [ "$binary" = "${TEST_FAIL_BINARY:-}" ]; then\n'
                '  if [ "${TEST_FAIL_AFTER_MOVE:-0}" = 1 ]; then\n'
                '    /bin/mv "${args[@]}" || exit $?\n'
                "  fi\n"
                '  if [ -n "${TEST_SIGNAL:-}" ]; then\n'
                '    /bin/kill -s "$TEST_SIGNAL" "$PPID"\n'
                "    exit 0\n"
                "  fi\n"
                "  exit 1\n"
                "fi\n"
                'if [ "$binary" = ".restore-${TEST_FAIL_ROLLBACK_BINARY:-}" ]; then\n'
                "  exit 1\n"
                "fi\n"
                'exec /bin/mv "${args[@]}"\n'
            )
            for path in mock.iterdir():
                path.chmod(0o755)
            env = {
                **os.environ,
                "HOME": str(root),
                "CODEX_HOME": str(root / "home"),
                "CODEX_INSTALL_DIR": str(install),
                "CODEX_INSTALL_NO_PATH": "1",
                "TEST_ARCHIVE": str(archive),
                "TEST_CHECKSUM": str(checksum),
                "TEST_MOVE_REACHED": str(root / "move-reached"),
                "TEST_MOVE_RELEASE": str(root / "move-release"),
                "PATH": f"{mock}:{install}:/usr/bin:/bin",
            }
            yield root, install, env, binary

    def command(self):
        return ["/bin/bash", str(INSTALLER), "rust-v0.153.4-ma.3"]

    def run_command(self, env):
        return subprocess.run(
            self.command(), env=env, capture_output=True, text=True, timeout=10
        )

    def snapshot(self, install):
        result = {}
        for path in install.iterdir():
            if path.is_symlink():
                result[path.name] = ("symlink", os.readlink(path))
            elif path.is_dir():
                result[path.name] = ("directory",)
            else:
                result[path.name] = (
                    "file",
                    path.read_bytes(),
                    stat.S_IMODE(path.stat().st_mode),
                )
        return result

    def run_installer(self, *, damaged=False, executable=True, bad_checksum=False):
        with self.fixture(
            damaged=damaged, executable=executable, bad_checksum=bad_checksum
        ) as (_, install, env, binary):
            original = self.snapshot(install)
            result = self.run_command(env)
            if damaged or not executable or bad_checksum:
                self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertEqual(self.snapshot(install), original)
            else:
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertEqual((install / "codex").read_bytes(), binary)
                self.assertTrue((install / "codex-code-mode-host").is_file())

    def test_damaged_archive_preserves_installation(self):
        self.run_installer(damaged=True)

    def test_unusable_binary_preserves_installation(self):
        self.run_installer(executable=False)

    def test_complete_bundle_installs(self):
        self.run_installer()

    def test_checksum_mismatch_preserves_installation(self):
        self.run_installer(bad_checksum=True)

    def test_commit_failure_restores_existing_bundle(self):
        for old_bundle in ("complete", "symlinks"):
            for binary in ("codex-responses-api-proxy", "codex"):
                for after_move in ("0", "1"):
                    with (
                        self.subTest(
                            old_bundle=old_bundle, binary=binary, after_move=after_move
                        ),
                        self.fixture(old_bundle=old_bundle) as (_, install, env, _),
                    ):
                        original = self.snapshot(install)
                        result = self.run_command(
                            {
                                **env,
                                "TEST_FAIL_BINARY": binary,
                                "TEST_FAIL_AFTER_MOVE": after_move,
                            }
                        )
                        self.assertNotEqual(
                            result.returncode, 0, result.stdout + result.stderr
                        )
                        self.assertEqual(self.snapshot(install), original)

    def test_commit_failure_removes_new_bundle_files(self):
        for old_bundle in ("codex", "empty"):
            for binary in ("codex-responses-api-proxy", "codex"):
                with (
                    self.subTest(old_bundle=old_bundle, binary=binary),
                    self.fixture(old_bundle=old_bundle) as (_, install, env, _),
                ):
                    original = self.snapshot(install)
                    result = self.run_command(
                        {**env, "TEST_FAIL_BINARY": binary, "TEST_FAIL_AFTER_MOVE": "1"}
                    )
                    self.assertNotEqual(
                        result.returncode, 0, result.stdout + result.stderr
                    )
                    self.assertEqual(self.snapshot(install), original)

    def test_interrupted_commit_restores_bundle(self):
        for interruption in ("HUP", "INT", "TERM"):
            for old_bundle in ("complete", "empty"):
                with (
                    self.subTest(interruption=interruption, old_bundle=old_bundle),
                    self.fixture(old_bundle=old_bundle) as (_, install, env, _),
                ):
                    original = self.snapshot(install)
                    result = self.run_command(
                        {
                            **env,
                            "TEST_FAIL_BINARY": "codex-responses-api-proxy",
                            "TEST_FAIL_AFTER_MOVE": "1",
                            "TEST_SIGNAL": interruption,
                        }
                    )
                    self.assertNotEqual(
                        result.returncode, 0, result.stdout + result.stderr
                    )
                    self.assertEqual(self.snapshot(install), original)

    def test_concurrent_install_refuses_commit_without_removing_owned_lock(self):
        with self.fixture(old_bundle="complete") as (root, install, env, binary):
            original = self.snapshot(install)
            active = subprocess.Popen(
                self.command(),
                env={**env, "TEST_BLOCK_BINARY": "codex-code-mode-host"},
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
            )
            try:
                deadline = time.monotonic() + 5
                while not (root / "move-reached").exists():
                    self.assertIsNone(
                        active.poll(), "First installer exited before commit"
                    )
                    self.assertLess(time.monotonic(), deadline, "Commit did not start")
                    time.sleep(0.01)
                rejected = self.run_command(env)
                self.assertNotEqual(
                    rejected.returncode, 0, rejected.stdout + rejected.stderr
                )
                self.assertTrue((install / ".codex-install.lock").is_dir())
                self.assertEqual(
                    {name: self.snapshot(install)[name] for name in original}, original
                )
            finally:
                (root / "move-release").touch()
                stdout, stderr = active.communicate(timeout=10)
            self.assertEqual(active.returncode, 0, stdout + stderr)
            self.assertEqual((install / "codex").read_bytes(), binary)
            self.assertEqual(
                sorted(path.name for path in install.iterdir()),
                ["codex", "codex-code-mode-host", "codex-responses-api-proxy"],
            )

    def test_failed_rollback_retains_backups_and_reports_recovery(self):
        with self.fixture(old_bundle="complete") as (_, install, env, _):
            original = self.snapshot(install)
            result = self.run_command(
                {
                    **env,
                    "TEST_FAIL_BINARY": "codex",
                    "TEST_FAIL_ROLLBACK_BINARY": "codex-code-mode-host",
                }
            )
            self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
            stages = list(install.glob(".codex-install.*"))
            self.assertEqual(len(stages), 1)
            self.assertEqual(self.snapshot(stages[0] / "backup"), original)
            self.assertIn(str(stages[0] / "backup"), result.stderr)
            self.assertIn("Restore", result.stderr)
            self.assertFalse((install / ".codex-install.lock").exists())


if __name__ == "__main__":
    unittest.main()
