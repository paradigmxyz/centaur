#!/usr/bin/env python3
"""Install shell shims for mounted Centaur tool packages."""

from __future__ import annotations

import json
import os
from pathlib import Path
import shlex
import shutil
import stat
import subprocess
import sys
import tempfile
import tomllib


def _split_paths(value: str) -> list[Path]:
    return [Path(part) for part in value.split(":") if part]


def _git_env() -> tuple[dict[str, str], tempfile.TemporaryDirectory[str] | None]:
    env = os.environ.copy()
    env["GIT_TERMINAL_PROMPT"] = "0"
    token_file = os.environ.get("CENTAUR_TOOLS_GITHUB_TOKEN_FILE")
    if not token_file:
        return env, None
    temp_dir = tempfile.TemporaryDirectory(prefix="centaur-tools-askpass-")
    askpass = Path(temp_dir.name) / "askpass.sh"
    askpass.write_text(
        "#!/bin/sh\n"
        "case \"$1\" in\n"
        "  *Username*) echo x-access-token;;\n"
        f"  *Password*) cat {shlex.quote(token_file)};;\n"
        "  *) echo;;\n"
        "esac\n"
    )
    askpass.chmod(0o700)
    env["GIT_ASKPASS"] = str(askpass)
    return env, temp_dir


def _publish_tools(tool_dir: Path, published: Path) -> None:
    if not published.is_dir():
        raise RuntimeError(f"refreshed tools subdir does not exist: {published}")

    for child in tool_dir.iterdir():
        if child.name in {".centaur-source", ".centaur-tools-source.json"}:
            continue
        if child.is_dir() and not child.is_symlink():
            shutil.rmtree(child)
        else:
            child.unlink()

    for child in published.iterdir():
        target = tool_dir / child.name
        if child.is_dir() and not child.is_symlink():
            shutil.copytree(child, target, symlinks=True)
        else:
            shutil.copy2(child, target, follow_symlinks=False)


def _refresh_tool_dir(tool_dir: Path) -> bool:
    source = tool_dir / ".centaur-source"
    metadata_path = tool_dir / ".centaur-tools-source.json"
    if not metadata_path.is_file():
        return False

    metadata = json.loads(metadata_path.read_text())
    subdir = metadata.get("source_subdir") or "tools"
    if metadata.get("source") == "repo_cache":
        repo_cache_repo_path = metadata.get("repo_cache_repo_path")
        if not repo_cache_repo_path:
            raise RuntimeError(f"repo-cache tools metadata is missing repo_cache_repo_path: {metadata_path}")
        _publish_tools(tool_dir, Path(repo_cache_repo_path) / subdir)
        return True

    if not source.is_dir():
        return False

    git_ref = metadata.get("git_ref")
    env, temp_dir = _git_env()
    try:
        if git_ref:
            subprocess.run(
                ["git", "-C", str(source), "-c", "gc.auto=0", "fetch", "--quiet", "origin", str(git_ref)],
                check=True,
                env=env,
            )
            subprocess.run(
                ["git", "-C", str(source), "checkout", "--quiet", "--detach", "FETCH_HEAD"],
                check=True,
                env=env,
            )
        else:
            subprocess.run(
                ["git", "-C", str(source), "pull", "--ff-only", "--quiet"],
                check=True,
                env=env,
            )
    finally:
        if temp_dir is not None:
            temp_dir.cleanup()

    _publish_tools(tool_dir, source / subdir)
    return True


def _refresh_tool_dirs(tool_dirs: list[Path]) -> int:
    refreshed = 0
    for tool_dir in tool_dirs:
        if _refresh_tool_dir(tool_dir):
            refreshed += 1
    return refreshed


def _discover_scripts(tool_dirs: list[Path]) -> dict[str, dict[str, str]]:
    scripts: dict[str, dict[str, str]] = {}
    for tool_dir in tool_dirs:
        if not tool_dir.is_dir():
            continue
        for pyproject in sorted(tool_dir.rglob("pyproject.toml")):
            if any(
                part in {".centaur-source", ".git", ".venv", "__pycache__"}
                for part in pyproject.parts
            ):
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
                    "client_module": str(
                        ((data.get("tool") or {}).get("centaur") or {}).get("module")
                        or "client.py"
                    ),
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


