from dotenv import load_dotenv

load_dotenv()

import json  # noqa: E402
from typing import Any  # noqa: E402

import typer  # noqa: E402
from rich.console import Console  # noqa: E402
from rich.markdown import Markdown  # noqa: E402

app = typer.Typer(name="paybox", help="Call the PayBox MCP server")
console = Console()


def _emit(value: Any, markdown: bool, json_output: bool) -> None:
    if markdown and json_output:
        raise typer.BadParameter("choose either --json or --markdown")
    rendered = json.dumps(value, indent=2, ensure_ascii=False, default=str)
    if markdown:
        console.print(Markdown(f"```json\n{rendered}\n```"))
    else:
        print(rendered)


@app.command("health")
def health(
    json_output: bool = typer.Option(False, "--json", help="Render as JSON"),
    markdown: bool = typer.Option(False, "--markdown", help="Render as Markdown"),
) -> None:
    """Assert authenticated PayBox MCP connectivity."""
    from .client import _client

    with _client() as client:
        _emit(client.health(), markdown, json_output)


@app.command("list-tools")
def list_tools(
    json_output: bool = typer.Option(False, "--json", help="Render as JSON"),
    markdown: bool = typer.Option(False, "--markdown", help="Render as Markdown"),
) -> None:
    """List MCP tools enabled for the connected user."""
    from .client import _client

    with _client() as client:
        _emit(client.list_tools(), markdown, json_output)


@app.command("call")
def call_tool(
    tool_name: str = typer.Argument(..., help="PayBox MCP tool name"),
    arguments: str = typer.Option("{}", "--arguments", "-a", help="JSON object of tool arguments"),
    confirm: bool = typer.Option(False, "--confirm", help="Confirm a sensitive operation"),
    json_output: bool = typer.Option(False, "--json", help="Render as JSON"),
    markdown: bool = typer.Option(False, "--markdown", help="Render as Markdown"),
) -> None:
    """Call a PayBox MCP tool by name."""
    from .client import _client

    try:
        parsed = json.loads(arguments)
    except json.JSONDecodeError as exc:
        raise typer.BadParameter("--arguments must be valid JSON") from exc
    if not isinstance(parsed, dict):
        raise typer.BadParameter("--arguments must decode to a JSON object")

    with _client() as client:
        _emit(client.execute(tool_name, parsed, confirm=confirm), markdown, json_output)


if __name__ == "__main__":
    app()
