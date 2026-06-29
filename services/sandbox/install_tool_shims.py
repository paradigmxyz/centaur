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

TOOLS_METADATA_NAME = ".centaur-tools-source.json"


def _split_paths(value: str) -> list[Path]:
    return [Path(part) for part in value.split(":") if part]


def _tool_allowlist() -> set[str] | None:
    """Tool package names to install, from ``TOOL_ALLOWLIST``.

    Returns ``None`` when unset/empty -> install every mounted tool (backward
    compatible). When set, only tools whose package name is listed are installed,
    so the sandbox catalog is exactly the configured tools and the agent neither
    sees nor wastes context on unconfigured ones (which also lack credentials).
    """
    raw = os.environ.get("TOOL_ALLOWLIST", "").strip()
    if not raw:
        return None
    return {name.strip() for name in raw.split(",") if name.strip()}


def _tool_blocklist() -> set[str]:
    """Tool package/project/script names to skip, from ``TOOL_BLOCKLIST``."""
    raw = os.environ.get("TOOL_BLOCKLIST", "").strip()
    if not raw:
        return set()
    return {name.strip() for name in raw.split(",") if name.strip()}


def _home_dir() -> Path:
    return Path.home()


def _workspace_dir() -> Path:
    env_workspace = os.environ.get("WORKSPACE_DIR") or os.environ.get("CENTAUR_WORKSPACE_DIR")
    if env_workspace:
        return Path(env_workspace)

    home_dir = _home_dir()
    state_workspace = Path(os.environ.get("CENTAUR_STATE_DIR", str(home_dir / "state"))) / "workspace"
    if os.environ.get("CENTAUR_PERSISTENT_STATE") == "1" or state_workspace.exists():
        return state_workspace
    return home_dir / "workspace"


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


def _sorted_children(path: Path) -> list[Path]:
    return sorted(path.iterdir(), key=lambda child: child.name)


def _remove_path(path: Path) -> None:
    if path.is_dir() and not path.is_symlink():
        shutil.rmtree(path)
    else:
        path.unlink()


def _clear_published_tools(tool_dir: Path) -> None:
    tool_dir.mkdir(parents=True, exist_ok=True)
    for child in _sorted_children(tool_dir):
        if child.name in {".centaur-source", TOOLS_METADATA_NAME} or child.name.startswith(
            ".centaur-source-"
        ):
            continue
        _remove_path(child)


def _copy_published_tools(tool_dir: Path, published: Path) -> None:
    if not published.is_dir():
        raise RuntimeError(f"refreshed tools subdir does not exist: {published}")

    allowlist = _tool_allowlist()
    blocklist = _tool_blocklist()
    existing = {package_dir.name: package_dir for package_dir in _tool_package_dirs(tool_dir)}
    for package_dir in _tool_package_dirs(published):
        tool_name = package_dir.name
        if allowlist is not None and tool_name not in allowlist:
            # Not in TOOL_ALLOWLIST -> don't install; keeps the agent's catalog
            # to configured tools (no phantom, credential-less tools).
            continue
        if tool_name in blocklist:
            continue
        if tool_name in existing:
            print(
                f"skipping duplicate tool {tool_name}: {package_dir} conflicts with {existing[tool_name]}",
                file=sys.stderr,
            )
            continue
        relative_package_dir = package_dir.relative_to(published)
        target = tool_dir / relative_package_dir
        if target.exists() or target.is_symlink():
            _remove_path(target)
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copytree(package_dir, target, symlinks=True)
        existing[tool_name] = target


def _tool_package_dirs(published: Path) -> list[Path]:
    if not published.is_dir():
        return []

    package_dirs: list[Path] = []
    for child in _visible_dirs(published):
        if (child / "pyproject.toml").is_file():
            package_dirs.append(child)
            continue
        for grandchild in _visible_dirs(child):
            if (grandchild / "pyproject.toml").is_file():
                package_dirs.append(grandchild)
    return package_dirs


def _visible_dirs(path: Path) -> list[Path]:
    return [
        child
        for child in _sorted_children(path)
        if child.is_dir()
        and not child.is_symlink()
        and not child.name.startswith(".")
        and not child.name.startswith("_")
    ]


