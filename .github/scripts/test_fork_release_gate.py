"""Release gating must never accept absent, stale, or failed CI."""

import os
from pathlib import Path
import subprocess
import tempfile
import unittest

GATE = Path(__file__).with_name("wait-for-fork-ci.sh")


class ReleaseGateTests(unittest.TestCase):
    def test_gate_checks_exact_commit_and_conclusion(self):
        for response, expected in [
            ("abc completed success", 0),
            ("abc completed failure", 1),
            ("abc completed cancelled", 1),
            ("abc in_progress", 1),
            ("", 1),
            ("old completed success", 1),
        ]:
            with (
                self.subTest(response=response),
                tempfile.TemporaryDirectory() as directory,
            ):
                mock = Path(directory) / "gh"
                mock.write_text('#!/bin/sh\nprintf "%s\\n" "$MOCK_RESULT"\n')
                mock.chmod(0o755)
                env = {
                    **os.environ,
                    "PATH": f"{directory}:/usr/bin:/bin",
                    "MOCK_RESULT": response,
                }
                result = subprocess.run(
                    ["/bin/bash", str(GATE), "gps949/codex", "abc", "1", "0"],
                    env=env,
                    capture_output=True,
                    text=True,
                )
                self.assertEqual(
                    result.returncode, expected, result.stdout + result.stderr
                )


if __name__ == "__main__":
    unittest.main()
