"""Exercise the fork installer with local archives and isolated homes."""

import io
import os
from pathlib import Path
import subprocess
import tarfile
import tempfile
import unittest

INSTALLER = Path(__file__).resolve().parents[1] / "install.sh"


class InstallerTests(unittest.TestCase):
    def run_installer(self, *, damaged=False, executable=True):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            install = root / "bin"
            mock = root / "mock"
            install.mkdir()
            mock.mkdir()
            old = b"#!/bin/sh\necho old-version\n"
            (install / "codex").write_bytes(old)
            (install / "codex").chmod(0o755)
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
            (mock / "uname").write_text(
                '#!/bin/sh\ncase "$1" in -s) echo Darwin;; -m) echo arm64;; esac\n'
            )
            (mock / "curl").write_text(
                '#!/bin/sh\nwhile [ "$1" != "-o" ]; do shift; done\ncp "$TEST_ARCHIVE" "$2"\n'
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
                "PATH": f"{mock}:{install}:/usr/bin:/bin",
            }
            result = subprocess.run(
                ["/bin/bash", str(INSTALLER), "rust-v0.153.4-ma.3"],
                env=env,
                capture_output=True,
                text=True,
            )
            if damaged or not executable:
                self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertEqual((install / "codex").read_bytes(), old)
                self.assertEqual(sorted(p.name for p in install.iterdir()), ["codex"])
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


if __name__ == "__main__":
    unittest.main()
