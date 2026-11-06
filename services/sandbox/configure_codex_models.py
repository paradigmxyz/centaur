#!/usr/bin/env python3
"""Compose native Codex model metadata from the deployment's provider catalog."""

import json
import os
import subprocess
import sys
import tomllib
from pathlib import Path

import tomli_w


def native_model(model_id: str, metadata: dict) -> dict:
    efforts = metadata.get("reasoningEfforts", [])
    model = {
        "slug": model_id,
        "display_name": metadata.get("displayName", model_id),
        "description": metadata.get("description", "Custom Responses model"),
        "default_reasoning_level": metadata.get("defaultReasoning"),
        "supported_reasoning_levels": [
            {"effort": effort, "description": effort.capitalize()} for effort in efforts
        ],
        "shell_type": "unified_exec",
        "visibility": "list",
        "supported_in_api": True,
        "priority": 100,
        "support_verbosity": False,
        "truncation_policy": {"mode": "tokens", "limit": 10000},
        "experimental_supported_tools": [],
        "additional_speed_tiers": [],
        "service_tiers": [],
        "use_responses_lite": False,
    }
    if "contextWindow" in metadata:
        window = metadata["contextWindow"]
        if type(window) is not int or window <= 0:
            raise ValueError(f"invalid contextWindow for model {model_id!r}")
        model["context_window"] = window
        model["max_context_window"] = window
    if "inputModalities" in metadata:
        modalities = metadata["inputModalities"]
        if (
            not isinstance(modalities, list)
            or "text" not in modalities
            or any(modality not in {"text", "image"} for modality in modalities)
        ):
            raise ValueError(f"invalid inputModalities for model {model_id!r}")
        model["input_modalities"] = sorted(set(modalities))
    return model


def custom_models(providers: dict) -> list[dict]:
    models = {}
    for provider in providers.values():
        for model_id, metadata in provider.get("models", {}).items():
            model = native_model(model_id, metadata)
            if model_id in models and models[model_id] != model:
                raise ValueError(f"conflicting native metadata for model {model_id!r}")
            models[model_id] = model
    return list(models.values())


def configure(
    config_path: Path,
    providers: dict,
    codex_bin: str = "codex",
    instructions_path: Path = Path("/etc/centaur/codex-base-instructions.md"),
) -> None:
    config = tomllib.loads(config_path.read_text())
    if config.get("model_catalog_json"):
        return
    models = custom_models(providers)
    if not models:
        return
    bundled = json.loads(
        subprocess.check_output(
            [codex_bin, "debug", "models", "--bundled"], text=True, timeout=30
        )
    )
    bundled_ids = {model["slug"] for model in bundled["models"]}
    collisions = bundled_ids.intersection(model["slug"] for model in models)
    if collisions:
        raise ValueError(
            f"custom model IDs conflict with bundled models: {sorted(collisions)}"
        )
    instructions = instructions_path.read_text()
    if not instructions.strip():
        raise ValueError("Codex base instructions must not be empty")
    for model in models:
        model["base_instructions"] = instructions
    bundled["models"].extend(models)
    catalog_path = config_path.with_name("centaur-models.json")
    temporary = catalog_path.with_suffix(".tmp")
    temporary.write_text(json.dumps(bundled))
    temporary.replace(catalog_path)
    config["model_catalog_json"] = str(catalog_path)
    config_path.write_text(tomli_w.dumps(config))


def main() -> None:
    raw = os.environ.get("CODEX_CUSTOM_PROVIDERS", "").strip()
    if raw:
        configure(
            Path(sys.argv[1]), json.loads(raw), os.environ.get("CODEX_BIN", "codex")
        )


if __name__ == "__main__":
    main()
