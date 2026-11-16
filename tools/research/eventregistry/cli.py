"""CLI for Event Registry article search."""

# ruff: noqa: E402, I001 -- dotenv must load before imports for standalone CLI use.

from dotenv import load_dotenv

load_dotenv()

import json

import typer

from .client import EVENT_REGISTRY_DOCS_URL, EventRegistryClient, GetArticlesRequest


app = typer.Typer(name="eventregistry", help="Submit an Event Registry getArticles JSON request")


@app.command("get-articles")
def get_articles(
    request_json: str = typer.Argument(
        ...,
        help=f"getArticles request as a JSON object. Parameters: {EVENT_REGISTRY_DOCS_URL}",
    ),
) -> None:
    """Submit a getArticles request and print the JSON response."""
    try:
        request = json.loads(request_json)
    except json.JSONDecodeError as exc:
        raise typer.BadParameter("request must be valid JSON") from exc
    if not isinstance(request, dict):
        raise typer.BadParameter("request must be a JSON object")

    with EventRegistryClient() as client:
        data = client.get_articles(request=GetArticlesRequest(**request))
    print(json.dumps(data, indent=2, ensure_ascii=False))


if __name__ == "__main__":
    app()
