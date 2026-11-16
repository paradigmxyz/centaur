"""CLI for Veo 3 video generation."""

import json
import typer
from dotenv import load_dotenv
from rich.console import Console
from rich.table import Table

load_dotenv()

app = typer.Typer(name="veo3", help="Veo 3 CLI for Google's video generation model")


@app.command("health")
def health():
    """Assert veo3 connectivity and auth with a safe read-only check."""
    from .client import _client

    client = _client()
    try:
        details = {
            "models": [
                getattr(model, "name", str(model))
                for model in list(client.client.models.list())[:3]
            ]
        }
        payload = {"ok": True, "tool": "veo3", "error": None, "details": details}
    except Exception as exc:
        payload = {"ok": False, "tool": "veo3", "error": str(exc), "details": {}}
        print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))
        raise typer.Exit(1) from exc
    finally:
        close = getattr(client, "close", None)
        if callable(close):
            close()
    print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))


console = Console()


def get_client():
    from .client import Veo3Client

    return Veo3Client()


def progress_callback(message: str):
    """Print progress message."""
    console.print(f"[dim]{message}[/]")


@app.command()
def generate(
    prompt: str = typer.Argument(..., help="Text description of the video to generate"),
    output: str = typer.Option(..., "--output", "-o", help="Output file path (e.g., output.mp4)"),
    model: str = typer.Option(
        "full",
        "--model",
        "-m",
        help="Model to use: 'full' (highest quality) or 'fast' (faster, cheaper)",
    ),
    aspect_ratio: str = typer.Option(
        "16:9", "--aspect-ratio", "-a", help="Aspect ratio: 16:9, 9:16, or 1:1"
    ),
    resolution: str = typer.Option(
        "720p", "--resolution", "-r", help="Resolution: 720p, 1080p, or 4k"
    ),
):
    """Generate a video from a text prompt."""
    client = get_client()

    with console.status("[bold green]Generating video...", spinner="dots"):
        try:
            result = client.generate(
                prompt=prompt,
                output=output,
                model=model,
                aspect_ratio=aspect_ratio,
                resolution=resolution,
                progress_callback=progress_callback,
            )
            console.print(f"\n[green]✓[/] Video saved to: [bold]{result}[/]")
        except ValueError as e:
            console.print(f"[red]Error: {e}[/]")
            raise typer.Exit(1)
        except RuntimeError as e:
            console.print(f"[red]Error: {e}[/]")
            raise typer.Exit(1)


@app.command("from-image")
def from_image(
    image: str = typer.Argument(..., help="Path to input image (first frame)"),
    prompt: str = typer.Argument(..., help="Text description of what should happen"),
    output: str = typer.Option(..., "--output", "-o", help="Output file path (e.g., output.mp4)"),
    model: str = typer.Option(
        "full",
        "--model",
        "-m",
        help="Model to use: 'full' (highest quality) or 'fast' (faster, cheaper)",
    ),
    aspect_ratio: str = typer.Option(
        "16:9", "--aspect-ratio", "-a", help="Aspect ratio: 16:9, 9:16, or 1:1"
    ),
    resolution: str = typer.Option(
        "720p", "--resolution", "-r", help="Resolution: 720p, 1080p, or 4k"
    ),
):
    """Generate a video using an image as the first frame."""
    client = get_client()

    with console.status("[bold green]Generating video from image...", spinner="dots"):
        try:
            result = client.generate_from_image(
                image_path=image,
                prompt=prompt,
                output=output,
                model=model,
                aspect_ratio=aspect_ratio,
                resolution=resolution,
                progress_callback=progress_callback,
            )
            console.print(f"\n[green]✓[/] Video saved to: [bold]{result}[/]")
        except ValueError as e:
            console.print(f"[red]Error: {e}[/]")
            raise typer.Exit(1)
        except RuntimeError as e:
            console.print(f"[red]Error: {e}[/]")
            raise typer.Exit(1)


@app.command()
def extend(
    video: str = typer.Argument(..., help="Path to input video to extend"),
    prompt: str = typer.Argument(..., help="Text description of what should happen next"),
    output: str = typer.Option(..., "--output", "-o", help="Output file path (e.g., extended.mp4)"),
    model: str = typer.Option(
        "full",
        "--model",
        "-m",
        help="Model to use: 'full' (highest quality) or 'fast' (faster, cheaper)",
    ),
):
    """Extend an existing video with additional content."""
    client = get_client()

    with console.status("[bold green]Extending video...", spinner="dots"):
        try:
            result = client.extend(
                video_path=video,
                prompt=prompt,
                output=output,
                model=model,
                progress_callback=progress_callback,
            )
            console.print(f"\n[green]✓[/] Extended video saved to: [bold]{result}[/]")
        except ValueError as e:
            console.print(f"[red]Error: {e}[/]")
            raise typer.Exit(1)
        except RuntimeError as e:
            console.print(f"[red]Error: {e}[/]")
            raise typer.Exit(1)


@app.command()
def models():
    """List available Veo models."""
    client = get_client()
    model_list = client.list_models()

    table = Table(title="Available Veo Models")
    table.add_column("Name", style="cyan")
    table.add_column("Model ID", style="green")
    table.add_column("Description", style="dim")

    for m in model_list:
        table.add_row(m["name"], m["id"], m["description"])

    console.print(table)
    console.print("\n[dim]Use --model fast or --model full with generate commands[/]")


if __name__ == "__main__":
    app()
