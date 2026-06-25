"""Twitter CLI."""

from dotenv import load_dotenv

load_dotenv()

import json  # noqa: E402
from datetime import datetime  # noqa: E402

import typer  # noqa: E402
from rich.console import Console  # noqa: E402

from centaur_sdk import Table  # noqa: E402

from .client import _client  # noqa: E402

app = typer.Typer(name="twitter", help="Twitter CLI")


@app.command("health")
def health():
    """Assert twitter connectivity and auth with a safe read-only check."""
    from .client import _client

    client = _client()
    try:
        details = client.get_usage()
        payload = {"ok": True, "tool": "twitter", "error": None, "details": details}
    except Exception as exc:
        payload = {"ok": False, "tool": "twitter", "error": str(exc), "details": {}}
        print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))
        raise typer.Exit(1) from exc
    finally:
        close = getattr(client, "close", None)
        if callable(close):
            close()
    print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))


console = Console()


def format_number(value: int | None) -> str:
    """Format large numbers with K/M suffixes."""
    if value is None:
        return "N/A"
    if value >= 1_000_000:
        return f"{value / 1_000_000:.1f}M"
    elif value >= 1_000:
        return f"{value / 1_000:.1f}K"
    return str(value)


def format_timestamp(ts: int | None) -> str:
    """Format epoch milliseconds to readable date."""
    if ts is None:
        return "N/A"
    try:
        dt = datetime.fromtimestamp(ts / 1000)
        return dt.strftime("%Y-%m-%d %H:%M")
    except (ValueError, OSError):
        return "N/A"


def truncate(text: str | None, max_len: int = 50) -> str:
    """Truncate text with ellipsis."""
    if not text:
        return ""
    text = text.replace("\n", " ").strip()
    if len(text) <= max_len:
        return text
    return text[: max_len - 3] + "..."


def print_metadata(meta: dict) -> None:
    """Print API usage metadata."""
    consumed = meta.get("consumed_units")
    remaining = meta.get("remaining_units")
    if consumed is not None:
        console.print(f"[dim]Credits: {consumed} consumed", end="")
        if remaining is not None:
            console.print(f", {format_number(remaining)} remaining[/dim]")
        else:
            console.print("[/dim]")


# =============================================================================
# User Commands
# =============================================================================


