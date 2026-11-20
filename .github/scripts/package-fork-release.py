#!/usr/bin/env python3
"""Assemble the canonical fork package with same-commit Voice resources."""

import argparse
from pathlib import Path
import shutil
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "third_party/voice"))
from assemble_package import assemble


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", required=True)
    parser.add_argument("--release-dir", type=Path, required=True)
    parser.add_argument("--helper", type=Path, required=True)
    parser.add_argument("--runtime", type=Path, required=True)
    parser.add_argument("--version", required=True)
    parser.add_argument("--commit", required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    suffix = ".exe" if args.target.endswith("windows-msvc") else ""
    base = args.output.with_name(args.output.name + "-base")
    cmd = [
        sys.executable,
        str(ROOT / "scripts/build_codex_package.py"),
        "--target",
        args.target,
        "--package-version",
        args.version,
        "--package-dir",
        str(base),
        "--strip",
        "none",
    ]
    for option, binary in (
        ("--entrypoint-bin", "codex"),
        ("--code-mode-host-bin", "codex-code-mode-host"),
        ("--exec-server-bin", "codex-exec-server"),
    ):
        cmd.extend((option, str(args.release_dir / (binary + suffix))))
    if "linux" in args.target:
        cmd.extend(("--bwrap-bin", str(args.release_dir / "bwrap")))
    if suffix:
        for option, binary in (
            ("--codex-command-runner-bin", "codex-command-runner"),
            ("--codex-windows-sandbox-setup-bin", "codex-windows-sandbox-setup"),
        ):
            cmd.extend((option, str(args.release_dir / (binary + suffix))))
    subprocess.run(cmd, check=True)
    shutil.copy2(
        args.release_dir / ("codex-responses-api-proxy" + suffix), base / "bin"
    )
    if suffix:
        shutil.copy2(
            args.release_dir / "codex-windows-sandbox-service.exe",
            base / "codex-resources",
        )
    assemble(
        base,
        args.helper,
        args.target,
        args.commit,
        args.output,
        runtime=args.runtime,
        release_version=args.version,
    )
    print(args.output)


if __name__ == "__main__":
    main()
