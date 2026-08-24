"""CLI for NewsAPI.ai article search."""

# ruff: noqa: E402, I001 -- dotenv must load before imports for standalone CLI use.

from dotenv import load_dotenv

load_dotenv()

import json

import typer

from .client import GET_ARTICLES_DOCS_URL, GetArticlesRequest, NewsAPIAIClient


app = typer.Typer(name="newsapi_ai", help="Submit a NewsAPI.ai getArticles JSON request")


@app.command("get-articles")
def get_articles(
    request_json: str = typer.Argument(
        ...,
        help=f"getArticles request as a JSON object. Parameters: {GET_ARTICLES_DOCS_URL}",
    ),
) -> None:
    """Submit a getArticles request and print the JSON response."""
    try:
        request = json.loads(request_json)
    except json.JSONDecodeError as exc:
        raise typer.BadParameter("request must be valid JSON") from exc
    if not isinstance(request, dict):
        raise typer.BadParameter("request must be a JSON object")

    with NewsAPIAIClient() as client:
        data = client.get_articles(request=GetArticlesRequest(**request))
    print(json.dumps(data, indent=2, ensure_ascii=False))


if __name__ == "__main__":
    app()
