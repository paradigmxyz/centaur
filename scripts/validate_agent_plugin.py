#!/usr/bin/env python3
"""Validate the cross-client Centaur plugin and marketplace contracts."""

from __future__ import annotations

import json
from pathlib import Path
import re
import sys
from typing import Any


ROOT = Path(__file__).resolve().parents[1]
PLUGIN = Path("plugins/centaur")
NAME = "centaur"
PRIVATE_HOST_SUFFIXES = (".ts.net",)


def _load_json(root: Path, relative: Path, errors: list[str]) -> dict[str, Any]:
    path = root / relative
    try:
        value = json.loads(path.read_text())
    except FileNotFoundError:
        errors.append(f"{relative}: missing file")
        return {}
    except json.JSONDecodeError as exc:
        errors.append(f"{relative}: invalid JSON: {exc}")
        return {}
    if not isinstance(value, dict):
        errors.append(f"{relative}: root must be an object")
        return {}
    return value


def _require(condition: bool, path: Path, message: str, errors: list[str]) -> None:
    if not condition:
        errors.append(f"{path}: {message}")


def _validate_marketplace_source(
    root: Path,
    path: Path,
    source: object,
    errors: list[str],
) -> None:
    source_path = source.get("path") if isinstance(source, dict) else source
    _require(
        source_path == "./plugins/centaur",
        path,
        "source must be ./plugins/centaur",
        errors,
    )
    if source_path == "./plugins/centaur":
        _require(
            (root / source_path).is_dir(),
            path,
            "source directory does not exist",
            errors,
        )


def validate(root: Path = ROOT) -> list[str]:
    """Return all plugin contract violations below *root*."""

    errors: list[str] = []
    codex_path = PLUGIN / ".codex-plugin/plugin.json"
    claude_path = PLUGIN / ".claude-plugin/plugin.json"
    codex_market_path = Path(".agents/plugins/marketplace.json")
    claude_market_path = Path(".claude-plugin/marketplace.json")
    skill_path = PLUGIN / "skills/centaur/SKILL.md"

    codex = _load_json(root, codex_path, errors)
    claude = _load_json(root, claude_path, errors)
    codex_market = _load_json(root, codex_market_path, errors)
    claude_market = _load_json(root, claude_market_path, errors)

    versions = {
        value
        for value in (
            codex.get("version"),
            claude.get("version"),
            claude_market.get("version"),
            (claude_market.get("plugins") or [{}])[0].get("version")
            if isinstance(claude_market.get("plugins"), list)
            and claude_market.get("plugins")
            and isinstance(claude_market["plugins"][0], dict)
            else None,
        )
        if value is not None
    }
    _require(
        len(versions) == 1,
        PLUGIN,
        "manifest and marketplace versions must match",
        errors,
    )
    _require(codex.get("name") == NAME, codex_path, "name must be centaur", errors)
    _require(claude.get("name") == NAME, claude_path, "name must be centaur", errors)
    _require(
        codex_market.get("name") == NAME,
        codex_market_path,
        "marketplace name must be centaur",
        errors,
    )
    _require(
        claude_market.get("name") == NAME,
        claude_market_path,
        "marketplace name must be centaur",
        errors,
    )
    _require(
        "mcpServers" not in codex,
        codex_path,
        "Codex MCP URL must remain deployment-specific",
        errors,
    )

    user_config = claude.get("userConfig")
    mcp_config = claude.get("mcpServers")
    mcp_url = None
    mcp_type = None
    if isinstance(mcp_config, dict) and isinstance(mcp_config.get(NAME), dict):
        mcp_url = mcp_config[NAME].get("url")
        mcp_type = mcp_config[NAME].get("type")
    _require(
        isinstance(user_config, dict)
        and isinstance(user_config.get("mcp_url"), dict)
        and user_config["mcp_url"].get("required") is True,
        claude_path,
        "mcp_url must be required user configuration",
        errors,
    )
    _require(
        mcp_type == "http", claude_path, "Centaur MCP transport must be http", errors
    )
    _require(
        mcp_url == "${user_config.mcp_url}",
        claude_path,
        "MCP URL must come from user_config.mcp_url",
        errors,
    )

    codex_plugins = codex_market.get("plugins")
    _require(
        isinstance(codex_plugins, list) and len(codex_plugins) == 1,
        codex_market_path,
        "marketplace must contain exactly one plugin",
        errors,
    )
    if (
        isinstance(codex_plugins, list)
        and codex_plugins
        and isinstance(codex_plugins[0], dict)
    ):
        entry = codex_plugins[0]
        _require(
            entry.get("name") == NAME,
            codex_market_path,
            "plugin name must be centaur",
            errors,
        )
        _validate_marketplace_source(
            root, codex_market_path, entry.get("source"), errors
        )
        policy = entry.get("policy")
        _require(
            isinstance(policy, dict)
            and policy.get("installation") == "AVAILABLE"
            and policy.get("authentication") == "ON_INSTALL",
            codex_market_path,
            "plugin policy must declare availability and authentication timing",
            errors,
        )

    claude_plugins = claude_market.get("plugins")
    _require(
        isinstance(claude_plugins, list) and len(claude_plugins) == 1,
        claude_market_path,
        "marketplace must contain exactly one plugin",
        errors,
    )
    if (
        isinstance(claude_plugins, list)
        and claude_plugins
        and isinstance(claude_plugins[0], dict)
    ):
        entry = claude_plugins[0]
        _require(
            entry.get("name") == NAME,
            claude_market_path,
            "plugin name must be centaur",
            errors,
        )
        _validate_marketplace_source(
            root, claude_market_path, entry.get("source"), errors
        )

    try:
        skill = (root / skill_path).read_text()
    except FileNotFoundError:
        errors.append(f"{skill_path}: missing file")
        skill = ""
    _require(
        bool(re.search(r"(?m)^name:\s*centaur\s*$", skill)),
        skill_path,
        "frontmatter name must be centaur",
        errors,
    )
    for invariant in (
        "centaur_whoami",
        "method",
        "arguments",
        "codex mcp add centaur --url",
    ):
        _require(
            invariant in skill,
            skill_path,
            f"missing operational invariant: {invariant}",
            errors,
        )

    artifact_paths = [
        codex_path,
        claude_path,
        codex_market_path,
        claude_market_path,
        skill_path,
        PLUGIN / "README.md",
    ]
    for relative in artifact_paths:
        path = root / relative
        if not path.is_file():
            continue
        text = path.read_text().lower()
        for suffix in PRIVATE_HOST_SUFFIXES:
            _require(
                suffix not in text,
                relative,
                f"must not contain private host suffix {suffix}",
                errors,
            )

    return errors


def main() -> int:
    errors = validate()
    if errors:
        print("Agent plugin validation failed:", file=sys.stderr)
        for error in errors:
            print(f"- {error}", file=sys.stderr)
        return 1
    print("Agent plugin validation passed.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