@app.command()
def user(
    handle: str = typer.Argument(..., help="Twitter handle (without @)"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown"),
):
    """Get user profile by handle."""
    client = _client()
    data = client.get_user(handle)

    if data is None:
        console.print(f"[red]User @{handle} not found[/red]")
        raise typer.Exit(1)

    if json_output:
        print(json.dumps(data, indent=2))
        return

    if markdown:
        print(f"# @{data.get('screen_name')} ({data.get('name')})\n")
        print(f"**ID:** {data.get('user_id')}")
        print(f"**Followers:** {format_number(data.get('followers_count'))}")
        print(f"**Following:** {format_number(data.get('following_count'))}")
        print(f"**Tweets:** {format_number(data.get('statuses_count'))}")
        print(f"**Verified:** {data.get('is_blue_verified', False)}")
        print(f"**Location:** {data.get('location') or 'N/A'}")
        print(f"**Created:** {data.get('created_at', 'N/A')}")
        if data.get("description"):
            print(f"\n**Bio:** {data.get('description')}")
        if data.get("website_url"):
            print(f"**Website:** {data.get('website_url')}")
        return

    console.print(f"\n[bold cyan]@{data.get('screen_name')}[/] ({data.get('name')})")
    console.print(f"[dim]ID: {data.get('user_id')}[/]\n")

    console.print(f"Followers: [green]{format_number(data.get('followers_count'))}[/]")
    console.print(f"Following: [blue]{format_number(data.get('following_count'))}[/]")
    console.print(f"Tweets: [yellow]{format_number(data.get('statuses_count'))}[/]")

    verified = data.get("is_blue_verified", False)
    if verified:
        console.print("[cyan]Verified[/cyan]")

    if data.get("location"):
        console.print(f"Location: {data.get('location')}")

    if data.get("description"):
        console.print(f"\n[dim]{data.get('description')}[/dim]")


@app.command()
def followers(
    handle: str = typer.Argument(..., help="Twitter handle (without @)"),
    limit: int = typer.Option(100, "--limit", "-n", help="Max followers to fetch"),
    ids_only: bool = typer.Option(False, "--ids", help="Return only user IDs"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown"),
):
    """Get followers of a user."""
    client = _client()
    followers_list, meta = client.get_followers(handle, limit=limit, ids_only=ids_only)

    if json_output:
        print(json.dumps(followers_list, indent=2))
        return

    if ids_only:
        if markdown:
            print(f"# Followers of @{handle}\n")
            print(f"**Count:** {len(followers_list)}\n")
            print("```")
            for uid in followers_list:
                print(uid)
            print("```")
        else:
            console.print(f"[bold]Followers of @{handle}[/] ({len(followers_list)} IDs)\n")
            for uid in followers_list:
                print(uid)
        return

    # Full user details
    if markdown:
        print(f"# Followers of @{handle}\n")
        print(f"**Count:** {len(followers_list)}\n")
        print("| Handle | Name | Followers | Following |")
        print("|--------|------|-----------|-----------|")
        for u in followers_list:
            print(
                f"| @{u.get('username', 'N/A')} | {u.get('name', 'N/A')} | "
                f"{format_number(u.get('followers_count'))} | "
                f"{format_number(u.get('following_count'))} |"
            )
        return

    table = Table(title=f"Followers of @{handle}")
    table.add_column("Handle", style="cyan")
    table.add_column("Name", style="white", max_width=25)
    table.add_column("Followers", justify="right", style="green")
    table.add_column("Following", justify="right", style="blue")

    for u in followers_list:
        table.add_row(
            f"@{u.get('username', 'N/A')}",
            truncate(u.get("name"), 25),
            format_number(u.get("followers_count")),
            format_number(u.get("following_count")),
        )

    console.print(table)
    print_metadata(meta)


@app.command()
def following(
    handle: str = typer.Argument(..., help="Twitter handle (without @)"),
    limit: int = typer.Option(100, "--limit", "-n", help="Max accounts to fetch"),
    ids_only: bool = typer.Option(False, "--ids", help="Return only user IDs"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown"),
):
    """Get accounts a user follows."""
    client = _client()
    following_list, meta = client.get_following(handle, limit=limit, ids_only=ids_only)

    if json_output:
        print(json.dumps(following_list, indent=2))
        return

    if ids_only:
        if markdown:
            print(f"# Following by @{handle}\n")
            print(f"**Count:** {len(following_list)}\n")
            print("```")
            for uid in following_list:
                print(uid)
            print("```")
        else:
            console.print(f"[bold]Following by @{handle}[/] ({len(following_list)} IDs)\n")
            for uid in following_list:
                print(uid)
        return

    # Full user details
    if markdown:
        print(f"# Following by @{handle}\n")
        print(f"**Count:** {len(following_list)}\n")
        print("| Handle | Name | Followers | Following |")
        print("|--------|------|-----------|-----------|")
        for u in following_list:
            print(
                f"| @{u.get('username', 'N/A')} | {u.get('name', 'N/A')} | "
                f"{format_number(u.get('followers_count'))} | "
                f"{format_number(u.get('following_count'))} |"
            )
        return

    table = Table(title=f"Following by @{handle}")
    table.add_column("Handle", style="cyan")
    table.add_column("Name", style="white", max_width=25)
    table.add_column("Followers", justify="right", style="green")
    table.add_column("Following", justify="right", style="blue")

    for u in following_list:
        table.add_row(
            f"@{u.get('username', 'N/A')}",
            truncate(u.get("name"), 25),
            format_number(u.get("followers_count")),
            format_number(u.get("following_count")),
        )

    console.print(table)
    print_metadata(meta)


@app.command()
def lookup(
    user_ids: str = typer.Argument(..., help="Comma-separated user IDs"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown"),
):
    """Lookup users by IDs."""
    ids = [uid.strip() for uid in user_ids.split(",") if uid.strip()]

    if not ids:
        console.print("[red]No valid user IDs provided[/red]")
        raise typer.Exit(1)

    client = _client()
    users = client.lookup_users(ids)

    if json_output:
        print(json.dumps(users, indent=2))
        return

    if markdown:
        print("# User Lookup\n")
        print("| ID | Handle | Name | Followers |")
        print("|----|--------|------|-----------|")
        for u in users:
            print(
                f"| {u.get('user_id')} | @{u.get('screen_name')} | "
                f"{u.get('name')} | {format_number(u.get('followers_count'))} |"
            )
        return

    table = Table(title="User Lookup")
    table.add_column("ID", style="dim")
    table.add_column("Handle", style="cyan")
    table.add_column("Name", style="white", max_width=25)
    table.add_column("Followers", justify="right", style="green")

    for u in users:
        table.add_row(
            u.get("user_id", "N/A"),
            f"@{u.get('screen_name', 'N/A')}",
            truncate(u.get("name"), 25),
            format_number(u.get("followers_count")),
        )

    console.print(table)


# =============================================================================
# Tweet Commands
# =============================================================================


@app.command()
def search(
    query: str = typer.Argument(..., help="Search query (e.g., 'bitcoin since:2025-01-01')"),
    search_type: str = typer.Option("latest", "--type", "-t", help="'top' or 'latest'"),
    limit: int = typer.Option(20, "--limit", "-n", help="Max tweets to fetch"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown"),
):
    """Search for tweets."""
    client = _client()
    tweets, meta = client.search_tweets(query, search_type=search_type, limit=limit)

    if json_output:
        print(json.dumps(tweets, indent=2))
        return

    if not tweets:
        console.print(f"[yellow]No tweets found for '{query}'[/yellow]")
        return

    if markdown:
        print(f"# Tweet Search: {query}\n")
        print(f"**Results:** {len(tweets)}\n")
        for t in tweets:
            print(f"---\n**@{t.get('screen_name')}** ({format_timestamp(t.get('published_at'))})")
            print(f"> {t.get('text', '')}\n")
            print(
                f"Likes: {t.get('like_count', 0)} | "
                f"RTs: {t.get('retweet_count', 0)} | "
                f"Replies: {t.get('reply_count', 0)}\n"
            )
        return

    console.print(f"\n[bold]Search: {query}[/] ({len(tweets)} results)\n")

    for t in tweets:
        console.print(
            f"[cyan]@{t.get('screen_name')}[/] [dim]{format_timestamp(t.get('published_at'))}[/dim]"
        )
        console.print(f"  {truncate(t.get('text'), 100)}")
        console.print(
            f"  [dim]Likes: {t.get('like_count', 0)} | "
            f"RTs: {t.get('retweet_count', 0)} | "
            f"Replies: {t.get('reply_count', 0)}[/dim]\n"
        )

    print_metadata(meta)


@app.command()
def tweets(
    tweet_ids: str = typer.Argument(..., help="Comma-separated tweet IDs"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown"),
):
    """Lookup tweets by IDs."""
    ids = [tid.strip() for tid in tweet_ids.split(",") if tid.strip()]

    if not ids:
        console.print("[red]No valid tweet IDs provided[/red]")
        raise typer.Exit(1)

    client = _client()
    tweets_list = client.lookup_tweets(ids)

    if json_output:
        print(json.dumps(tweets_list, indent=2))
        return

    if not tweets_list:
        console.print("[yellow]No tweets found[/yellow]")
        return

    if markdown:
        print("# Tweet Lookup\n")
        for t in tweets_list:
            print(f"---\n**@{t.get('screen_name')}** ({format_timestamp(t.get('published_at'))})")
            print(f"> {t.get('text', '')}\n")
            print(
                f"Likes: {t.get('like_count', 0)} | "
                f"RTs: {t.get('retweet_count', 0)} | "
                f"Views: {format_number(t.get('view_count'))}\n"
            )
        return

    for t in tweets_list:
        console.print(
            f"[cyan]@{t.get('screen_name')}[/] [dim]{format_timestamp(t.get('published_at'))}[/dim]"
        )
        console.print(f"  {t.get('text', '')}")
        console.print(
            f"  [dim]Likes: {t.get('like_count', 0)} | "
            f"RTs: {t.get('retweet_count', 0)} | "
            f"Views: {format_number(t.get('view_count'))}[/dim]\n"
        )


@app.command()
def timeline(
    handle: str = typer.Argument(..., help="Twitter handle (without @)"),
    limit: int = typer.Option(20, "--limit", "-n", help="Max tweets to fetch"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
    markdown: bool = typer.Option(False, "--markdown", "-m", help="Output as markdown"),
):
    """Get a user's timeline (recent tweets)."""
    client = _client()
    user, tweets, meta = client.get_timeline(handle, limit=limit)

    if user is None:
        console.print(f"[red]User @{handle} not found[/red]")
        raise typer.Exit(1)

    if json_output:
        print(json.dumps(tweets, indent=2))
        return

    if not tweets:
        console.print(f"[yellow]No tweets found for @{handle}[/yellow]")
        return

    if markdown:
        print(f"# Timeline: @{handle}\n")
        print(f"**Tweets:** {len(tweets)}\n")
        for t in tweets:
            print(f"---\n**{format_timestamp(t.get('published_at'))}**")
            print(f"> {t.get('text', '')}\n")
            print(
                f"Likes: {t.get('like_count', 0)} | "
                f"RTs: {t.get('retweet_count', 0)} | "
                f"Views: {format_number(t.get('view_count'))}\n"
            )
        return

    console.print(f"\n[bold]Timeline: @{handle}[/] ({len(tweets)} tweets)\n")

    for t in tweets:
        console.print(f"[dim]{format_timestamp(t.get('published_at'))}[/dim]")
        console.print(f"  {truncate(t.get('text'), 100)}")
        console.print(
            f"  [dim]Likes: {t.get('like_count', 0)} | "
            f"RTs: {t.get('retweet_count', 0)} | "
            f"Views: {format_number(t.get('view_count'))}[/dim]\n"
        )

    print_metadata(meta)


# =============================================================================
# Utility Commands
# =============================================================================


@app.command()
def usage():
    """Check API credit usage."""
    client = _client()
    api_usage = client.get_usage()

    console.print("\n[bold]API Usage[/bold]\n")
    console.print(api_usage.get("message", "No usage information returned."))


if __name__ == "__main__":
    app()