def _publish_tools(tool_dir: Path, published: Path) -> None:
    _clear_published_tools(tool_dir)
    _copy_published_tools(tool_dir, published)


def _source_checkout_path(tool_dir: Path, source_metadata: dict[str, object]) -> Path:
    raw_source_path = source_metadata.get("source_path")
    if not raw_source_path:
        return tool_dir / ".centaur-source"

    source_path = Path(str(raw_source_path))
    if not source_path.is_absolute():
        return tool_dir / source_path
    return source_path


def _copy_refreshed_source(tool_dir: Path, published: Path, label: str) -> None:
    if not published.is_dir():
        print(f"skipping tools source {label}: no tools tree at {published}", file=sys.stderr)
        return
    _copy_published_tools(tool_dir, published)


def _refresh_checkout(source_path: Path, git_ref: object | None) -> None:
    env, temp_dir = _git_env()
    try:
        if git_ref:
            subprocess.run(
                ["git", "-C", str(source_path), "-c", "gc.auto=0", "fetch", "--quiet", "origin", str(git_ref)],
                check=True,
                env=env,
            )
            subprocess.run(
                ["git", "-C", str(source_path), "checkout", "--quiet", "--detach", "FETCH_HEAD"],
                check=True,
                env=env,
            )
        else:
            subprocess.run(
                ["git", "-C", str(source_path), "pull", "--ff-only", "--quiet"],
                check=True,
                env=env,
            )
    finally:
        if temp_dir is not None:
            temp_dir.cleanup()


def _refresh_source(tool_dir: Path, source_metadata: dict[str, object]) -> None:
    subdir = str(source_metadata.get("source_subdir") or "tools")
    if source_metadata.get("source") == "repo_cache":
        repo_cache_repo_path = source_metadata.get("repo_cache_repo_path")
        if not repo_cache_repo_path:
            raise RuntimeError("repo-cache tools metadata is missing repo_cache_repo_path")
        _copy_refreshed_source(
            tool_dir,
            Path(str(repo_cache_repo_path)) / subdir,
            f"{source_metadata.get('repo') or repo_cache_repo_path}:{subdir}",
        )
        return

    source_path = _source_checkout_path(tool_dir, source_metadata)
    if not source_path.is_dir():
        raise RuntimeError(f"git tools source does not exist: {source_path}")

    _refresh_checkout(source_path, source_metadata.get("git_ref"))
    _copy_refreshed_source(
        tool_dir,
        source_path / subdir,
        f"{source_metadata.get('repo') or source_path}:{subdir}",
    )


def _refresh_tool_dir(tool_dir: Path) -> bool:
    metadata_path = tool_dir / TOOLS_METADATA_NAME
    if not metadata_path.is_file():
        return False

    metadata = json.loads(metadata_path.read_text())
    sources = metadata.get("sources")
    if isinstance(sources, list) and sources:
        _clear_published_tools(tool_dir)
        for source_metadata in sources:
            if not isinstance(source_metadata, dict):
                raise RuntimeError(
                    f"invalid tools source metadata in {metadata_path}: {source_metadata!r}"
                )
            _refresh_source(tool_dir, source_metadata)
        return True

    subdir = metadata.get("source_subdir") or "tools"
    if metadata.get("source") == "repo_cache":
        repo_cache_repo_path = metadata.get("repo_cache_repo_path")
        if not repo_cache_repo_path:
            raise RuntimeError(
                f"repo-cache tools metadata is missing repo_cache_repo_path: {metadata_path}"
            )
        _publish_tools(tool_dir, Path(repo_cache_repo_path) / subdir)
        return True

    source = _source_checkout_path(tool_dir, metadata)
    if not source.is_dir():
        return False

    _refresh_checkout(source, metadata.get("git_ref"))
    _publish_tools(tool_dir, source / subdir)
    return True


def _refresh_tool_dirs(tool_dirs: list[Path]) -> int:
    refreshed = 0
    for tool_dir in tool_dirs:
        if _refresh_tool_dir(tool_dir):
            refreshed += 1
    return refreshed


