"""Select the latest official stable Rust release from repository tags."""

import re
import sys

STABLE_TAG = re.compile(r"rust-v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)")


def latest_stable_tag(tags: list[str]) -> str:
    versions = {
        tuple(map(int, match.groups())): tag
        for tag in tags
        if (match := STABLE_TAG.fullmatch(tag)) is not None
    }
    if not versions:
        raise ValueError("No official stable Rust release tag found")
    return versions[max(versions)]


if __name__ == "__main__":
    print(latest_stable_tag(sys.stdin.read().splitlines()))
