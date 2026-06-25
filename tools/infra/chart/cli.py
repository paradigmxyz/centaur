"""CLI for Chart generation for Centaur. Single render_chart method returns base64 PNG images ready for Slack upload.."""

from dotenv import load_dotenv

load_dotenv()

import json
import typer

app = typer.Typer(
    name="chart",
    help="Chart generation for Centaur. Single render_chart method returns base64 PNG images ready for Slack upload.",
)


@app.callback()
def main() -> None:
    """chart CLI."""


@app.command("health")
def health():
    """Assert chart connectivity and auth with a safe read-only check."""
    from .client import _client

    client = _client()
    try:
        details = {
            "png_base64_length": len(
                client.render_chart(
                    chart_type="bar",
                    data=[{"label": "health", "value": 1}],
                    title="Health",
                    x="label",
                    y="value",
                )
            )
        }
        payload = {"ok": True, "tool": "chart", "error": None, "details": details}
    except Exception as exc:
        payload = {"ok": False, "tool": "chart", "error": str(exc), "details": {}}
        print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))
        raise typer.Exit(1) from exc
    finally:
        close = getattr(client, "close", None)
        if callable(close):
            close()
    print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))


if __name__ == "__main__":
    app()
