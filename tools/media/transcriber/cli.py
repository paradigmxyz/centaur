"""Voice transcription CLI with streaming support."""

from dotenv import load_dotenv

load_dotenv()

import queue
import shutil
import signal
import subprocess
import tempfile
import threading
import time
from pathlib import Path
from typing import Optional

import json
import typer
from rich.console import Console
from rich.panel import Panel

from .client import DEFAULT_MODEL, END_PHRASE, IS_MACOS, TranscriberClient

app = typer.Typer(help="Local-first voice transcription with live streaming")


@app.command("health")
def health():
    """Assert transcriber connectivity and auth with a safe read-only check."""
    from .client import _client

    client = _client()
    try:
        details = {"models": client.list_models(), "recorder": client.find_recorder()}
        payload = {"ok": True, "tool": "transcriber", "error": None, "details": details}
    except Exception as exc:
        payload = {"ok": False, "tool": "transcriber", "error": str(exc), "details": {}}
        print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))
        raise typer.Exit(1) from exc
    finally:
        close = getattr(client, "close", None)
        if callable(close):
            close()
    print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))


console = Console()

_client = TranscriberClient()


def _recorder_thread(
    tmpdir: Path, chunk_size: float, chunk_queue: "queue.Queue", stop_event: threading.Event
):
    """Background thread that continuously records audio chunks."""
    chunk_num = 0
    while not stop_event.is_set():
        chunk_path = tmpdir / f"chunk_{chunk_num:04d}.wav"
        try:
            _client.record_chunk(chunk_path, chunk_size)
            if chunk_path.exists() and chunk_path.stat().st_size > 500:
                chunk_queue.put(chunk_path)
            chunk_num += 1
        except Exception:
            break


@app.command()
def record(
    output: Optional[Path] = typer.Option(
        None, "-o", "--output", help="Save transcription to file"
    ),
    model: str = typer.Option(
        DEFAULT_MODEL, "-m", "--model", help="Model: tiny/base/small/medium/large/turbo"
    ),
    duration: Optional[float] = typer.Option(
        None, "-d", "--duration", help="Max recording duration in seconds"
    ),
    language: Optional[str] = typer.Option(
        None, "-l", "--language", help="Language code (e.g., en, es)"
    ),
    copy: bool = typer.Option(False, "-c", "--copy", help="Copy result to clipboard"),
    chunk_size: float = typer.Option(5.0, "--chunk", help="Chunk size for streaming (seconds)"),
    streaming: bool = typer.Option(
        True, "--stream/--no-stream", help="Enable live streaming transcription"
    ),
):
    """Record with live streaming transcription. Say 'over and out' to stop."""
    if not streaming:
        _record_simple(output, model, duration, language, copy)
        return

    # Pre-load model before recording
    with console.status(f"[bold blue]Loading model {_client.get_model_name(model)}...[/]"):
        _client.get_whisper_model(model)
    console.print("[dim]Model loaded[/]")

    tmpdir = Path(tempfile.mkdtemp())
    chunks: list[Path] = []
    transcripts: list[str] = []
    stop_event = threading.Event()
    chunk_queue: queue.Queue = queue.Queue()
    start_time = time.time()

    console.print(f"[bold green]🎤 Recording...[/] (say '{END_PHRASE}' to stop)\n")

    # Start background recording thread
    recorder = threading.Thread(
        target=_recorder_thread, args=(tmpdir, chunk_size, chunk_queue, stop_event), daemon=True
    )
    recorder.start()

    try:
        while not stop_event.is_set():
            # Check duration limit
            if duration and (time.time() - start_time) >= duration:
                break

            # Get next chunk (with timeout to allow checking stop condition)
            try:
                chunk_path = chunk_queue.get(timeout=0.5)
            except queue.Empty:
                continue

            chunks.append(chunk_path)

            # Transcribe chunk (recording continues in background)
            try:
                text = _client.transcribe_audio(chunk_path, model, language)
                if text:
                    transcripts.append(text)
                    console.print(f"[cyan]>[/] {text}")

                    if END_PHRASE in text.lower():
                        console.print("\n[yellow]End phrase detected, stopping...[/]")
                        break
            except Exception as e:
                console.print(f"[dim]Transcription error: {e}[/]")

    except KeyboardInterrupt:
        console.print("\n[yellow]Stopped[/]")
    finally:
        stop_event.set()
        recorder.join(timeout=2)

    if not chunks:
        console.print("[yellow]No audio recorded[/]")
        shutil.rmtree(tmpdir, ignore_errors=True)
        raise typer.Exit(1)

    # Final transcription of full audio for accuracy
    console.print("\n[dim]Finalizing transcription...[/]")
    merged_path = tmpdir / "merged.wav"
    _client.merge_wav_files(chunks, merged_path)

    try:
        final_text = _client.transcribe_audio(merged_path, model, language)
        # Remove end phrase from final text
        final_text = final_text.lower().replace(END_PHRASE, "").strip()
        final_text = " ".join(final_text.split())  # Clean whitespace
    except Exception:
        # Fallback to concatenated chunks
        final_text = " ".join(transcripts)
        if END_PHRASE in final_text.lower():
            final_text = final_text.lower().split(END_PHRASE)[0].strip()

    # Cleanup
    shutil.rmtree(tmpdir, ignore_errors=True)

    if not final_text:
        console.print("[yellow]No speech detected[/]")
        raise typer.Exit(1)

    console.print(Panel(final_text, title="Final Transcription", border_style="green"))

    if output:
        output.write_text(final_text)
        console.print(f"[dim]Saved to {output}[/]")

    if copy:
        _copy_to_clipboard(final_text)

    typer.echo(final_text)


