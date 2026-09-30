"""Stable discovery excludes prereleases, fork releases and malformed historical tags."""

import unittest

from latest_upstream_stable_tag import latest_stable_tag


class LatestStableTagTests(unittest.TestCase):
    def test_ignores_fork_prerelease_and_malformed_tags(self):
        tags = [
            "rust-v0.154.0",
            "rust-v0.159.2",
            "rust-v0.159.2-ma.8",
            "rust-v0.160.0-alpha.1",
            "rust-v0.160.0-beta.1",
            "rust-v0.160.0-rc.1",
            "rust-v.0.0.2504292236",
            "rust-vrust-v0.999.0",
            "rust-v0.999.0+dev",
        ]
        self.assertEqual(latest_stable_tag(tags), "rust-v0.159.2")

    def test_compares_version_components_numerically(self):
        self.assertEqual(
            latest_stable_tag(["rust-v0.9.9", "rust-v0.10.0", "rust-v0.9.100"]),
            "rust-v0.10.0",
        )

    def test_fails_when_no_official_stable_tag_exists(self):
        with self.assertRaisesRegex(ValueError, "No official stable"):
            latest_stable_tag(["rust-v0.159.2-ma.1", "rust-v0.160.0-alpha.1"])


if __name__ == "__main__":
    unittest.main()
