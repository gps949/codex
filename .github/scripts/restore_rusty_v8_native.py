"""Restore verified rusty_v8 native inputs omitted by Cargo cache cleanup."""

import argparse
import gzip
from pathlib import Path
import shutil
import tempfile


def restore_native_archive(archive: Path, target_dir: Path, target: str) -> Path:
    if not target or Path(target).name != target or target in {".", ".."}:
        raise ValueError("target must be a Rust target triple")
    library = "rusty_v8.lib" if "windows" in target.split("-") else "librusty_v8.a"
    destination = target_dir / target / "release" / "gn_out" / "obj" / library
    destination.parent.mkdir(parents=True, exist_ok=True)
    staged = None
    try:
        with (
            gzip.open(archive, "rb") as source,
            tempfile.NamedTemporaryFile(dir=destination.parent, delete=False) as output,
        ):
            staged = Path(output.name)
            shutil.copyfileobj(source, output, length=1024 * 1024)
        if staged.stat().st_size == 0:
            raise ValueError("verified V8 archive is empty")
        staged.replace(destination)
    finally:
        if staged is not None:
            staged.unlink(missing_ok=True)
    return destination


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--archive", type=Path, required=True)
    parser.add_argument("--target", required=True)
    parser.add_argument("--target-dir", type=Path, default=Path("codex-rs/target"))
    args = parser.parse_args()
    print(restore_native_archive(args.archive, args.target_dir, args.target))


if __name__ == "__main__":
    main()
