"""Render bounded, self-contained Vega-Lite specifications."""

from __future__ import annotations

import base64
import copy
import json
from collections.abc import Mapping
from typing import Any

import vl_convert as vlc

VEGA_LITE_VERSION = "6.4"
MAX_SPEC_BYTES = 2 * 1024 * 1024
MAX_INLINE_VALUES = 50_000
MAX_DIMENSION = 1_600
MAX_SCALE = 3.0

_DEFAULT_CONFIG: dict[str, Any] = {
    "font": "Inter, IBM Plex Sans, Source Sans 3, DejaVu Sans, Arial, sans-serif",
    "background": "#F8F9FA",
    "view": {
        "continuousWidth": 800,
        "continuousHeight": 450,
        "stroke": None,
    },
    "mark": {"color": "#0072B2"},
    "line": {"strokeWidth": 2},
    "point": {"filled": True, "size": 50},
    "axis": {
        "domainColor": "#9CA3AF",
        "domainWidth": 0.8,
        "gridColor": "#E5E7EB",
        "gridWidth": 0.6,
        "labelColor": "#4B5563",
        "labelFontSize": 9.5,
        "labelPadding": 6,
        "tickColor": "#9CA3AF",
        "tickWidth": 0.8,
        "titleColor": "#374151",
        "titleFontSize": 10.5,
        "titleFontWeight": "normal",
        "titlePadding": 10,
    },
    "axisX": {"grid": False, "tickSize": 4, "ticks": True},
    "axisY": {"domain": True, "grid": True, "ticks": False},
    "legend": {
        "labelColor": "#4B5563",
        "labelFontSize": 9.5,
        "titleColor": "#374151",
        "titleFontSize": 10.5,
        "titleFontWeight": "normal",
    },
    "title": {
        "anchor": "start",
        "color": "#111827",
        "fontSize": 13,
        "fontWeight": 600,
        "offset": 12,
        "subtitleColor": "#4B5563",
        "subtitleFontSize": 10,
        "subtitleFontWeight": "normal",
        "subtitlePadding": 6,
    },
    "range": {
        "category": [
            "#0072B2",
            "#D55E00",
            "#009E73",
            "#CC79A7",
            "#F0E442",
            "#56B4E9",
            "#E69F00",
            "#000000",
        ]
    },
}


def _deep_merge(defaults: Mapping[str, Any], overrides: Mapping[str, Any]) -> dict[str, Any]:
    merged = copy.deepcopy(dict(defaults))
    for key, value in overrides.items():
        if isinstance(value, Mapping) and isinstance(merged.get(key), Mapping):
            merged[key] = _deep_merge(merged[key], value)
        else:
            merged[key] = copy.deepcopy(value)
    return merged


def _inspect_spec(value: Any, *, key: str | None = None) -> int:
    """Reject remote data and unsafe dimensions; return the inline row count."""
    if isinstance(value, Mapping):
        if key == "data" and "url" in value:
            raise ValueError("External data URLs are not allowed; use inline data.values")

        is_spec = key in {None, "concat", "hconcat", "layer", "spec", "vconcat"}
        count = 0
        if key == "datasets":
            count = sum(len(dataset) for dataset in value.values() if isinstance(dataset, list))
        for child_key, child in value.items():
            if (
                is_spec
                and child_key in {"width", "height"}
                and isinstance(child, (int, float))
                and (isinstance(child, bool) or child <= 0 or child > MAX_DIMENSION)
            ):
                raise ValueError(f"{child_key} must be between 1 and {MAX_DIMENSION} pixels")
            if child_key == "values" and isinstance(child, list):
                count += len(child)
            count += _inspect_spec(child, key=str(child_key))
        return count

    if isinstance(value, list):
        return sum(_inspect_spec(item, key=key) for item in value)

    return 0


def _prepare_spec(spec: dict[str, Any]) -> dict[str, Any]:
    if not isinstance(spec, dict):
        raise TypeError("spec must be a JSON object")

    try:
        encoded = json.dumps(spec, ensure_ascii=False, separators=(",", ":")).encode()
    except (TypeError, ValueError) as exc:
        raise ValueError(f"spec must contain only JSON-compatible values: {exc}") from exc

    if len(encoded) > MAX_SPEC_BYTES:
        raise ValueError(f"spec exceeds the {MAX_SPEC_BYTES}-byte limit")

    inline_values = _inspect_spec(spec)
    if inline_values > MAX_INLINE_VALUES:
        raise ValueError(
            f"spec contains {inline_values} inline values; limit is {MAX_INLINE_VALUES}"
        )

    prepared = copy.deepcopy(spec)
    prepared.setdefault("$schema", "https://vega.github.io/schema/vega-lite/v6.json")
    if "mark" in prepared or "layer" in prepared:
        prepared.setdefault("width", 800)
        prepared.setdefault("height", 450)
        prepared.setdefault("autosize", {"type": "fit", "contains": "padding"})
    config = prepared.get("config", {})
    if not isinstance(config, Mapping):
        raise ValueError("spec.config must be an object")
    prepared["config"] = _deep_merge(_DEFAULT_CONFIG, config)
    return prepared


class VegaLiteClient:
    """Render Vega-Lite JSON without network access."""

    def render(
        self,
        spec: dict[str, Any],
        output_format: str = "png",
        scale: float = 2.0,
    ) -> str:
        """Render a Vega-Lite spec and return base64-encoded PNG or SVG bytes.

        Data must be provided inline with ``data.values`` or ``datasets``.
        External data and image URLs are blocked. The spec is compiled with
        Vega-Lite 6.4 and receives the default visual style; values in the
        spec's ``config`` override those defaults.
        """
        prepared = _prepare_spec(spec)
        normalized_format = output_format.lower()
        if normalized_format not in {"png", "svg"}:
            raise ValueError("output_format must be 'png' or 'svg'")
        if isinstance(scale, bool) or not 0.1 <= scale <= MAX_SCALE:
            raise ValueError(f"scale must be between 0.1 and {MAX_SCALE}")

        if normalized_format == "png":
            rendered = vlc.vegalite_to_png(
                prepared,
                vl_version=VEGA_LITE_VERSION,
                scale=scale,
                allowed_base_urls=[],
            )
        else:
            rendered = vlc.vegalite_to_svg(
                prepared,
                vl_version=VEGA_LITE_VERSION,
                allowed_base_urls=[],
            ).encode("utf-8")

        return base64.b64encode(rendered).decode("ascii")


def _client() -> VegaLiteClient:
    return VegaLiteClient()
