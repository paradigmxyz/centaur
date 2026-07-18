#!/usr/bin/env python3
"""Reject exact duplicate member paths in Helm chart archives."""

from __future__ import annotations

import argparse
from collections import Counter
from pathlib import Path
import sys
import tarfile


def duplicate_members(archive_path: Path) -> list[tuple[str, int]]:
    """Return archive member names that occur more than once."""
    with tarfile.open(archive_path, mode="r:*") as archive:
        counts = Counter(member.name for member in archive.getmembers())
    return sorted((name, count) for name, count in counts.items() if count > 1)


def check_archive(archive_path: Path) -> bool:
    """Print a stable result and return whether the archive is duplicate-free."""
    try:
        duplicates = duplicate_members(archive_path)
    except (OSError, tarfile.TarError) as exc:
        print(f"error: cannot inspect {archive_path}: {exc}", file=sys.stderr)
        return False

    if duplicates:
        print(
            f"error: {archive_path} contains {len(duplicates)} duplicate member path(s):",
            file=sys.stderr,
        )
        for name, count in duplicates:
            print(f"  {count}x {name}", file=sys.stderr)
        return False

    print(f"ok: {archive_path} has no duplicate member paths")
    return True


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Fail when a Helm .tgz contains an exact duplicate tar member path."
    )
    parser.add_argument("archives", nargs="+", type=Path)
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    return 0 if all(check_archive(path) for path in args.archives) else 1


if __name__ == "__main__":
    raise SystemExit(main())
