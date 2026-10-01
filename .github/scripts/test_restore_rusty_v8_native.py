"""V8 native inputs must be rebuilt outside cached Cargo build-script records."""

import gzip
from pathlib import Path
import tempfile
import unittest

from restore_rusty_v8_native import restore_native_archive


class RestoreNativeArchiveTests(unittest.TestCase):
    def test_verified_archive_is_restored_for_every_release_target(self):
        targets = [
            "aarch64-apple-darwin",
            "x86_64-unknown-linux-gnu",
            "aarch64-unknown-linux-gnu",
            "x86_64-pc-windows-msvc",
            "aarch64-pc-windows-msvc",
        ]
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            archive = root / "verified.gz"
            archive.write_bytes(gzip.compress(b"verified-native-library"))
            for target in targets:
                with self.subTest(target=target):
                    expected_name = (
                        "rusty_v8.lib" if "windows" in target else "librusty_v8.a"
                    )
                    destination = (
                        root / "target" / target / "release/gn_out/obj" / expected_name
                    )
                    destination.parent.mkdir(parents=True)
                    destination.write_bytes(b"stale-cache")
                    self.assertEqual(
                        restore_native_archive(archive, root / "target", target),
                        destination,
                    )
                    self.assertEqual(
                        destination.read_bytes(), b"verified-native-library"
                    )
                    self.assertEqual(list(destination.parent.iterdir()), [destination])

    def test_corrupt_archive_keeps_previous_library(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            archive = root / "corrupt.gz"
            archive.write_bytes(b"not-gzip")
            destination = (
                root
                / "target/x86_64-unknown-linux-gnu/release/gn_out/obj/librusty_v8.a"
            )
            destination.parent.mkdir(parents=True)
            destination.write_bytes(b"previous-library")
            with self.assertRaises(gzip.BadGzipFile):
                restore_native_archive(
                    archive, root / "target", "x86_64-unknown-linux-gnu"
                )
            self.assertEqual(destination.read_bytes(), b"previous-library")
            self.assertEqual(list(destination.parent.iterdir()), [destination])


if __name__ == "__main__":
    unittest.main()