def _copy_skill_dir(skills_src: Path, workspace_skills: Path) -> int:
    if not skills_src.is_dir():
        return 0

    copied = 0
    workspace_skills.mkdir(parents=True, exist_ok=True)
    for skill_entry in sorted(skills_src.iterdir(), key=lambda entry: entry.name):
        skill_name = skill_entry.name
        target = workspace_skills / skill_name
        if target.exists() or target.is_symlink():
            if target.is_dir() and not target.is_symlink():
                shutil.rmtree(target)
            else:
                target.unlink()
        if skill_entry.is_dir() and not skill_entry.is_symlink():
            shutil.copytree(skill_entry, target, symlinks=True)
        else:
            shutil.copy2(skill_entry, target, follow_symlinks=False)
        copied += 1
    return copied


def _first_base_centaur_skills(home_dir: Path) -> Path | None:
    github_dir = home_dir / "github"
    if not github_dir.is_dir():
        return None
    for skills_dir in sorted(github_dir.glob("*/centaur/.agents/skills")):
        if skills_dir.is_dir():
            return skills_dir
    return None


def _skill_sources() -> list[Path]:
    home_dir = _home_dir()
    sources = [
        home_dir / ".agents" / "skills",
        home_dir / "centaur-skills",
    ]
    if base_skills := _first_base_centaur_skills(home_dir):
        sources.append(base_skills)
    sources.append(home_dir / "centaur-overlay-skills")

    overlay_dir = os.environ.get("CENTAUR_OVERLAY_DIR")
    if overlay_dir:
        overlay_tree_skills = Path(overlay_dir) / ".agents" / "skills"
        if overlay_tree_skills.is_dir():
            sources.append(overlay_tree_skills)

    sources.extend(_split_paths(os.environ.get("CENTAUR_SKILL_DIRS", "")))
    return sources


def _refresh_skill_dirs(workspace_dir: Path) -> int:
    workspace_skills = workspace_dir / ".agents" / "skills"
    copied = 0
    for skills_src in _skill_sources():
        copied += _copy_skill_dir(skills_src, workspace_skills)

    if workspace_skills.is_dir():
        claude_dir = workspace_dir / ".claude"
        claude_dir.mkdir(parents=True, exist_ok=True)
        claude_skills = claude_dir / "skills"
        if claude_skills.exists() or claude_skills.is_symlink():
            if claude_skills.is_dir() and not claude_skills.is_symlink():
                shutil.rmtree(claude_skills)
            else:
                claude_skills.unlink()
        claude_skills.symlink_to(workspace_skills)

    return copied


def _discover_scripts(tool_dirs: list[Path]) -> dict[str, dict[str, str]]:
    allowlist = _tool_allowlist()
    blocklist = _tool_blocklist()
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
            # Only shim allowlisted tools (by package dir or project name) so the
            # agent's catalog is exactly the configured tools. Unset -> shim all.
            package_dir = pyproject.parent.name
            project_name = str(project.get("name") or "")
            if (
                allowlist is not None
                and package_dir not in allowlist
                and project_name not in allowlist
            ):
                continue
            if package_dir in blocklist or project_name in blocklist:
                continue
            project_scripts = project.get("scripts") or {}
            if not isinstance(project_scripts, dict):
                continue
            for name in sorted(project_scripts):
                if name in blocklist:
                    continue
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


