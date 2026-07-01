"""CLI for Notion API."""

import json
import sys
from datetime import datetime

import typer
from rich.console import Console
from rich.table import Table

app = typer.Typer(name="notion", help="Notion CLI for AI agents")


@app.command("health")
def health():
    """Assert notion connectivity and auth with a safe read-only check."""
    from .client import _client

    client = _client()
    try:
        details = client.me()
        payload = {"ok": True, "tool": "notion", "error": None, "details": details}
    except Exception as exc:
        payload = {"ok": False, "tool": "notion", "error": str(exc), "details": {}}
        print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))
        raise typer.Exit(1) from exc
    finally:
        close = getattr(client, "close", None)
        if callable(close):
            close()
    print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))


console = Console()


def get_client():
    """Get Notion client with env loading."""
    from pathlib import Path

    from dotenv import load_dotenv

    cli_env = Path(__file__).parent.parent.parent / ".env"
    repo_env = Path(__file__).parent.parent.parent.parent.parent / ".env"

    for env_file in [cli_env, repo_env]:
        if env_file.exists():
            load_dotenv(env_file)
            break

    from .client import NotionClient

    return NotionClient()


def format_date(iso_str: str | None) -> str:
    """Format ISO date string to readable format."""
    if not iso_str:
        return ""
    try:
        dt = datetime.fromisoformat(iso_str.replace("Z", "+00:00"))
        return dt.strftime("%Y-%m-%d %H:%M")
    except Exception:
        return iso_str[:16] if iso_str else ""


def extract_id(url_or_id: str) -> str:
    """Extract Notion ID from URL or return as-is."""
    if "notion.so" in url_or_id or "notion.site" in url_or_id:
        parts = url_or_id.rstrip("/").split("/")
        last = parts[-1]
        if "-" in last:
            last = last.split("-")[-1]
        if "?" in last:
            last = last.split("?")[0]
        return last
    return url_or_id.replace("-", "")


# -----------------------------------------------------------------------------
# User commands
# -----------------------------------------------------------------------------


@app.command()
def me():
    """Show authenticated bot info."""
    client = get_client()
    user = client.me()
    console.print(f"[bold]{user.get('name')}[/] ({user.get('type')})")
    console.print(f"ID: {user.get('id')}")