def _write_catalog(path: Path, index_path: Path, pythonpath: str) -> None:
    content = f"""#!/usr/bin/env python3
from __future__ import annotations

import json
import os
from pathlib import Path
import subprocess
import sys

INDEX = {str(index_path)!r}
PYTHONPATH_VALUE = {pythonpath!r}


def load():
    with open(INDEX) as f:
        return json.load(f)


def usage():
    print("usage: centaur-tools [list|json|refresh|which <name>|run <name> [args...]|call <name> <method> [json]]", file=sys.stderr)
    return 2


CALL_RUNNER = r'''
import asyncio
import importlib
import importlib.util
import inspect
import json
from pathlib import Path
import sys

project_dir = Path(sys.argv[1])
client_module = sys.argv[2]
method = sys.argv[3]
payload = json.loads(sys.argv[4])

module_path = project_dir / client_module
package_name = project_dir.name.replace("-", "_")
if (project_dir / "__init__.py").is_file() and package_name.isidentifier() and module_path.suffix == ".py":
    parent = str(project_dir.parent)
    if parent not in sys.path:
        sys.path.insert(0, parent)
    module = importlib.import_module(f"{{package_name}}.{{module_path.stem}}")
else:
    spec = importlib.util.spec_from_file_location("_centaur_tool_client", module_path)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"cannot load client module from {{module_path}}")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)

target = getattr(module, method, None)
if target is None and hasattr(module, "_client"):
    target = getattr(module._client(), method, None)
if target is None:
    raise RuntimeError(f"tool has no method {{method}}")

if isinstance(payload, dict):
    result = target(**payload)
elif payload is None:
    result = target()
else:
    result = target(payload)
if inspect.isawaitable(result):
    result = asyncio.run(result)
print(json.dumps(result, default=str, separators=(",", ":")))
'''


def call_tool(tool, method, payload):
    project_dir = Path(tool["project_dir"])
    client_module = tool.get("client_module", "client.py")
    env = os.environ.copy()
    if PYTHONPATH_VALUE:
        if env.get("PYTHONPATH"):
            env["PYTHONPATH"] = f"{{PYTHONPATH_VALUE}}:{{env['PYTHONPATH']}}"
        else:
            env["PYTHONPATH"] = PYTHONPATH_VALUE
    return subprocess.run(
        [
            "uvx",
            "--from",
            str(project_dir),
            "python",
            "-c",
            CALL_RUNNER,
            str(project_dir),
            client_module,
            method,
            json.dumps(payload, separators=(",", ":")),
        ],
        check=False,
        text=True,
        capture_output=True,
        env=env,
    )


def main(argv):
    command = argv[1] if len(argv) > 1 else "list"
    if command == "refresh":
        return subprocess.call(["install-tool-shims", "--refresh"])
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
    if command == "call" and len(argv) >= 4:
        name = argv[2]
        method = argv[3]
        if name not in by_name:
            print(f"unknown tool: {{name}}", file=sys.stderr)
            return 1
        try:
            payload = json.loads(argv[4]) if len(argv) >= 5 else {{}}
            result = call_tool(by_name[name], method, payload)
            if result.stdout:
                print(result.stdout, end="")
            if result.returncode != 0:
                if result.stderr:
                    print(result.stderr, file=sys.stderr, end="")
                return result.returncode
            return 0
        except Exception as exc:
            print(str(exc), file=sys.stderr)
            return 1
    return usage()


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
"""
    _write_executable(path, content)


def main(argv: list[str]) -> int:
    refresh = "--refresh" in argv[1:]
    tool_dirs = _split_paths(os.environ.get("TOOL_DIRS", ""))
    bin_dir = Path(os.environ.get("CENTAUR_TOOL_BIN_DIR", str(Path.home() / ".local/bin")))
    bin_dir.mkdir(parents=True, exist_ok=True)

    if refresh:
        refreshed = _refresh_tool_dirs(tool_dirs)
        print(f"refreshed {refreshed} Centaur tool source dirs", file=sys.stderr)

    scripts = _discover_scripts(tool_dirs)
    pythonpath_parts = [
        part for part in os.environ.get("CENTAUR_TOOL_PYTHONPATH", "").split(os.pathsep) if part
    ]
    sdk_parent = Path("/opt/centaur")
    if (sdk_parent / "centaur_sdk").is_dir() and str(sdk_parent) not in pythonpath_parts:
        pythonpath_parts.append(str(sdk_parent))
    pythonpath = os.pathsep.join(pythonpath_parts)

    for name, script in scripts.items():
        _write_tool_shim(bin_dir / name, script, pythonpath)

    index_path = bin_dir / ".centaur-tools.json"
    index_path.write_text(json.dumps(list(scripts.values()), indent=2, sort_keys=True) + "\n")
    _write_catalog(bin_dir / "centaur-tools", index_path, pythonpath)
    # stdout is reserved for harness JSONL output (the session stdout pump streams
    # it to clients); write bootstrap notices to stderr so they never pollute it.
    print(f"installed {len(scripts)} Centaur tool CLI shims into {bin_dir}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