def _write_tool_shim(path: Path, script: dict[str, str], _pythonpath: str) -> None:
    catalog = path.parent / "centaur-tools"
    content = f"""#!/bin/sh
set -e
exec {shlex.quote(str(catalog))} run {shlex.quote(script["name"])} "$@"
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
from datetime import datetime, timezone
import time

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
import os
from pathlib import Path
import sys

from centaur_sdk.tool_sdk import ToolContext, reset_tool_context, set_tool_context

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

ctx_token = None
thread_key = os.environ.get("CENTAUR_THREAD_KEY", "").strip()
if thread_key:
    ctx_token = set_tool_context(ToolContext(name=project_dir.name, thread_key=thread_key))
try:
    if isinstance(payload, dict):
        result = target(**payload)
    elif payload is None:
        result = target()
    else:
        result = target(payload)
    if inspect.isawaitable(result):
        result = asyncio.run(result)
finally:
    if ctx_token is not None:
        reset_tool_context(ctx_token)
print(json.dumps(result, default=str, separators=(",", ":")))
'''


def tool_env():
    env = os.environ.copy()
    if PYTHONPATH_VALUE:
        if env.get("PYTHONPATH"):
            env["PYTHONPATH"] = f"{{PYTHONPATH_VALUE}}:{{env['PYTHONPATH']}}"
        else:
            env["PYTHONPATH"] = PYTHONPATH_VALUE
    return env


def analytics_log_path():
    configured = os.environ.get("CENTAUR_TOOL_ANALYTICS_LOG_PATH")
    if configured is not None:
        return configured.strip()
    return "/proc/1/fd/2"


def emit_tool_call_event(event, tool, method, started_at=None, returncode=None):
    path = analytics_log_path()
    if path.lower() in {{"", "0", "false", "none", "off"}}:
        return

    payload = {{
        "timestamp": datetime.now(timezone.utc).isoformat(),
        "level": "info",
        "service": "sandbox",
        "component": "tool_shim",
        "event": event,
        "msg": f"Centaur tool shim {{event}}",
        "tool_name": str(tool.get("name") or "unknown"),
        "tool_method": method,
    }}
    thread_key = os.environ.get("CENTAUR_THREAD_KEY", "").strip()
    if thread_key:
        payload["thread_key"] = thread_key
    if started_at is not None:
        payload["duration_ms"] = round((time.monotonic() - started_at) * 1000, 3)
    if returncode is not None:
        payload["exit_code"] = returncode
        payload["success"] = "true" if returncode == 0 else "false"

    try:
        with open(path, "a", encoding="utf-8") as log_file:
            log_file.write(json.dumps(payload, separators=(",", ":"), default=str) + "\\n")
    except Exception:
        pass


def run_tool(tool, args):
    project_dir = Path(tool["project_dir"])
    started_at = time.monotonic()
    emit_tool_call_event("tool_call_started", tool, "cli")
    try:
        returncode = subprocess.call(
            ["uvx", "--from", str(project_dir), tool["name"], *args],
            env=tool_env(),
        )
    except Exception:
        emit_tool_call_event("tool_call_completed", tool, "cli", started_at, 1)
        raise
    emit_tool_call_event("tool_call_completed", tool, "cli", started_at, returncode)
    return returncode


def call_tool(tool, method, payload):
    project_dir = Path(tool["project_dir"])
    client_module = tool.get("client_module", "client.py")
    started_at = time.monotonic()
    emit_tool_call_event("tool_call_started", tool, method)
    try:
        result = subprocess.run(
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
            env=tool_env(),
        )
    except Exception:
        emit_tool_call_event("tool_call_completed", tool, method, started_at, 1)
        raise
    emit_tool_call_event("tool_call_completed", tool, method, started_at, result.returncode)
    return result


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
        return run_tool(by_name[name], argv[3:])
    if command == "call" and len(argv) >= 4:
        # Internal compatibility for Python workflow ctx.call_tool(...). Agents
        # should use direct tool CLIs (`<tool> --help`, `<tool> ...`) instead.
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


def _option_values(argv: list[str], option: str, count: int) -> list[str] | None:
    if option not in argv[1:]:
        return None
    index = argv.index(option)
    if index + count >= len(argv):
        raise RuntimeError(f"{option} requires {count} values")
    return argv[index + 1 : index + 1 + count]


def main(argv: list[str]) -> int:
    refresh = "--refresh" in argv[1:]
    refresh_skills_only = "--refresh-skills" in argv[1:]
    copy_tools_args = _option_values(argv, "--copy-tools", 2)
    tool_dirs = _split_paths(os.environ.get("TOOL_DIRS", ""))

    if copy_tools_args:
        source, target = copy_tools_args
        _copy_published_tools(Path(target), Path(source))
        return 0

    if refresh_skills_only:
        copied = _refresh_skill_dirs(_workspace_dir())
        print(f"reloaded {copied} Centaur skill entries", file=sys.stderr)
        return 0

    bin_dir = Path(os.environ.get("CENTAUR_TOOL_BIN_DIR", str(Path.home() / ".local/bin")))
    bin_dir.mkdir(parents=True, exist_ok=True)

    if refresh:
        refreshed = _refresh_tool_dirs(tool_dirs)
        print(f"refreshed {refreshed} Centaur tool source dirs", file=sys.stderr)
        copied = _refresh_skill_dirs(_workspace_dir())
        print(f"reloaded {copied} Centaur skill entries", file=sys.stderr)

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
