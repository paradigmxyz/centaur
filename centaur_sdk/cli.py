"""Shared helpers for tool CLIs."""

from __future__ import annotations

import os
import sys
from typing import Any

import typer


class PipeSafeStdout:
    """Stdout wrapper that treats a reader closing the pipe early as success."""

    def __init__(self, stream: Any):
        self._stream = stream

    def write(self, text: str) -> int:
        try:
            return self._stream.write(text)
        except BrokenPipeError:
            self._discard()
            return len(text)

    def flush(self) -> None:
        try:
            self._stream.flush()
        except BrokenPipeError:
            self._discard()

    def _discard(self) -> None:
        # The reader (e.g. `head`) stopped early. Point stdout at devnull so later
        # writes and the exit-time flush succeed instead of exiting non-zero.
        os.dup2(os.open(os.devnull, os.O_WRONLY), self._stream.fileno())

    def __getattr__(self, name: str) -> Any:
        return getattr(self._stream, name)


class PipeSafeTyper(typer.Typer):
    """Typer app that exits 0 when stdout's reader closes the pipe early.

    Click turns EPIPE into `sys.exit(1)` before a `BrokenPipeError` can reach the
    caller, so `tool --help | head` fails. Wrapping stdout when the app runs as a
    program catches the error at the write instead.
    """

    def __call__(self, *args: Any, **kwargs: Any) -> Any:
        if not isinstance(sys.stdout, PipeSafeStdout):
            sys.stdout = PipeSafeStdout(sys.stdout)
        return super().__call__(*args, **kwargs)