@app.command()
def users(
    limit: int = typer.Option(50, "--limit", "-n", help="Max results"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """List workspace users."""
    client = get_client()
    result = client.users(page_size=limit)
    users_list = result.get("results", [])

    if not users_list:
        console.print("[yellow]No users found.[/]")
        raise typer.Exit()

    if json_output:
        print(json.dumps(users_list, indent=2, default=str), file=sys.stdout)
        raise typer.Exit()

    table = Table(title=f"Users ({len(users_list)})")
    table.add_column("Name", style="cyan", max_width=25)
    table.add_column("Type", style="green", max_width=10)
    table.add_column("Email", style="white", max_width=35)

    for user in users_list:
        email = user.get("person", {}).get("email", "") if user.get("type") == "person" else ""
        table.add_row(user.get("name", ""), user.get("type", ""), email)

    console.print(table)


# -----------------------------------------------------------------------------
# Search
# -----------------------------------------------------------------------------


@app.command()
def search(
    query: str = typer.Argument(None, help="Search query (optional)"),
    filter_type: str = typer.Option(None, "--type", "-t", help="Filter: 'page' or 'database'"),
    limit: int = typer.Option(25, "--limit", "-n", help="Max results"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """Search pages and databases by title.

    Examples:
        notion search "Meeting Notes"
        notion search --type database
        notion search "Project" -t page
    """
    client = get_client()
    result = client.search(query=query, filter_type=filter_type, page_size=limit)
    items = result.get("results", [])

    if not items:
        console.print("[yellow]No results found.[/]")
        raise typer.Exit()

    if json_output:
        print(json.dumps(items, indent=2, default=str), file=sys.stdout)
        raise typer.Exit()

    table = Table(title=f"Search Results ({len(items)})")
    table.add_column("Type", style="cyan", max_width=10)
    table.add_column("Title", style="white", max_width=45)
    table.add_column("Last Edited", style="dim", max_width=18)
    table.add_column("ID", style="dim", max_width=36)

    for item in items:
        obj_type = item.get("object", "")
        title = client.extract_title(item)[:45]
        edited = format_date(item.get("last_edited_time"))
        table.add_row(obj_type, title, edited, item.get("id", ""))

    console.print(table)


# -----------------------------------------------------------------------------
# Database commands
# -----------------------------------------------------------------------------


@app.command("db")
def get_database(
    database_id: str = typer.Argument(..., help="Database ID or URL"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """Get database details and schema."""
    client = get_client()
    db_id = extract_id(database_id)
    result = client.database(db_id)

    if json_output:
        print(json.dumps(result, indent=2, default=str), file=sys.stdout)
        raise typer.Exit()

    title = client.extract_title(result)
    console.print(f"\n[bold cyan]{title}[/]")
    console.print(f"ID: {result.get('id')}")
    console.print(f"URL: {result.get('url')}")
    console.print(f"Inline: {result.get('is_inline', False)}")
    console.print(f"Created: {format_date(result.get('created_time'))}")
    console.print(f"Last Edited: {format_date(result.get('last_edited_time'))}")

    # Show properties schema
    props = result.get("properties", {})
    if props:
        console.print(f"\n[bold]Properties ({len(props)}):[/]")
        for name, prop in props.items():
            console.print(f"  • {name}: [green]{prop.get('type')}[/]")


@app.command("query")
def query_database(
    database_id: str = typer.Argument(..., help="Database ID or URL"),
    limit: int = typer.Option(25, "--limit", "-n", help="Max results"),
    filter_json: str = typer.Option(None, "--filter", "-f", help="Filter as JSON string"),
    sort_json: str = typer.Option(None, "--sort", "-s", help="Sort as JSON string"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """Query a database.

    Examples:
        notion query DATABASE_ID
        notion query DATABASE_ID --filter '{"property": "Status", "status": {"equals": "Done"}}'
        notion query DATABASE_ID --sort '[{"property": "Created", "direction": "descending"}]'
    """
    client = get_client()
    db_id = extract_id(database_id)

    filter_obj = json.loads(filter_json) if filter_json else None
    sort_obj = json.loads(sort_json) if sort_json else None

    result = client.query_database(db_id, filter=filter_obj, sorts=sort_obj, page_size=limit)
    pages = result.get("results", [])

    if not pages:
        console.print("[yellow]No pages found in database.[/]")
        raise typer.Exit()

    if json_output:
        print(json.dumps(pages, indent=2, default=str), file=sys.stdout)
        raise typer.Exit()

    table = Table(title=f"Database Pages ({len(pages)})")
    table.add_column("Title", style="white", max_width=40)
    table.add_column("Last Edited", style="dim", max_width=18)
    table.add_column("ID", style="dim", max_width=36)

    for page in pages:
        title = client.extract_title(page)[:40]
        edited = format_date(page.get("last_edited_time"))
        table.add_row(title, edited, page.get("id", ""))

    console.print(table)


# -----------------------------------------------------------------------------
# Page commands
# -----------------------------------------------------------------------------


@app.command("page")
def get_page(
    page_id: str = typer.Argument(..., help="Page ID or URL"),
    content: bool = typer.Option(False, "--content", "-c", help="Include page content"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """Get page details and optionally content.

    Examples:
        notion page PAGE_ID
        notion page PAGE_ID --content
        notion page https://notion.so/My-Page-abc123 --json
    """
    client = get_client()
    pg_id = extract_id(page_id)
    result = client.page(pg_id)

    if content:
        blocks = client.get_page_content(pg_id)
        result["_content"] = blocks

    if json_output:
        print(json.dumps(result, indent=2, default=str), file=sys.stdout)
        raise typer.Exit()

    title = client.extract_title(result)
    console.print(f"\n[bold cyan]{title}[/]")
    console.print(f"ID: {result.get('id')}")
    console.print(f"URL: {result.get('url')}")
    console.print(f"Created: {format_date(result.get('created_time'))}")
    console.print(f"Last Edited: {format_date(result.get('last_edited_time'))}")
    console.print(f"Archived: {result.get('archived', False)}")

    parent = result.get("parent", {})
    parent_type = parent.get("type", "")
    parent_id = parent.get(parent_type, "")
    console.print(f"Parent: {parent_type} ({parent_id})")

    if content and "_content" in result:
        blocks = result["_content"]
        console.print(f"\n[bold]Content ({len(blocks)} blocks):[/]")
        for block in blocks[:20]:
            block_type = block.get("type", "")
            block_data = block.get(block_type, {})
            text = ""
            if "rich_text" in block_data:
                text = client.extract_rich_text(block_data["rich_text"])
            elif "text" in block_data:
                text = client.extract_rich_text(block_data["text"])
            console.print(f"  [{block_type}] {text[:80]}{'...' if len(text) > 80 else ''}")
        if len(blocks) > 20:
            console.print(f"  ... and {len(blocks) - 20} more blocks")


@app.command("read")
def read_page(
    page_id: str = typer.Argument(..., help="Page ID or URL"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """Read page content as plain text.

    Examples:
        notion read PAGE_ID
        notion read https://notion.so/My-Page-abc123
    """
    client = get_client()
    pg_id = extract_id(page_id)
    blocks = client.get_page_content(pg_id)

    if json_output:
        print(json.dumps(blocks, indent=2, default=str), file=sys.stdout)
        raise typer.Exit()

    def render_block(block: dict, indent: int = 0) -> str:
        lines = []
        block_type = block.get("type", "")
        block_data = block.get(block_type, {})
        prefix = "  " * indent

        text = ""
        if "rich_text" in block_data:
            text = client.extract_rich_text(block_data["rich_text"])
        elif "text" in block_data:
            text = client.extract_rich_text(block_data["text"])

        if block_type == "paragraph":
            lines.append(f"{prefix}{text}")
        elif block_type.startswith("heading_"):
            level = int(block_type[-1])
            lines.append(f"{prefix}{'#' * level} {text}")
        elif block_type == "bulleted_list_item":
            lines.append(f"{prefix}• {text}")
        elif block_type == "numbered_list_item":
            lines.append(f"{prefix}1. {text}")
        elif block_type == "to_do":
            checked = "x" if block_data.get("checked") else " "
            lines.append(f"{prefix}[{checked}] {text}")
        elif block_type == "toggle":
            lines.append(f"{prefix}▸ {text}")
        elif block_type == "quote":
            lines.append(f"{prefix}> {text}")
        elif block_type == "callout":
            emoji = block_data.get("icon", {}).get("emoji", "💡")
            lines.append(f"{prefix}{emoji} {text}")
        elif block_type == "code":
            lang = block_data.get("language", "")
            lines.append(f"{prefix}```{lang}")
            lines.append(f"{prefix}{text}")
            lines.append(f"{prefix}```")
        elif block_type == "divider":
            lines.append(f"{prefix}---")
        elif block_type == "child_page":
            lines.append(f"{prefix}📄 [Page: {block_data.get('title', '')}]")
        elif block_type == "child_database":
            lines.append(f"{prefix}📊 [Database: {block_data.get('title', '')}]")
        elif block_type == "image":
            url = block_data.get("file", {}).get("url", "") or block_data.get("external", {}).get(
                "url", ""
            )
            lines.append(f"{prefix}[Image: {url[:50]}...]")
        elif block_type == "bookmark":
            lines.append(f"{prefix}🔗 {block_data.get('url', '')}")
        elif block_type == "table_of_contents":
            lines.append(f"{prefix}[Table of Contents]")
        else:
            if text:
                lines.append(f"{prefix}{text}")

        return "\n".join(lines)

    for block in blocks:
        rendered = render_block(block)
        if rendered.strip():
            console.print(rendered)


@app.command("create-page")
def create_page(
    title: str = typer.Argument(..., help="Page title"),
    parent_id: str = typer.Option(..., "--parent", "-p", help="Parent page or database ID"),
    parent_type: str = typer.Option("page", "--parent-type", help="'page' or 'database'"),
    content: str = typer.Option(None, "--content", "-c", help="Initial content (paragraph)"),
):
    """Create a new page.

    Examples:
        notion create-page "My Page" --parent PAGE_ID
        notion create-page "Task" --parent DATABASE_ID --parent-type database
        notion create-page "Notes" -p PAGE_ID -c "Initial content here"
    """
    client = get_client()
    pid = extract_id(parent_id)

    if parent_type == "database":
        parent = {"database_id": pid}
        properties = {"title": {"title": client.make_rich_text(title)}}
    else:
        parent = {"page_id": pid}
        properties = {"title": {"title": client.make_rich_text(title)}}

    children = []
    if content:
        children.append(client.make_paragraph_block(content))

    result = client.create_page(parent, properties, children=children if children else None)
    console.print(f"[green]Created:[/] [bold]{client.extract_title(result)}[/]")
    console.print(f"ID: {result.get('id')}")
    console.print(f"URL: {result.get('url')}")


@app.command("append")
def append_content(
    page_id: str = typer.Argument(..., help="Page ID or URL"),
    text: str = typer.Argument(..., help="Text to append"),
    block_type: str = typer.Option(
        "paragraph",
        "--type",
        "-t",
        help="Block type: paragraph, heading1, heading2, heading3, bullet, todo",
    ),
):
    """Append content to a page.

    Examples:
        notion append PAGE_ID "New paragraph text"
        notion append PAGE_ID "Heading" --type heading1
        notion append PAGE_ID "Task item" -t todo
    """
    client = get_client()
    pg_id = extract_id(page_id)

    if block_type == "paragraph":
        block = client.make_paragraph_block(text)
    elif block_type.startswith("heading"):
        level = int(block_type[-1]) if block_type[-1].isdigit() else 1
        block = client.make_heading_block(text, level)
    elif block_type == "bullet":
        block = client.make_bullet_block(text)
    elif block_type == "todo":
        block = client.make_todo_block(text)
    else:
        block = client.make_paragraph_block(text)

    client.append_block_children(pg_id, [block])
    console.print(f"[green]Appended {block_type} block to page[/]")


# -----------------------------------------------------------------------------
# Block commands
# -----------------------------------------------------------------------------


@app.command("blocks")
def get_blocks(
    block_id: str = typer.Argument(..., help="Block or page ID"),
    limit: int = typer.Option(50, "--limit", "-n", help="Max results"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """Get child blocks of a page or block.

    Examples:
        notion blocks PAGE_ID
        notion blocks BLOCK_ID --json
    """
    client = get_client()
    bid = extract_id(block_id)
    result = client.block_children(bid, page_size=limit)
    blocks = result.get("results", [])

    if not blocks:
        console.print("[yellow]No blocks found.[/]")
        raise typer.Exit()

    if json_output:
        print(json.dumps(blocks, indent=2, default=str), file=sys.stdout)
        raise typer.Exit()

    table = Table(title=f"Blocks ({len(blocks)})")
    table.add_column("Type", style="cyan", max_width=20)
    table.add_column("Content", style="white", max_width=50)
    table.add_column("Has Children", style="dim", max_width=12)

    for block in blocks:
        block_type = block.get("type", "")
        block_data = block.get(block_type, {})
        text = ""
        if "rich_text" in block_data:
            text = client.extract_rich_text(block_data["rich_text"])[:50]
        elif "text" in block_data:
            text = client.extract_rich_text(block_data["text"])[:50]
        has_children = "Yes" if block.get("has_children") else ""
        table.add_row(block_type, text, has_children)

    console.print(table)


@app.command("block")
def get_block(
    block_id: str = typer.Argument(..., help="Block ID"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """Get a single block."""
    client = get_client()
    bid = extract_id(block_id)
    result = client.block(bid)

    if json_output:
        print(json.dumps(result, indent=2, default=str), file=sys.stdout)
        raise typer.Exit()

    block_type = result.get("type", "")
    console.print(f"Type: [cyan]{block_type}[/]")
    console.print(f"ID: {result.get('id')}")
    console.print(f"Has Children: {result.get('has_children', False)}")
    console.print(f"Created: {format_date(result.get('created_time'))}")

    block_data = result.get(block_type, {})
    if "rich_text" in block_data:
        text = client.extract_rich_text(block_data["rich_text"])
        console.print(f"Content: {text}")


@app.command("delete-block")
def delete_block(
    block_id: str = typer.Argument(..., help="Block ID to delete"),
):
    """Delete (archive) a block."""
    client = get_client()
    bid = extract_id(block_id)
    client.delete_block(bid)
    console.print(f"[green]Deleted block {bid}[/]")


# -----------------------------------------------------------------------------
# Comments
# -----------------------------------------------------------------------------


@app.command("comments")
def get_comments(
    block_id: str = typer.Argument(..., help="Block or page ID"),
    limit: int = typer.Option(25, "--limit", "-n", help="Max results"),
    json_output: bool = typer.Option(False, "--json", help="Output as JSON"),
):
    """Get comments on a block or page."""
    client = get_client()
    bid = extract_id(block_id)
    result = client.comments(block_id=bid, page_size=limit)
    comments = result.get("results", [])

    if not comments:
        console.print("[yellow]No comments found.[/]")
        raise typer.Exit()

    if json_output:
        print(json.dumps(comments, indent=2, default=str), file=sys.stdout)
        raise typer.Exit()

    for comment in comments:
        text = client.extract_rich_text(comment.get("rich_text", []))
        created = format_date(comment.get("created_time"))
        console.print(f"[dim]{created}[/] {text}")


@app.command("comment")
def add_comment(
    page_id: str = typer.Argument(..., help="Page ID"),
    text: str = typer.Argument(..., help="Comment text"),
):
    """Add a comment to a page."""
    client = get_client()
    pid = extract_id(page_id)
    result = client.create_comment(
        parent={"page_id": pid},
        rich_text=client.make_rich_text(text),
    )
    console.print("[green]Comment added[/]")
    console.print(f"ID: {result.get('id')}")


# -----------------------------------------------------------------------------
# Archive/Restore
# -----------------------------------------------------------------------------


@app.command("archive")
def archive_page(
    page_id: str = typer.Argument(..., help="Page ID to archive"),
):
    """Archive (trash) a page."""
    client = get_client()
    pid = extract_id(page_id)
    client.archive_page(pid)
    console.print(f"[green]Archived page {pid}[/]")


@app.command("restore")
def restore_page(
    page_id: str = typer.Argument(..., help="Page ID to restore"),
):
    """Restore a page from trash."""
    client = get_client()
    pid = extract_id(page_id)
    client.restore_page(pid)
    console.print(f"[green]Restored page {pid}[/]")


if __name__ == "__main__":
    app()
