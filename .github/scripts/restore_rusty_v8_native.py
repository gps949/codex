"""Restore verified rusty_v8 native inputs omitted by Cargo cache cleanup."""

import argparse
import gzip
from pathlib import Path
import shutil
import tempfile


def restore_native_archive(
    archive: Path,
    target_dir: Path,
    target: str,
    *,
    profile: str = "release",
    explicit_target: bool = True,
) -> Path:
    if not target or Path(target).name != target or target in {".", ".."}:
        raise ValueError("target must be a Rust target triple")
    if profile not in {"debug", "release"}:
        raise ValueError("profile must be debug or release")
    library = "rusty_v8.lib" if "windows" in target.split("-") else "librusty_v8.a"
    build_root = target_dir / target if explicit_target else target_dir
    destination = build_root / profile / "gn_out" / "obj" / library
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
    parser.add_argument("--profile", choices=("debug", "release"), default="release")
    parser.add_argument(
        "--host-layout", action="store_true", help="Cargo command has no --target"
    )
    args = parser.parse_args()
    print(
        restore_native_archive(
            args.archive,
            args.target_dir,
            args.target,
            profile=args.profile,
            explicit_target=not args.host_layout,
        )
    )


if __name__ == "__main__":
    main()
