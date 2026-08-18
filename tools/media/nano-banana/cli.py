"""CLI for Nano Banana (Gemini Image Generation)."""

from pathlib import Path

import json
import typer
from dotenv import load_dotenv
from rich.console import Console
from rich.table import Table

from .client import DEFAULT_MODEL, MODELS

load_dotenv()

app = typer.Typer(name="nano-banana", help="Nano Banana CLI for Google Gemini image generation")


@app.command("health")
def health():
    """Assert nano-banana connectivity and auth with a safe read-only check."""
    from .client import _client

    client = _client()
    try:
        details = {
            "models": [
                getattr(model, "name", str(model))
                for model in list(client.client.models.list())[:3]
            ]
        }
        payload = {"ok": True, "tool": "nano-banana", "error": None, "details": details}
    except Exception as exc:
        payload = {"ok": False, "tool": "nano-banana", "error": str(exc), "details": {}}
        print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))
        raise typer.Exit(1) from exc
    finally:
        close = getattr(client, "close", None)
        if callable(close):
            close()
    print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))


console = Console()


def get_client():
    from .client import NanoBananaClient

    return NanoBananaClient()


@app.command()
def generate(
    prompt: str = typer.Argument(..., help="Text description of the image to generate"),
    output: Path = typer.Option(Path("output.png"), "--output", "-o", help="Output file path"),
    model: str = typer.Option(
        DEFAULT_MODEL,
        "--model",
        "-m",
        help="Model to use: 'flash' (fast) or 'pro' (high quality/4K)",
    ),
    aspect_ratio: str = typer.Option(
        None,
        "--aspect-ratio",
        "-a",
        help="Aspect ratio: 1:1, 3:4, 4:3, 9:16, 16:9",
    ),
    size: str = typer.Option(
        None,
        "--size",
        "-s",
        help="Image size for pro model: 1K, 2K, 4K",
    ),
):
    """Generate an image from a text prompt.

    Examples:
        nano-banana generate "A serene mountain landscape at sunset"
        nano-banana generate "A cute robot" --model pro --size 4K
        nano-banana generate "A logo for a coffee shop" -o logo.png -a 1:1
    """
    client = get_client()

    with console.status(f"[bold green]Generating image with {model} model..."):
        try:
            image = client.generate(
                prompt=prompt,
                model=model,
                aspect_ratio=aspect_ratio,
                image_size=size,
            )
            image.save(output)
            console.print(f"[green]✓[/] Image saved to [cyan]{output}[/]")
        except Exception as e:
            console.print(f"[red]Error:[/] {e}")
            raise typer.Exit(1)


@app.command()
def edit(
    image: Path = typer.Argument(..., help="Path to the input image to edit"),
    prompt: str = typer.Argument(..., help="Text description of the edit to make"),
    output: Path = typer.Option(
        None, "--output", "-o", help="Output file path (defaults to input_edited.png)"
    ),
    model: str = typer.Option(
        DEFAULT_MODEL,
        "--model",
        "-m",
        help="Model to use: 'flash' (fast) or 'pro' (high quality)",
    ),
    aspect_ratio: str = typer.Option(
        None,
        "--aspect-ratio",
        "-a",
        help="Aspect ratio: 1:1, 3:4, 4:3, 9:16, 16:9",
    ),
):
    """Edit an existing image based on a text prompt.

    Examples:
        nano-banana edit photo.png "Change the sky to a starry night"
        nano-banana edit input.jpg "Remove the background" -o transparent.png
        nano-banana edit portrait.png "Make it look like a watercolor painting" --model pro
    """
    if not image.exists():
        console.print(f"[red]Error:[/] Image not found: {image}")
        raise typer.Exit(1)

    if output is None:
        output = image.with_stem(f"{image.stem}_edited")

    client = get_client()

    with console.status(f"[bold green]Editing image with {model} model..."):
        try:
            result = client.edit(
                image_path=image,
                prompt=prompt,
                model=model,
                aspect_ratio=aspect_ratio,
            )
            result.save(output)
            console.print(f"[green]✓[/] Edited image saved to [cyan]{output}[/]")
        except Exception as e:
            console.print(f"[red]Error:[/] {e}")
            raise typer.Exit(1)


@app.command()
def models():
    """List available image generation models."""
    table = Table(title="Available Models")
    table.add_column("Name", style="cyan")
    table.add_column("Model ID", style="green")
    table.add_column("Description", style="dim")

    descriptions = {
        "flash": "Fast generation (default). Best for quick iterations.",
        "pro": "High quality, 4K support, advanced reasoning. Best for production assets.",
    }

    for name, model_id in MODELS.items():
        table.add_row(name, model_id, descriptions.get(name, ""))

    console.print(table)
    console.print("\n[dim]Use --model flash or --model pro with generate/edit commands.[/]")


if __name__ == "__main__":
    app()
