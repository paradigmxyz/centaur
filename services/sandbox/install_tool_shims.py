#!/usr/bin/env python3
"""Install shell shims for mounted Centaur tool packages."""

from __future__ import annotations

import json
import os
from pathlib import Path
import shlex
import stat
import sys
import tomllib


def _split_paths(value: str) -> list[Path]:
    return [Path(part) for part in value.split(":") if part]


def _discover_scripts(tool_dirs: list[Path]) -> dict[str, dict[str, str]]:
    scripts: dict[str, dict[str, str]] = {}
    for tool_dir in tool_dirs:
        if not tool_dir.is_dir():
            continue
        for pyproject in sorted(tool_dir.rglob("pyproject.toml")):
            if any(part in {".git", ".venv", "__pycache__"} for part in pyproject.parts):
                continue
            try:
                data = tomllib.loads(pyproject.read_text())
            except (OSError, tomllib.TOMLDecodeError) as exc:
                print(f"warning: failed to read {pyproject}: {exc}", file=sys.stderr)
                continue
            project = data.get("project") or {}
            project_scripts = project.get("scripts") or {}
            if not isinstance(project_scripts, dict):
                continue
            for name in sorted(project_scripts):
                if "/" in name or "\0" in name:
                    print(f"warning: ignoring invalid script name {name!r}", file=sys.stderr)
                    continue
                scripts[name] = {
                    "name": name,
                    "project_dir": str(pyproject.parent),
                    "package": str(project.get("name") or pyproject.parent.name),
                    "entrypoint": str(project_scripts[name]),
                }
    return scripts


def _write_executable(path: Path, content: str) -> None:
    path.write_text(content)
    path.chmod(path.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)


def _write_tool_shim(path: Path, script: dict[str, str], pythonpath: str) -> None:
    content = f"""#!/bin/sh
set -e
_centaur_tool_pythonpath={shlex.quote(pythonpath)}
if [ -n "$_centaur_tool_pythonpath" ]; then
  if [ -n "${{PYTHONPATH:-}}" ]; then
    export PYTHONPATH="$_centaur_tool_pythonpath:$PYTHONPATH"
  else
    export PYTHONPATH="$_centaur_tool_pythonpath"
  fi
fi
exec uvx --from {shlex.quote(script["project_dir"])} {shlex.quote(script["name"])} "$@"
"""
    _write_executable(path, content)


def _write_catalog(path: Path, index_path: Path) -> None:
    content = f"""#!/usr/bin/env python3
from __future__ import annotations

import json
import subprocess
import sys

INDEX = {str(index_path)!r}


def load():
    with open(INDEX) as f:
        return json.load(f)


def usage():
    print("usage: centaur-tools [list|json|which <name>|run <name> [args...]]", file=sys.stderr)
    return 2


def main(argv):
    command = argv[1] if len(argv) > 1 else "list"
    tools = load()
    by_name = {{tool["name"]: tool for tool in tools}}
    if command == "list":
        for tool in tools:
            print(f'{{tool["name"]}}\\t{{tool["project_dir"]}}')
        return 0
    if command == "json":
        print(json.dumps(tools, indent=2, sort_keys=True))
        return 0
    if command == "which" and len(argv) == 3:
        tool = by_name.get(argv[2])
        if not tool:
            print(f"unknown tool: {{argv[2]}}", file=sys.stderr)
            return 1
        print(tool["project_dir"])
        return 0
    if command == "run" and len(argv) >= 3:
        name = argv[2]
        if name not in by_name:
            print(f"unknown tool: {{name}}", file=sys.stderr)
            return 1
        return subprocess.call([name, *argv[3:]])
    return usage()


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
"""
    _write_executable(path, content)


def main() -> int:
    tool_dirs = _split_paths(os.environ.get("TOOL_DIRS", ""))
    bin_dir = Path(os.environ.get("CENTAUR_TOOL_BIN_DIR", str(Path.home() / ".local/bin")))
    bin_dir.mkdir(parents=True, exist_ok=True)

    scripts = _discover_scripts(tool_dirs)
    pythonpath = os.environ.get("CENTAUR_TOOL_PYTHONPATH", "")

    for name, script in scripts.items():
        _write_tool_shim(bin_dir / name, script, pythonpath)

    index_path = bin_dir / ".centaur-tools.json"
    index_path.write_text(json.dumps(list(scripts.values()), indent=2, sort_keys=True) + "\n")
    _write_catalog(bin_dir / "centaur-tools", index_path)
    print(f"installed {len(scripts)} Centaur tool CLI shims into {bin_dir}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
