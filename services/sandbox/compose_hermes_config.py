#!/usr/bin/env python3
"""Compose the Hermes harness config from sandbox state + operator overlay.

Hermes owns its own config file (``$HERMES_HOME/config.yaml``) rather than
reading harness flags, so two things the sandbox knows have to be written into
it at startup:

- **Centaur skills.** ``install-tool-shims --refresh-skills`` copies them into
  ``$WORKSPACE_DIR/.agents/skills``. Hermes scans ``$HERMES_HOME/skills`` plus
  ``skills.external_dirs`` from its config, so without this entry it is the one
  harness that cannot see the deployment's skills.
- **Operator configuration.** ``HERMES_CONFIG_OVERLAY`` carries a YAML (or JSON —
  YAML is a superset) fragment deep-merged over the file, symmetric to
  ``CODEX_CONFIG_OVERLAY`` and ``CLAUDE_SETTINGS_OVERLAY``, so a deployment can
  set ``model.default``, ``providers``, or anything else Hermes reads without
  forking the image.

Rerunnable: the merge preserves whatever Hermes has already written (its own
migrations, learned skills, cron state live in the same file) and de-duplicates
the skills entry, so a sandbox restart against persistent state is a no-op.
"""

from __future__ import annotations

import argparse
import os
import sys
from pathlib import Path
from typing import Any

import yaml


def _deep_merge(base: dict[str, Any], overlay: dict[str, Any]) -> dict[str, Any]:
    for key, value in overlay.items():
        if isinstance(value, dict) and isinstance(base.get(key), dict):
            _deep_merge(base[key], value)
        else:
            base[key] = value
    return base


def _load_config(path: Path) -> dict[str, Any]:
    if not path.is_file():
        return {}
    try:
        loaded = yaml.safe_load(path.read_text(encoding="utf-8"))
    except yaml.YAMLError as exc:
        print(f"ignoring unreadable {path}: {exc}", file=sys.stderr)
        return {}
    return loaded if isinstance(loaded, dict) else {}


def _parse_overlay(raw: str) -> dict[str, Any] | None:
    try:
        overlay = yaml.safe_load(raw)
    except yaml.YAMLError as exc:
        print(f"ignoring invalid HERMES_CONFIG_OVERLAY: {exc}", file=sys.stderr)
        return None
    if overlay is None:
        return None
    if not isinstance(overlay, dict):
        print("ignoring HERMES_CONFIG_OVERLAY: expected a mapping", file=sys.stderr)
        return None
    return overlay


def _add_external_skills_dir(config: dict[str, Any], skills_dir: Path) -> None:
    skills = config.get("skills")
    if not isinstance(skills, dict):
        skills = {}
        config["skills"] = skills
    existing = skills.get("external_dirs")
    dirs = [str(entry) for entry in existing] if isinstance(existing, list) else []
    if str(skills_dir) not in dirs:
        dirs.append(str(skills_dir))
    skills["external_dirs"] = dirs


def compose_hermes_config(
    *,
    hermes_home: Path,
    skills_dir: Path | None = None,
    overlay_raw: str | None = None,
) -> Path:
    config_path = hermes_home / "config.yaml"
    config = _load_config(config_path)

    if skills_dir is not None:
        _add_external_skills_dir(config, skills_dir)

    if overlay_raw and overlay_raw.strip():
        overlay = _parse_overlay(overlay_raw)
        if overlay is not None:
            _deep_merge(config, overlay)

    config_path.parent.mkdir(parents=True, exist_ok=True)
    config_path.write_text(
        yaml.safe_dump(config, sort_keys=False, default_flow_style=False),
        encoding="utf-8",
    )
    return config_path


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--hermes-home", required=True, type=Path)
    parser.add_argument(
        "--skills-dir",
        type=Path,
        default=None,
        help="Directory of Centaur skills to expose as skills.external_dirs.",
    )
    args = parser.parse_args(argv)

    compose_hermes_config(
        hermes_home=args.hermes_home,
        skills_dir=args.skills_dir,
        overlay_raw=os.environ.get("HERMES_CONFIG_OVERLAY"),
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
