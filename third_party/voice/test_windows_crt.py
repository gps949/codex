"""Exercise CRT staging with read-only Bazel runtime receipts."""

import hashlib
import json
from pathlib import Path
import shutil
import stat
import subprocess
import tempfile
import unittest
from unittest.mock import patch
import zipfile

from windows_crt import stage


class WindowsCrtTests(unittest.TestCase):
    def test_updates_read_only_staged_receipt_without_changing_source(self):
        for target in ("x86_64-pc-windows-msvc", "aarch64-pc-windows-msvc"):
            with self.subTest(target=target), tempfile.TemporaryDirectory() as name:
                root = Path(name)
                source = root / "bazel-runtime.json"
                runtime = root / "staged"
                (runtime / "bin").mkdir(parents=True)
                manifest_path = runtime / "runtime.json"
                original = json.dumps(
                    {"target": target, "developmentOnly": True, "libraries": []}
                )
                source.write_text(original)
                source.chmod(stat.S_IRUSR)
                shutil.copy2(source, manifest_path)
                self.assertFalse(manifest_path.stat().st_mode & stat.S_IWUSR)

                dll = b"pinned retail CRT fixture"
                dll_sha256 = hashlib.sha256(dll).hexdigest()
                member = "retail/vcruntime140.dll"
                archive = root / "crt.zip"
                with zipfile.ZipFile(archive, "w") as output:
                    output.writestr(member, dll)
                (root / "windows-crt.json").write_text(
                    json.dumps(
                        {
                            target: {
                                "url": archive.as_uri(),
                                "sha256": hashlib.sha256(
                                    archive.read_bytes()
                                ).hexdigest(),
                                "member": member,
                                "dllSha256": dll_sha256,
                            }
                        }
                    )
                )
                # Only the external Windows inspection tool is unavailable here.
                inspection = subprocess.CompletedProcess(
                    [], 0, stdout=b"    KERNEL32.dll\n", stderr=b""
                )
                try:
                    with (
                        patch("windows_crt.__file__", str(root / "windows_crt.py")),
                        patch("windows_crt.subprocess.run", return_value=inspection),
                    ):
                        stage(runtime, target, root / "helper.exe")
                    self.assertEqual((runtime / "bin/vcruntime140.dll").read_bytes(), dll)
                    self.assertEqual(
                        json.loads(manifest_path.read_text()),
                        {
                            "target": target,
                            "developmentOnly": True,
                            "libraries": [
                                {
                                    "path": "bin/vcruntime140.dll",
                                    "sha256": dll_sha256,
                                }
                            ],
                        },
                    )
                    self.assertEqual(source.read_text(), original)
                    self.assertFalse(source.stat().st_mode & stat.S_IWUSR)
                finally:
                    # Windows cannot remove read-only fixture files during cleanup.
                    source.chmod(stat.S_IRUSR | stat.S_IWUSR)
                    manifest_path.chmod(stat.S_IRUSR | stat.S_IWUSR)


if __name__ == "__main__":
    unittest.main()