def _record_simple(
    output: Optional[Path],
    model: str,
    duration: Optional[float],
    language: Optional[str],
    copy: bool,
):
    """Simple non-streaming record."""
    tmp = Path(tempfile.NamedTemporaryFile(suffix=".wav", delete=False).name)

    console.print("[bold green]🎤 Recording...[/] (Ctrl+C to stop)")

    try:
        if duration:
            _client.record_chunk(tmp, duration)
        else:
            # Record until Ctrl+C
            recorder = _client.find_recorder()
            if recorder == "sox":
                cmd = ["rec", "-q", "-r", "16000", "-c", "1", "-b", "16", str(tmp)]
            elif IS_MACOS:
                device = _client.get_default_audio_device()
                cmd = [
                    "ffmpeg",
                    "-y",
                    "-f",
                    "avfoundation",
                    "-i",
                    device,
                    "-ar",
                    "16000",
                    "-ac",
                    "1",
                    str(tmp),
                ]
            else:
                cmd = [
                    "ffmpeg",
                    "-y",
                    "-f",
                    "alsa",
                    "-i",
                    "default",
                    "-ar",
                    "16000",
                    "-ac",
                    "1",
                    str(tmp),
                ]

            proc = subprocess.Popen(cmd, stderr=subprocess.DEVNULL)
            try:
                proc.wait()
            except KeyboardInterrupt:
                proc.send_signal(signal.SIGINT)
                proc.wait()
    except KeyboardInterrupt:
        pass

    if not tmp.exists() or tmp.stat().st_size < 1000:
        tmp.unlink(missing_ok=True)
        console.print("[yellow]Recording too short[/]")
        raise typer.Exit(1)

    console.print(f"[dim]Recorded {tmp.stat().st_size // 1024}KB[/]")

    with console.status("[bold blue]Transcribing...[/]"):
        text = _client.transcribe_audio(tmp, model, language)

    tmp.unlink(missing_ok=True)

    if not text:
        console.print("[yellow]No speech detected[/]")
        raise typer.Exit(1)

    console.print(Panel(text, title="Transcription", border_style="green"))

    if output:
        output.write_text(text)

    if copy:
        _copy_to_clipboard(text)

    typer.echo(text)


