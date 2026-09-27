"""CLI for rendering Vega-Lite specifications."""

from __future__ import annotations

import base64
import json
import sys
from pathlib import Path
from typing import Annotated, Any

import typer

app = typer.Typer(
    name="vega-lite",
    help="Render declarative Vega-Lite JSON specifications to PNG or SVG.",
    epilog=(
        "Vega-Lite specification reference: https://vega.github.io/vega-lite/docs/\n\n"
        "Keep data inline, then run: "
        "vega-lite render spec.json --output chart.png"
    ),
    no_args_is_help=True,
)


def _load_spec(path: str) -> dict[str, Any]:
    text = sys.stdin.read() if path == "-" else Path(path).read_text(encoding="utf-8")
    value = json.loads(text)
    if not isinstance(value, dict):
        raise ValueError("Vega-Lite spec must be a JSON object")
    return value


@app.command("render")
def render(
    spec_path: Annotated[str, typer.Argument(help="Vega-Lite JSON file, or - for stdin")],
    output: Annotated[Path, typer.Option("--output", "-o", help="Output PNG or SVG path")],
    output_format: Annotated[
        str | None,
        typer.Option("--format", help="Output format: png or svg (defaults to output extension)"),
    ] = None,
    scale: Annotated[float, typer.Option(help="PNG scale factor (0.1 to 3.0)")] = 2.0,
) -> None:
    """Render SPEC_PATH to a local PNG or SVG file.

    SPEC_PATH must contain one JSON object and may be `-` to read JSON from
    stdin. Keep all chart data inline; external data and image requests are
    blocked. The command prints JSON metadata after writing the output file.
    """
    from .client import _client

    chosen_format = (output_format or output.suffix.lstrip(".")).lower()
    if chosen_format not in {"png", "svg"}:
        raise typer.BadParameter("use --format png|svg or a .png/.svg output path")

    try:
        encoded = _client().render(
            spec=_load_spec(spec_path),
            output_format=chosen_format,
            scale=scale,
        )
        rendered = base64.b64decode(encoded)
        output.parent.mkdir(parents=True, exist_ok=True)
        output.write_bytes(rendered)
    except (OSError, ValueError, TypeError, json.JSONDecodeError) as exc:
        typer.echo(f"Error: {exc}", err=True)
        raise typer.Exit(1) from exc

    typer.echo(
        json.dumps(
            {
                "ok": True,
                "format": chosen_format,
                "output": str(output),
                "bytes": len(rendered),
            }
        )
    )


@app.command("health")
def health() -> None:
    """Verify that the local Vega-Lite renderer is operational."""
    from .client import _client

    try:
        encoded = _client().render(
            spec={
                "data": {"values": [{"label": "health", "value": 1}]},
                "mark": "bar",
                "encoding": {
                    "x": {"field": "label", "type": "nominal"},
                    "y": {"field": "value", "type": "quantitative"},
                },
            },
            scale=1,
        )
        rendered = base64.b64decode(encoded)
        payload = {
            "ok": rendered.startswith(b"\x89PNG"),
            "tool": "vega-lite",
            "error": None,
            "details": {"png_bytes": len(rendered)},
        }
    except Exception as exc:
        payload = {
            "ok": False,
            "tool": "vega-lite",
            "error": str(exc),
            "details": {},
        }
        typer.echo(json.dumps(payload, ensure_ascii=False))
        raise typer.Exit(1) from exc

    typer.echo(json.dumps(payload, ensure_ascii=False))


if __name__ == "__main__":
    app()
