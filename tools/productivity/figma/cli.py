"""CLI entrypoint for Figma tool."""

import json

from dotenv import load_dotenv

load_dotenv()

import typer
from rich.console import Console
from rich.table import Table

app = typer.Typer(name="figma", help="Figma design system extraction")


@app.command("health")
def health():
    """Assert figma connectivity and auth with a safe read-only check."""
    from .client import _client

    client = _client()
    try:
        details = client._request("/me")
        payload = {"ok": True, "tool": "figma", "error": None, "details": details}
    except Exception as exc:
        payload = {"ok": False, "tool": "figma", "error": str(exc), "details": {}}
        print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))
        raise typer.Exit(1) from exc
    finally:
        close = getattr(client, "close", None)
        if callable(close):
            close()
    print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))


console = Console()


@app.command()
def crawl(
    url: str = typer.Argument(..., help="Figma file or frame URL"),
    json_output: bool = typer.Option(False, "--json", "-j", help="Output as JSON"),
):
    """Crawl a Figma file and extract design system info.

    Extracts colors, typography, components, variables, and frames from any Figma URL.
    Uses FIGMA env var for authentication (personal access token).
    """
    from .client import FigmaClient

    try:
        client = FigmaClient()
        ds = client.crawl(url)
    except ValueError as e:
        console.print(f"[red]{e}[/]")
        return
    except Exception as e:
        console.print(f"[red]Figma API error: {e}[/]")
        return

    if json_output:
        import json
        import sys

        output = {
            "file_name": ds.file_name,
            "colors": ds.colors,
            "text_styles": ds.text_styles,
            "components": ds.components,
            "variables": ds.variables,
            "frames": ds.frames,
            "effects": ds.effects,
            "grids": ds.grids,
        }
        print(json.dumps(output, indent=2, ensure_ascii=True), file=sys.stdout)
        return

    console.print(f"\n[bold]{ds.file_name}[/]\n")

    # Colors
    if ds.colors:
        table = Table(title="Colors")
        table.add_column("Color", style="cyan", max_width=20)
        table.add_column("Name/Source", style="white", max_width=40)
        seen = set()
        for c in ds.colors:
            val = c.get("value") or c.get("name", "")
            if val in seen:
                continue
            seen.add(val)
            source = c.get("source") or c.get("description", "")
            table.add_row(val, source[:40])
        console.print(table)

    # Typography
    if ds.text_styles:
        table = Table(title="Typography")
        table.add_column("Font", style="cyan", max_width=25)
        table.add_column("Size", style="green", max_width=8)
        table.add_column("Weight", style="dim", max_width=8)
        table.add_column("Line Height", style="dim", max_width=12)
        for t in ds.text_styles:
            font = t.get("fontFamily") or t.get("name", "")
            size = str(t.get("fontSize", "")) if t.get("fontSize") else ""
            weight = str(t.get("fontWeight", "")) if t.get("fontWeight") else ""
            lh = f"{t['lineHeight']:.1f}px" if t.get("lineHeight") else ""
            table.add_row(font, size, weight, lh)
        console.print(table)

    # Components
    if ds.components:
        table = Table(title="Components")
        table.add_column("Name", style="cyan", max_width=40)
        table.add_column("Description", style="dim", max_width=50)
        for c in ds.components:
            table.add_row(c["name"], c.get("description", "")[:50])
        console.print(table)

    # Variables
    if ds.variables:
        table = Table(title="Variables")
        table.add_column("Name", style="cyan", max_width=30)
        table.add_column("Type", style="green", max_width=15)
        for v in ds.variables:
            table.add_row(v["name"], v.get("type", ""))
        console.print(table)

    # Frames
    if ds.frames:
        table = Table(title="Frames")
        table.add_column("Name", style="cyan", max_width=40)
        table.add_column("Size", style="green", max_width=15)
        table.add_column("Background", style="dim", max_width=20)
        for f in ds.frames:
            w, h = f.get("width"), f.get("height")
            size = f"{int(w)}x{int(h)}" if w and h else ""
            bg = f.get("background") or ""
            table.add_row(f["name"], size, bg)
        console.print(table)

    # Summary
    console.print(
        f"\n[dim]Found: {len(ds.colors)} colors, {len(ds.text_styles)} text styles, "
        f"{len(ds.components)} components, {len(ds.variables)} variables, "
        f"{len(ds.frames)} frames[/]"
    )