@app.command()
def file(
    path: Path = typer.Argument(..., help="Audio file to transcribe"),
    output: Optional[Path] = typer.Option(
        None, "-o", "--output", help="Save transcription to file"
    ),
    model: str = typer.Option(
        DEFAULT_MODEL, "-m", "--model", help="Model: tiny/base/small/medium/large/turbo"
    ),
    language: Optional[str] = typer.Option(None, "-l", "--language", help="Language code"),
):
    """Transcribe an audio file."""
    if not path.exists():
        console.print(f"[red]File not found: {path}[/]")
        raise typer.Exit(1)

    with console.status("[bold blue]Transcribing...[/]"):
        text = _client.transcribe_audio(path, model, language)

    console.print(Panel(text, title="Transcription", border_style="green"))

    if output:
        output.write_text(text)
        console.print(f"[dim]Saved to {output}[/]")

    typer.echo(text)


@app.command()
def listen(
    model: str = typer.Option(
        DEFAULT_MODEL, "-m", "--model", help="Model: tiny/base/small/medium/large/turbo"
    ),
    language: Optional[str] = typer.Option(None, "-l", "--language", help="Language code"),
    prefix: str = typer.Option("", "-p", "--prefix", help="Prefix for each transcription"),
):
    """Continuous listening mode - press Enter to start each recording."""
    console.print("[bold]Continuous listening mode[/]")
    console.print(
        "[dim]Press Enter to record, say 'end recording' to stop each, Ctrl+C to exit[/]\n"
    )

    tmpdir = Path(tempfile.mkdtemp())
    chunk_size = 3.0

    try:
        while True:
            input("[Press Enter to record]")

            chunks = []
            transcripts = []
            chunk_num = 0

            console.print("[green]Recording...[/]")

            try:
                while True:
                    chunk_path = tmpdir / f"listen_{chunk_num:04d}.wav"
                    _client.record_chunk(chunk_path, chunk_size)

                    if chunk_path.exists() and chunk_path.stat().st_size > 500:
                        chunks.append(chunk_path)
                        text = _client.transcribe_audio(chunk_path, model, language)
                        if text:
                            transcripts.append(text)
                            console.print(f"  {text}")
                            if END_PHRASE in text.lower():
                                break
                    chunk_num += 1
            except KeyboardInterrupt:
                pass

            if transcripts:
                result = " ".join(transcripts)
                if END_PHRASE in result.lower():
                    result = result.lower().split(END_PHRASE)[0].strip()
                out = f"{prefix}{result}" if prefix else result
                console.print(f"[green]>[/green] {out}\n")

            # Cleanup chunks
            for c in chunks:
                c.unlink(missing_ok=True)

    except KeyboardInterrupt:
        console.print("\n[dim]Goodbye![/]")
    finally:
        shutil.rmtree(tmpdir, ignore_errors=True)


@app.command()
def models():
    """List available Whisper models."""
    console.print("[bold]Available models:[/]\n")

    model_list = _client.list_models()
    backend = model_list[0]["backend"] if model_list else "unknown"
    console.print(f"[dim]Backend: {backend}[/]\n")

    for m in model_list:
        console.print(f"  [cyan]{m['name']}[/] -> {m['model_id']}")
        console.print(f"    Size: {m['size']} | {m['description']}\n")


def _copy_to_clipboard(text: str):
    """Copy text to system clipboard."""
    try:
        if IS_MACOS:
            subprocess.run(["pbcopy"], input=text.encode(), check=True)
        else:
            subprocess.run(["xclip", "-selection", "clipboard"], input=text.encode(), check=True)
        console.print("[dim]Copied to clipboard[/]")
    except Exception:
        pass


if __name__ == "__main__":
    app()
