#!/usr/bin/env python3
from __future__ import annotations

import fcntl
import os
import stat
import sys
from pathlib import Path
from typing import Any


DOWNLOADS_ROOT = Path("/tmp/downloads")
MAX_DOWNLOAD_BYTES = 10 * 1024 * 1024
MACOS_F_GETPATH = 50


def _open_file_path(file_fd: int) -> Path:
    """Return the kernel-resolved path for an already-open file descriptor."""
    if sys.platform.startswith("linux"):
        return Path(os.readlink(f"/proc/self/fd/{file_fd}"))
    if sys.platform == "darwin":
        # F_GETPATH returns the path associated with the open descriptor on macOS.
        path_buffer = fcntl.fcntl(file_fd, MACOS_F_GETPATH, bytes(1024))
        path = path_buffer.split(b"\0", 1)[0]
        return Path(os.fsdecode(path))
    raise OSError(f"open file path lookup is unsupported on {sys.platform}")


def _open_artifact(artifact_path: Any) -> int:
    """Open one regular file whose resolved location is below the download root."""
    if not isinstance(artifact_path, str) or not artifact_path:
        raise ValueError("artifact path must be a non-empty string")
    relative_path = Path(artifact_path)
    if (
        relative_path.is_absolute()
        or relative_path == Path(".")
        or ".." in relative_path.parts
        or "\0" in artifact_path
    ):
        raise ValueError("artifact path must be relative to /tmp/downloads")

    root_fd = os.open(
        DOWNLOADS_ROOT,
        os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW,
    )
    file_fd: int | None = None
    try:
        root_path = _open_file_path(root_fd)
        file_fd = os.open(
            artifact_path,
            os.O_RDONLY | os.O_NONBLOCK,
            dir_fd=root_fd,
        )
        file_stat = os.fstat(file_fd)
        if not stat.S_ISREG(file_stat.st_mode):
            raise ValueError("artifact must be a regular file")
        if file_stat.st_size > MAX_DOWNLOAD_BYTES:
            raise ValueError(
                f"artifact exceeds the {MAX_DOWNLOAD_BYTES}-byte size limit"
            )
        file_path = _open_file_path(file_fd)
        try:
            file_path.relative_to(root_path)
        except ValueError as error:
            raise ValueError(
                "artifact path resolves outside /tmp/downloads"
            ) from error
        return file_fd
    except Exception:
        if file_fd is not None:
            os.close(file_fd)
        raise
    finally:
        os.close(root_fd)


def read_artifact(artifact_path: Any) -> bytes:
    file_fd = _open_artifact(artifact_path)
    with os.fdopen(file_fd, "rb") as file:
        contents = file.read(MAX_DOWNLOAD_BYTES + 1)
    if len(contents) > MAX_DOWNLOAD_BYTES:
        raise ValueError(
            f"artifact exceeds the {MAX_DOWNLOAD_BYTES}-byte size limit"
        )
    return contents


def main() -> int:
    if len(sys.argv) != 2:
        print("usage: centaur-artifact-get PATH", file=sys.stderr)
        return 2
    try:
        contents = read_artifact(sys.argv[1])
    except (OSError, ValueError) as error:
        print(f"centaur-artifact-get: {error}", file=sys.stderr)
        return 1
    sys.stdout.buffer.write(contents)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
