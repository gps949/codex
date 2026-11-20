#!/usr/bin/env python3
"""Verify a relocated release package without credentials, microphones or backend calls."""

import argparse
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import queue
import shutil
import struct
import subprocess
import sys
import tempfile
import threading

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "third_party/voice"))
from package_runtime import runtime_files


def verify_manifest(package, target, version, commit):
    metadata = json.loads((package / "codex-package.json").read_text())
    suffix = ".exe" if target.endswith("windows-msvc") else ""
    for key, value in {
        "layoutVersion": 1,
        "target": target,
        "version": version,
        "variant": "codex",
        "entrypoint": "bin/codex" + suffix,
        "resourcesDir": "codex-resources",
        "pathDir": "codex-path",
    }.items():
        if metadata.get(key) != value:
            raise ValueError(f"Package metadata mismatch: {key}")
    required = [
        "bin/" + name + suffix
        for name in (
            "codex",
            "codex-code-mode-host",
            "codex-exec-server",
            "codex-responses-api-proxy",
        )
    ]
    required += ["codex-path/rg" + suffix]
    if "linux" in target:
        required += ["codex-resources/bwrap"]
    if suffix:
        required += [
            "codex-resources/" + name + suffix
            for name in (
                "codex-command-runner",
                "codex-windows-sandbox-setup",
                "codex-windows-sandbox-service",
            )
        ]
    for relative in required:
        if not (package / relative).is_file():
            raise ValueError(f"Missing release resource: {relative}")
    voice = package / "codex-resources/voice"
    runtime_files(voice.resolve(), target, public_release=True)
    manifest = json.loads((voice / "manifest.json").read_text())
    for key, value in {
        "buildCommit": commit,
        "appTarget": target,
        "voiceTarget": target,
        "appVersion": version,
    }.items():
        if manifest.get(key) != value:
            raise ValueError(f"Voice manifest mismatch: {key}")
    for relative, expected in manifest["sha256"].items():
        path = PurePosixPath(relative)
        if path.is_absolute() or ".." in path.parts:
            raise ValueError("Unsafe manifest path")
        physical = package / relative
        if physical.is_symlink() or not physical.is_file():
            raise ValueError(f"Missing regular manifest file: {relative}")
        if hashlib.sha256(physical.read_bytes()).hexdigest() != expected:
            raise ValueError(f"Package checksum mismatch: {relative}")
    return suffix


def check_voice(helper, commit, environment):
    environment = dict(environment)
    environment.update(
        {
            "GST_PLUGIN_PATH": "",
            "GST_PLUGIN_PATH_1_0": "",
            "GST_PLUGIN_SYSTEM_PATH": "",
            "GST_PLUGIN_SYSTEM_PATH_1_0": "",
            "GST_REGISTRY": "NUL" if os.name == "nt" else "/dev/null",
            "GST_REGISTRY_UPDATE": "no",
            "GST_REGISTRY_FORK": "no",
        }
    )
    actual = subprocess.check_output(
        [str(helper), "--build-commit"], env=environment, text=True, timeout=30
    ).strip()
    if actual != commit:
        raise ValueError("Voice helper build identity mismatch")
    process = subprocess.Popen(
        [str(helper)],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        env=environment,
    )
    received = queue.Queue()

    def read_frames():
        try:
            while True:
                header = process.stdout.read(4)
                if len(header) != 4:
                    raise ValueError("Voice helper closed before acknowledgement")
                size = struct.unpack(">I", header)[0]
                if size > 128 * 1024:
                    raise ValueError("Oversized Voice helper response")
                received.put(json.loads(process.stdout.read(size)))
        except BaseException as error:
            received.put(error)

    threading.Thread(target=read_frames, daemon=True).start()
    try:
        for request, expected in (
            ({"type": "hello", "protocol": 1, "buildCommit": commit}, "ready"),
            ({"type": "initializeRuntime"}, "runtimeReady"),
            ({"type": "close"}, "closed"),
        ):
            payload = json.dumps(request).encode()
            process.stdin.write(struct.pack(">I", len(payload)) + payload)
            process.stdin.flush()
            response = received.get(timeout=45)
            if isinstance(response, BaseException):
                raise response
            if response != {"type": expected}:
                raise ValueError(f"Unexpected Voice reply for {expected}")
        if process.wait(timeout=15) != 0:
            raise ValueError("Voice helper failed during shutdown")
    finally:
        if process.poll() is None:
            process.kill()
        process.wait(timeout=15)
        process.stdin.close()
        process.stdout.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--package", type=Path, required=True)
    parser.add_argument("--target", required=True)
    parser.add_argument("--version", required=True)
    parser.add_argument("--commit", required=True)
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="fork relocated package ") as directory:
        package = Path(directory) / "package"
        shutil.copytree(args.package, package)
        suffix = verify_manifest(package, args.target, args.version, args.commit)
        environment = {
            key: value
            for key, value in os.environ.items()
            if not key.startswith(("CODEX_", "OPENAI_", "LD_", "DYLD_", "GST_"))
        }
        environment["CODEX_HOME"] = str(Path(directory) / "empty-home")
        Path(environment["CODEX_HOME"]).mkdir()
        cli = package / ("bin/codex" + suffix)
        banner = subprocess.check_output(
            [str(cli), "--version"], env=environment, text=True, timeout=30
        )
        if banner.strip() != f"codex-cli {args.version}":
            raise ValueError("CLI version mismatch")
        for command in (
            [str(cli), "account", "list", "--format", "json"],
            [str(package / ("bin/codex-exec-server" + suffix)), "--help"],
            [str(package / ("bin/codex-code-mode-host" + suffix)), "--help"],
            [str(package / ("codex-path/rg" + suffix)), "--version"],
        ):
            subprocess.run(
                command,
                check=True,
                env=environment,
                stdout=subprocess.DEVNULL,
                timeout=30,
            )
        check_voice(
            package / ("codex-resources/voice/bin/codex-voice-host" + suffix),
            args.commit,
            environment,
        )
    print(
        "Relocated CLI, helpers and Voice runtime passed (no devices or backend opened)."
    )


if __name__ == "__main__":
    main()
