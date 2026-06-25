"""CLI for GSuite operations - Gmail, Calendar, Drive."""

import json
from pathlib import Path

import typer
from centaur_sdk import Table
from rich.console import Console

app = typer.Typer(name="gsuite", help="GSuite CLI for AI agents - Gmail, Calendar, Drive")


@app.command("health")
def health():
    """Assert gsuite connectivity and auth with a safe read-only check."""
    from .client import _client

    client = _client()
    try:
        details = client.gmail_labels()
        payload = {"ok": True, "tool": "gsuite", "error": None, "details": details}
    except Exception as exc:
        payload = {"ok": False, "tool": "gsuite", "error": str(exc), "details": {}}
        print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))
        raise typer.Exit(1) from exc
    finally:
        close = getattr(client, "close", None)
        if callable(close):
            close()
    print(json.dumps(payload, indent=2, ensure_ascii=False, default=str))


console = Console()

# Sub-apps for each service
gmail_app = typer.Typer(help="Gmail operations")
calendar_app = typer.Typer(help="Calendar operations")
drive_app = typer.Typer(help="Drive operations")
docs_app = typer.Typer(help="Google Docs operations")
sheets_app = typer.Typer(help="Google Sheets operations")
slides_app = typer.Typer(help="Google Slides operations")
analytics_app = typer.Typer(help="Google Analytics operations")

app.add_typer(gmail_app, name="gmail")
app.add_typer(calendar_app, name="calendar")
app.add_typer(drive_app, name="drive")
app.add_typer(docs_app, name="docs")
app.add_typer(sheets_app, name="sheets")
app.add_typer(slides_app, name="slides")
app.add_typer(analytics_app, name="analytics")


@app.callback()
def main():
    """GSuite CLI for AI agents - Gmail, Calendar, Drive.

    Authentication is handled transparently by iron-proxy's ``gcp_auth``
    transform, which mints a service-account token for outbound Google API
    requests. There is no client-side login step.
    """


# Gmail commands


@gmail_app.command("search")
def gmail_search(
    query: str = typer.Argument(..., help="Gmail search query"),
    limit: int = typer.Option(20, "--limit", "-n", help="Max results"),
    full: bool = typer.Option(False, "--full", "-f", help="Show full snippets"),
):
    """Search Gmail messages.

    Examples:
        gsuite gmail search "from:someone@example.com"
        gsuite gmail search "subject:invoice" -n 10
        gsuite gmail search "is:unread" --full
    """
    from .client import gmail_search as search

    results = search(query, max_results=limit)

    if not results:
        console.print("[yellow]No messages found.[/]")
        raise typer.Exit()

    if full:
        for msg in results:
            console.print(f"\n[bold cyan]{msg['subject']}[/]")
            console.print(f"[green]{msg['from']}[/] | {msg['date']}")
            console.print(f"{msg['snippet']}")
            console.print(f"[dim]ID: {msg['id']}[/]")
    else:
        table = Table(title=f"Gmail: '{query}' ({len(results)} results)")
        table.add_column("From", style="green", max_width=25)
        table.add_column("Subject", style="cyan", max_width=40)
        table.add_column("Date", style="dim", max_width=20)

        for msg in results:
            from_addr = msg["from"][:25] if len(msg["from"]) > 25 else msg["from"]
            subject = msg["subject"][:40] if len(msg["subject"]) > 40 else msg["subject"]
            table.add_row(from_addr, subject, msg["date"][:20])

        console.print(table)


@gmail_app.command("read")
def gmail_read(
    message_id: str = typer.Argument(..., help="Message ID"),
):
    """Read a specific Gmail message.

    Examples:
        gsuite gmail read "18d1234567890abc"
    """
    from .client import gmail_read as read

    try:
        msg = read(message_id)
        console.print(f"\n[bold cyan]{msg['subject']}[/]")
        console.print(f"[green]From:[/] {msg['from']}")
        console.print(f"[green]To:[/] {msg['to']}")
        if msg["cc"]:
            console.print(f"[green]CC:[/] {msg['cc']}")
        console.print(f"[green]Date:[/] {msg['date']}")
        console.print(f"\n{msg['body']}")
    except Exception as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)


@gmail_app.command("send")
def gmail_send(
    to: str = typer.Argument(..., help="Recipient email"),
    subject: str = typer.Argument(..., help="Email subject"),
    body: str = typer.Argument(..., help="Email body"),
    cc: str = typer.Option(None, "--cc", help="CC recipients"),
):
    """Send an email.

    Examples:
        gsuite gmail send "someone@example.com" "Hello" "Message body"
        gsuite gmail send "a@b.com" "Subject" "Body" --cc "c@d.com"
    """
    from .client import gmail_send as send

    try:
        result = send(to, subject, body, cc=cc)
        console.print("[green]✓ Email sent[/]")
        console.print(f"[dim]ID: {result['id']}[/]")
    except Exception as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)


@gmail_app.command("labels")
def gmail_labels():
    """List Gmail labels."""
    from .client import gmail_labels as labels

    results = labels()

    table = Table(title=f"Gmail Labels ({len(results)})")
    table.add_column("Name", style="cyan")
    table.add_column("Type", style="dim")
    table.add_column("ID", style="dim", max_width=30)

    for label in sorted(results, key=lambda x: x["name"]):
        table.add_row(label["name"], label["type"], label["id"])

    console.print(table)


@gmail_app.command("archive")
def gmail_archive_cmd(
    message_ids: list[str] = typer.Argument(..., help="Message IDs to archive"),
):
    """Archive Gmail messages (remove from inbox).

    Examples:
        gsuite gmail archive "18d1234567890abc"
        gsuite gmail archive "id1" "id2" "id3"
    """
    from .client import gmail_archive

    try:
        result = gmail_archive(message_ids)
        console.print(f"[green]✓ Archived {result['archived']} message(s)[/]")
    except Exception as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)


@gmail_app.command("delete")
def gmail_delete_cmd(
    message_ids: list[str] = typer.Argument(..., help="Message IDs to delete"),
):
    """Delete Gmail messages (move to trash).

    Examples:
        gsuite gmail delete "18d1234567890abc"
        gsuite gmail delete "id1" "id2" "id3"
    """
    from .client import gmail_delete

    try:
        result = gmail_delete(message_ids)
        console.print(f"[green]✓ Deleted {result['deleted']} message(s)[/]")
    except Exception as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)


@gmail_app.command("reply")
def gmail_reply_cmd(
    message_id: str = typer.Argument(..., help="Message ID to reply to"),
    body: str = typer.Argument(..., help="Reply body"),
    attachments: list[str] = typer.Option(None, "--attach", "-a", help="File paths to attach"),
):
    """Reply to a Gmail message with optional attachments.

    Examples:
        gsuite gmail reply "18d1234567890abc" "Thanks for the update!"
        gsuite gmail reply "id" "See attached" -a "/path/to/file.pdf"
        gsuite gmail reply "id" "Files attached" -a file1.pdf -a file2.png
    """
    from .client import gmail_reply

    try:
        result = gmail_reply(message_id, body, attachments=attachments)
        console.print("[green]✓ Reply sent[/]")
        console.print(f"[dim]ID: {result['id']}[/]")
    except Exception as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)


# Calendar commands


@calendar_app.command("list")
def calendar_list():
    """List all calendars."""
    from .client import calendar_list as list_cals

    results = list_cals()

    table = Table(title=f"Calendars ({len(results)})")
    table.add_column("Name", style="cyan")
    table.add_column("Primary", style="green")
    table.add_column("Access", style="dim")

    for cal in results:
        primary = "✓" if cal["primary"] else ""
        table.add_row(cal["summary"], primary, cal["access_role"])

    console.print(table)


@calendar_app.command("events")
def calendar_events(
    limit: int = typer.Option(20, "--limit", "-n", help="Max results"),
    calendar: str = typer.Option("primary", "--calendar", "-c", help="Calendar ID"),
    query: str = typer.Option(None, "--query", "-q", help="Search query"),
    days: int = typer.Option(None, "--days", "-d", help="Look ahead N days"),
    start: str = typer.Option(None, "--start", "-s", help="Start date (YYYY-MM-DD or ISO8601)"),
    end: str = typer.Option(None, "--end", "-e", help="End date (YYYY-MM-DD or ISO8601)"),
):
    """List calendar events.

    Examples:
        gsuite calendar events
        gsuite calendar events -n 10 -d 7
        gsuite calendar events --query "meeting"
        gsuite calendar events --start 2025-01-01 --end 2025-12-31
        gsuite calendar events -c dan@paradigm.xyz -s 2025-01-01 -e 2025-06-30
    """
    from datetime import datetime, timedelta, timezone

    from .client import calendar_events as list_events
    from .client import calendar_get_timezone

    # Get the target calendar's timezone for accurate date filtering
    # This handles cases where users are traveling or in different timezones
    try:
        calendar_tz = calendar_get_timezone(calendar)
    except Exception:
        calendar_tz = None

    def parse_date(date_str: str, tz_name: str | None, end_of_day: bool = False) -> str:
        """Parse date string to ISO8601 format using the target calendar's timezone.

        Uses the calendar owner's timezone (e.g., Matt's calendar in America/Los_Angeles)
        so date boundaries work correctly regardless of where the server runs or if the
        user is traveling.
        """
        from zoneinfo import ZoneInfo

        if "T" in date_str:
            # Already has time component - if no TZ specified, assume calendar's TZ
            if not date_str.endswith("Z") and "+" not in date_str and "-" not in date_str[10:]:
                if tz_name:
                    dt = datetime.fromisoformat(date_str)
                    dt = dt.replace(tzinfo=ZoneInfo(tz_name))
                    return dt.isoformat()
            return date_str

        # Date only - use calendar's timezone for accurate date boundaries
        if tz_name:
            tz = ZoneInfo(tz_name)
            if end_of_day:
                dt = datetime.strptime(date_str, "%Y-%m-%d").replace(
                    hour=23, minute=59, second=59, tzinfo=tz
                )
            else:
                dt = datetime.strptime(date_str, "%Y-%m-%d").replace(
                    hour=0, minute=0, second=0, tzinfo=tz
                )
            return dt.isoformat()
        else:
            # Fallback to UTC if we can't get calendar timezone
            if end_of_day:
                return f"{date_str}T23:59:59Z"
            return f"{date_str}T00:00:00Z"

    time_min = None
    time_max = None

    if start:
        time_min = parse_date(start, calendar_tz)
    if end:
        time_max = parse_date(end, calendar_tz, end_of_day=True)
    elif days:
        time_max = (datetime.now(timezone.utc) + timedelta(days=days)).isoformat()

    results = list_events(
        calendar_id=calendar,
        max_results=limit,
        query=query,
        time_min=time_min,
        time_max=time_max,
    )

    if not results:
        console.print("[yellow]No events found.[/]")
        raise typer.Exit()

    if start and end:
        title = f"Events {start} to {end} ({len(results)})"
    elif start:
        title = f"Events from {start} ({len(results)})"
    elif end:
        title = f"Events until {end} ({len(results)})"
    else:
        title = f"Upcoming Events ({len(results)})"
    table = Table(title=title)
    table.add_column("Date", style="green", max_width=20)
    table.add_column("Event", style="cyan", no_wrap=False)
    table.add_column("Location", style="dim", max_width=30)

    for event in results:
        start = event["start"][:16].replace("T", " ") if "T" in event["start"] else event["start"]
        summary = event["summary"]
        location = (event["location"] or "")[:30]
        table.add_row(start, summary, location)

    console.print(table)


@calendar_app.command("create")
def calendar_create(
    summary: str = typer.Argument(..., help="Event title"),
    start: str = typer.Argument(..., help="Start time (YYYY-MM-DDTHH:MM:SSZ or YYYY-MM-DD)"),
    end: str = typer.Argument(..., help="End time"),
    calendar: str = typer.Option("primary", "--calendar", "-c", help="Calendar ID"),
    description: str = typer.Option(None, "--description", "-d", help="Event description"),
    location: str = typer.Option(None, "--location", "-l", help="Event location"),
    attendees: str = typer.Option(None, "--attendees", "-a", help="Comma-separated emails"),
):
    """Create a calendar event.

    Examples:
        gsuite calendar create "Team Meeting" "2024-01-15T10:00:00Z" "2024-01-15T11:00:00Z"
        gsuite calendar create "All-day event" "2024-01-15" "2024-01-16"
        gsuite calendar create "Meeting" "..." "..." -a "a@b.com,c@d.com" -l "Room 1"
    """
    from .client import calendar_create_event

    attendee_list = [a.strip() for a in attendees.split(",")] if attendees else None

    try:
        result = calendar_create_event(
            summary=summary,
            start=start,
            end=end,
            calendar_id=calendar,
            description=description,
            location=location,
            attendees=attendee_list,
        )
        console.print("[green]✓ Event created[/]")
        console.print(f"[dim]{result['html_link']}[/]")
    except Exception as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)


@calendar_app.command("update")
def calendar_update(
    event_id: str = typer.Argument(..., help="Event ID"),
    calendar: str = typer.Option("primary", "--calendar", "-c", help="Calendar ID"),
    summary: str = typer.Option(None, "--summary", "-s", help="New event title"),
    start: str = typer.Option(None, "--start", help="New start time"),
    end: str = typer.Option(None, "--end", help="New end time"),
    description: str = typer.Option(None, "--description", "-d", help="New description"),
    location: str = typer.Option(None, "--location", "-l", help="New location"),
    add_attendees: str = typer.Option(None, "--add", "-a", help="Comma-separated emails to add"),
):
    """Update a calendar event.

    Examples:
        gsuite calendar update "event_id" --summary "New Title"
        gsuite calendar update "event_id" --start "2024-01-15T14:00:00Z" --end "2024-01-15T15:00:00Z"
        gsuite calendar update "event_id" --add "a@b.com,c@d.com"
    """
    from .client import calendar_update_event

    attendee_list = [a.strip() for a in add_attendees.split(",")] if add_attendees else None

    try:
        result = calendar_update_event(
            event_id=event_id,
            calendar_id=calendar,
            summary=summary,
            start=start,
            end=end,
            description=description,
            location=location,
            add_attendees=attendee_list,
        )
        console.print("[green]✓ Event updated[/]")
        console.print(f"[dim]{result['html_link']}[/]")
    except Exception as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)


@calendar_app.command("rsvp")
def calendar_rsvp_cmd(
    event_id: str = typer.Argument(..., help="Event ID"),
    response: str = typer.Argument(..., help="Response: accepted, declined, tentative"),
    calendar: str = typer.Option("primary", "--calendar", "-c", help="Calendar ID"),
):
    """RSVP to a calendar event.

    Examples:
        gsuite calendar rsvp "event_id" accepted
        gsuite calendar rsvp "event_id" declined
        gsuite calendar rsvp "event_id" tentative
    """
    from .client import calendar_rsvp

    if response not in ("accepted", "declined", "tentative"):
        console.print("[red]Response must be: accepted, declined, or tentative[/]")
        raise typer.Exit(1)

    try:
        result = calendar_rsvp(event_id=event_id, response=response, calendar_id=calendar)
        console.print(f"[green]✓ RSVP: {result['status']}[/]")
    except Exception as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)


# Drive commands


@drive_app.command("list")
def drive_list(
    limit: int = typer.Option(50, "--limit", "-n", help="Max results"),
    folder: str = typer.Option(None, "--folder", "-f", help="Folder ID to list"),
    query: str = typer.Option(None, "--query", "-q", help="Search by name"),
    file_type: str = typer.Option(None, "--type", "-t", help="Filter by MIME type"),
):
    """List files in Google Drive.

    Examples:
        gsuite drive list
        gsuite drive list -q "report"
        gsuite drive list --folder "1234abc" -n 20
        gsuite drive list --type "application/pdf"
    """
    from .client import drive_list as list_files

    results = list_files(
        query=query,
        folder_id=folder,
        max_results=limit,
        file_type=file_type,
    )

    if not results:
        console.print("[yellow]No files found.[/]")
        raise typer.Exit()

    table = Table(title=f"Drive Files ({len(results)})")
    table.add_column("Name", style="cyan", max_width=40)
    table.add_column("Type", style="dim", max_width=20)
    table.add_column("Size", style="green", justify="right", max_width=10)
    table.add_column("Modified", style="dim", max_width=20)

    for f in results:
        name = f["name"][:40]
        mime = f["mime_type"].split("/")[-1][:20]
        size = f"{f['size'] / 1024:.1f} KB" if f["size"] else "-"
        modified = f["modified_time"][:10] if f["modified_time"] else ""
        table.add_row(name, mime, size, modified)

    console.print(table)


@drive_app.command("download")
def drive_download(
    file_id: str = typer.Argument(..., help="File ID"),
    output: str = typer.Option(".", "--output", "-o", help="Output directory or path"),
):
    """Download a file from Google Drive.

    Examples:
        gsuite drive download "1abc123"
        gsuite drive download "1abc123" -o /tmp/myfile.pdf
    """
    from pathlib import Path

    from .client import _drive_download_bytes, drive_get

    try:
        file_info = drive_get(file_id)
        output_path = Path(output)

        if output_path.is_dir():
            output_path = output_path / file_info["name"]

        _metadata, data = _drive_download_bytes(file_id)
        output_path.write_bytes(data)
        console.print(f"[green]✓ Downloaded {file_info['name']}[/]")
        console.print(f"[dim]{output_path.absolute()}[/]")
    except Exception as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)


@drive_app.command("upload")
def drive_upload(
    file_path: str = typer.Argument(..., help="Local file path"),
    channel: str = typer.Option(
        ..., "--channel", "-c", help="Slack channel to share with (shares with all members)"
    ),
    requester_slack_id: str = typer.Option(
        ..., "--requester", "-r", help="Slack user ID to transfer ownership to (e.g., U123ABC)"
    ),
    name: str = typer.Option(None, "--name", "-n", help="File name in Drive"),
    folder: str = typer.Option(None, "--folder", "-f", help="Parent folder ID"),
    sheets: bool = typer.Option(
        False, "--sheets", "-s", help="Convert CSV to Google Sheets (opens directly in Sheets)"
    ),
):
    """Upload a file to Google Drive with Slack channel permissions.

    Uploads the file, shares with all channel members, and transfers ownership
    to the requester.

    Examples:
        gsuite drive upload report.pdf -c general -r U123ABC
        gsuite drive upload report.pdf -c general -r U123ABC -n "Q4 Report.pdf"
        gsuite drive upload data.csv -c general -r U123ABC --sheets
    """
    import base64
    import subprocess
    import json
    from .client import drive_upload as upload, drive_setup_channel_permissions

    def get_channel_member_emails_via_cli(channel_name: str) -> list[str]:
        """Get channel member emails via slack CLI subprocess."""
        try:
            result = subprocess.run(
                ["slack", "channel-emails", channel_name, "-o", "json"],
                capture_output=True,
                text=True,
                check=True,
            )
            data = json.loads(result.stdout)
            return data.get("emails", [])
        except (subprocess.CalledProcessError, json.JSONDecodeError, FileNotFoundError) as e:
            console.print(f"[red]Error calling slack CLI: {e}[/]")
            return []

    def get_user_email_via_cli(slack_user_id: str) -> str | None:
        """Get user email via slack CLI subprocess."""
        try:
            result = subprocess.run(
                ["slack", "user-info", slack_user_id, "-o", "json"],
                capture_output=True,
                text=True,
                check=True,
            )
            data = json.loads(result.stdout)
            return data.get("email")
        except (subprocess.CalledProcessError, json.JSONDecodeError, FileNotFoundError):
            return None

    try:
        # Upload the file (optionally converting to Google Sheets)
        path = Path(file_path)
        result = upload(
            content_base64=base64.b64encode(path.read_bytes()).decode("ascii"),
            name=name,
            filename=path.name,
            folder_id=folder,
            convert_to_sheets=sheets,
        )
        file_type = "spreadsheet" if sheets else "file"
        console.print(f"[green]✓ Uploaded {result['name']} as {file_type}[/]")
        console.print(f"[dim]{result['web_view_link']}[/]", soft_wrap=True)

        file_id = result.get("id")
        if not file_id:
            console.print("[red]Error: Could not get file ID from upload result[/]")
            raise typer.Exit(1)

        # Get channel member emails (optional - sharing will be skipped if none found)
        member_emails = get_channel_member_emails_via_cli(channel)
        if not member_emails:
            console.print(
                f"[yellow]Warning: No members with emails found in channel {channel}, skipping sharing[/]"
            )

        # Get requester email from Slack user ID (optional - ownership transfer will be skipped if not found)
        requester_email = get_user_email_via_cli(requester_slack_id)
        if not requester_email:
            console.print(
                f"[yellow]Warning: Could not get email for Slack user {requester_slack_id}, skipping ownership transfer[/]"
            )

        # Set up permissions (only if there's something to do)
        if member_emails or requester_email:
            perm_result = drive_setup_channel_permissions(
                file_id=file_id,
                channel_member_emails=member_emails,
                requester_email=requester_email,
            )

            if perm_result.get("shared_with"):
                console.print(
                    f"[green]✓ Shared with {len(perm_result['shared_with'])} channel members[/]"
                )
            if perm_result.get("new_owner"):
                console.print(f"[green]✓ Ownership transferred to {perm_result['new_owner']}[/]")

            if perm_result.get("share_errors"):
                for err in perm_result["share_errors"]:
                    console.print(f"[yellow]  Warning: {err['email']}: {err['error']}[/]")
            if perm_result.get("ownership_error"):
                console.print(
                    f"[yellow]Warning: Could not transfer ownership: {perm_result['ownership_error']}[/]"
                )

    except Exception as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)


@drive_app.command("create-folder")
def drive_create_folder_cmd(
    name: str = typer.Argument(..., help="Folder name"),
    parent: str = typer.Option(None, "--parent", "-p", help="Parent folder ID"),
):
    """Create a Google Drive folder.

    Examples:
        gsuite drive create-folder "True Anomaly - Series D Financing"
        gsuite drive create-folder "Closing Docs" --parent "1abc123"
    """
    from .client import drive_create_folder

    try:
        result = drive_create_folder(name, parent_id=parent)
        console.print(f"[green]✓ Created folder {result['name']}[/]")
        console.print(f"[dim]{result['web_view_link']}[/]", soft_wrap=True)
    except Exception as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)


@drive_app.command("info")
def drive_info(
    file_id: str = typer.Argument(..., help="File ID"),
):
    """Get file info from Google Drive.

    Examples:
        gsuite drive info "1abc123"
    """
    from .client import drive_get

    try:
        f = drive_get(file_id)
        console.print(f"[bold cyan]{f['name']}[/]")
        console.print(f"[green]Type:[/] {f['mime_type']}")
        console.print(f"[green]Size:[/] {f['size'] / 1024:.1f} KB")
        console.print(f"[green]Modified:[/] {f['modified_time']}")
        if f["owners"]:
            console.print(f"[green]Owner:[/] {f['owners'][0]}")
        console.print(f"[green]Link:[/] {f['web_view_link']}", soft_wrap=True)
    except Exception as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)


@drive_app.command("permissions")
def drive_permissions_cmd(
    file_id: str = typer.Argument(..., help="File ID"),
):
    """List permissions on a Google Drive file.

    Examples:
        gsuite drive permissions "1abc123"
    """
    from .client import drive_list_permissions

    try:
        permissions = drive_list_permissions(file_id)

        if not permissions:
            console.print("[yellow]No permissions found.[/]")
            raise typer.Exit()

        table = Table(title=f"Permissions ({len(permissions)})")
        table.add_column("Email", style="cyan", max_width=35)
        table.add_column("Role", style="green", max_width=15)
        table.add_column("Type", style="dim", max_width=10)

        for p in permissions:
            table.add_row(p["email"] or p["display_name"] or "(anyone)", p["role"], p["type"])

        console.print(table)
    except Exception as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)


@drive_app.command("share")
def drive_share_cmd(
    file_id: str = typer.Argument(..., help="File ID"),
    email: str = typer.Argument(..., help="Email address to share with"),
    role: str = typer.Option(
        "writer", "--role", "-r", help="Permission role: reader, writer, commenter"
    ),
    notify: bool = typer.Option(False, "--notify", "-n", help="Send email notification"),
):
    """Share a Google Drive file with a user.

    Examples:
        gsuite drive share "1abc123" "user@example.com"
        gsuite drive share "1abc123" "user@example.com" --role reader
        gsuite drive share "1abc123" "user@example.com" --notify
    """
    from .client import drive_share

    if role not in ("reader", "writer", "commenter"):
        console.print("[red]Role must be: reader, writer, or commenter[/]")
        raise typer.Exit(1)

    try:
        result = drive_share(file_id, email, role=role, send_notification=notify)
        console.print(f"[green]✓ Shared with {result['email']} as {result['role']}[/]")
    except Exception as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)


@drive_app.command("transfer-ownership")
def drive_transfer_ownership_cmd(
    file_id: str = typer.Argument(..., help="File ID"),
    new_owner: str = typer.Argument(..., help="Email address of new owner"),
):
    """Transfer ownership of a Google Drive file.

    Note: Both users must be in the same Google Workspace domain.

    Examples:
        gsuite drive transfer-ownership "1abc123" "newowner@example.com"
    """
    from .client import drive_transfer_ownership

    try:
        result = drive_transfer_ownership(file_id, new_owner)
        console.print(f"[green]✓ Ownership transferred to {result['email']}[/]")
    except Exception as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)


@drive_app.command("unshare")
def drive_unshare_cmd(
    file_id: str = typer.Argument(..., help="File ID"),
    email: str = typer.Argument(..., help="Email address to remove"),
):
    """Remove a user's permission from a Google Drive file.

    Examples:
        gsuite drive unshare "1abc123" "user@example.com"
    """
    from .client import drive_remove_permission

    try:
        removed = drive_remove_permission(file_id, email)
        if removed:
            console.print(f"[green]✓ Removed {email} from file[/]")
        else:
            console.print(f"[yellow]User {email} did not have access to the file[/]")
    except Exception as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)


@drive_app.command("setup-permissions")
def drive_setup_permissions_cmd(
    file_id: str = typer.Argument(..., help="File ID"),
    emails: str = typer.Argument(..., help="Comma-separated list of email addresses to share with"),
    owner: str = typer.Argument(..., help="Email address of the new owner"),
):
    """Set up permissions for channel members and transfer ownership.

    This command:
    1. Shares the file with all provided email addresses (writer role)
    2. Transfers ownership to the specified owner

    The original owner (service account) is automatically downgraded to editor
    by Google Drive when ownership is transferred. An Okta Workflows configuration
    removes the service account's editor role permissions after 7 days.

    Examples:
        gsuite drive setup-permissions "1abc123" "user1@example.com,user2@example.com" "owner@example.com"
    """
    from .client import drive_setup_channel_permissions

    email_list = [e.strip() for e in emails.split(",") if e.strip()]

    try:
        result = drive_setup_channel_permissions(file_id, email_list, owner)

        if result["shared_with"]:
            console.print(f"[green]✓ Shared with {len(result['shared_with'])} users[/]")
        if result["share_errors"]:
            for err in result["share_errors"]:
                console.print(
                    f"[yellow]  Warning: Could not share with {err['email']}: {err['error']}[/]"
                )

        if result["new_owner"]:
            console.print(f"[green]✓ Ownership transferred to {result['new_owner']}[/]")
        elif result["ownership_error"]:
            console.print(f"[red]✗ Could not transfer ownership: {result['ownership_error']}[/]")

    except Exception as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)


@drive_app.command("export")
def drive_export(
    file_id: str = typer.Argument(..., help="File ID (Google Doc/Sheet/Slides)"),
    format: str = typer.Option(
        "txt",
        "--format",
        "-f",
        help="Export format: txt, pdf, docx, html, csv, xlsx, pptx",
    ),
    output: str = typer.Option(None, "--output", "-o", help="Output file path"),
):
    """Export a Google Doc/Sheet/Slides to a file format.

    For Google Docs, use 'txt' to get plain text content directly.
    Other formats will download the exported file.

    Examples:
        gsuite drive export "1abc123"                    # Export Doc as text to stdout
        gsuite drive export "1abc123" -f pdf -o doc.pdf  # Export as PDF
        gsuite drive export "1abc123" -f txt -o doc.txt  # Export as text file
        gsuite drive export "1abc123" -f csv             # Export Sheet as CSV
    """
    from .client import _drive_export_bytes, drive_get, docs_get_text

    try:
        file_info = drive_get(file_id)
        mime_type = file_info["mime_type"]

        # For Google Docs with txt format, use the docs API for clean text
        if mime_type == "application/vnd.google-apps.document" and format == "txt":
            text = docs_get_text(file_id)
            if output:
                with open(output, "w") as f:
                    f.write(text)
                console.print(f"[green]✓ Exported to {output}[/]")
            else:
                console.print(text)
            return

        # For other formats, use Drive export
        metadata, _mime_type, data = _drive_export_bytes(file_id, format)
        if output:
            with open(output, "wb") as f:
                f.write(data)
            console.print(f"[green]✓ Exported {file_info['name']} to {output}[/]")
        else:
            # If no output specified, print the content for text formats
            if format in ("txt", "csv", "html"):
                console.print(data.decode("utf-8", errors="replace"))
            else:
                console.print(f"[green]✓ Exported {metadata.get('name') or file_id}[/]")
    except Exception as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)


@drive_app.command("add-label")
def drive_add_label_cmd(
    file_id: str = typer.Argument(..., help="File ID"),
    label_id: str = typer.Argument(..., help="Label ID to apply"),
):
    """Apply a label to a Google Drive file.

    Use this to apply organizational labels (e.g., "confidential") to files.

    Examples:
        gsuite drive add-label "1abc123" "LABEL_ID"
    """
    from .client import drive_add_label

    try:
        result = drive_add_label(file_id, label_id)
        if result["applied"]:
            console.print("[green]✓ Label applied to file[/]")
        else:
            console.print("[yellow]Label may not have been applied (no modifications returned)[/]")
    except Exception as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)


@drive_app.command("remove-label")
def drive_remove_label_cmd(
    file_id: str = typer.Argument(..., help="File ID"),
    label_id: str = typer.Argument(..., help="Label ID to remove"),
):
    """Remove a label from a Google Drive file.

    Examples:
        gsuite drive remove-label "1abc123" "LABEL_ID"
    """
    from .client import drive_remove_label

    try:
        drive_remove_label(file_id, label_id)
        console.print("[green]✓ Label removed from file[/]")
    except Exception as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)


@drive_app.command("label-folder")
def drive_label_folder_cmd(
    folder_id: str = typer.Argument(..., help="Folder ID or Shared Drive ID"),
    label_id: str = typer.Argument(..., help="Label ID to apply to all files"),
    no_recursive: bool = typer.Option(
        False, "--no-recursive", help="Don't recurse into subfolders"
    ),
):
    """Apply a label to all files in a folder or Shared Drive (recursive).

    Walks through all files in the given folder (or Shared Drive) and applies
    the specified label. Useful for bulk-applying the "confidential" label.

    Examples:
        gsuite drive label-folder "FOLDER_ID" "LABEL_ID"
        gsuite drive label-folder "SHARED_DRIVE_ID" "LABEL_ID" --no-recursive
    """
    from .client import drive_label_folder

    try:
        result = drive_label_folder(folder_id, label_id, recursive=not no_recursive)
        console.print(f"[green]✓ Labeled {result['labeled']} file(s)[/]")
        if result["errors"]:
            console.print(f"[yellow]⚠ Failed to label {result['failed']} file(s):[/]")
            for err in result["errors"]:
                console.print(f"  [red]• {err['name']}: {err['error']}[/]")
        if result["files"]:
            for f in result["files"]:
                console.print(f"  [dim]✓ {f['name']}[/]")
    except Exception as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)


def extract_doc_id(doc_id_or_url: str) -> str:
    """Extract document ID from a URL or return as-is if already an ID."""
    import re

    if doc_id_or_url.startswith("http"):
        match = re.search(r"/d/([a-zA-Z0-9_-]+)", doc_id_or_url)
        if match:
            return match.group(1)
        raise ValueError(f"Could not extract document ID from URL: {doc_id_or_url}")
    return doc_id_or_url


# Docs commands


@docs_app.command("read")
def docs_read(
    doc_id: str = typer.Argument(..., help="Document ID or Google Docs URL"),
):
    """Read a Google Doc's content as plain text.

    Examples:
        gsuite docs read "1abc123"
        gsuite docs read "https://docs.google.com/document/d/1abc123/edit"
    """
    from .client import docs_get_text

    try:
        document_id = extract_doc_id(doc_id)
        text = docs_get_text(document_id)
        console.print(text)
    except Exception as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)


@docs_app.command("replace")
def docs_replace_cmd(
    doc_id: str = typer.Argument(..., help="Document ID or Google Docs URL"),
    old_text: str = typer.Argument(..., help="Text to find"),
    new_text: str = typer.Argument(..., help="Text to replace with"),
):
    """Find and replace literal text in a Google Doc.

    This only swaps characters. For actual Google Docs list formatting, use
    `gsuite docs bullets`.

    Examples:
        gsuite docs replace "1abc123" "old text" "new text"
        gsuite docs replace "https://docs.google.com/document/d/1abc123/edit" "foo" "bar"
    """
    from .client import docs_replace

    try:
        document_id = extract_doc_id(doc_id)
        result = docs_replace(document_id, old_text, new_text)
        console.print(f"[green]✓ Replaced {result['occurrences_replaced']} occurrence(s)[/]")
    except Exception as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)


@docs_app.command("bullets")
def docs_bullets_cmd(
    doc_id: str = typer.Argument(..., help="Document ID or Google Docs URL"),
    match: str = typer.Option("- ", "--match", help="Literal paragraph prefix to convert"),
    preset: str = typer.Option(
        "BULLET_DISC_CIRCLE_SQUARE",
        "--preset",
        help="Google Docs bullet preset to apply",
    ),
    tab_id: str = typer.Option(None, "--tab-id", help="Only convert paragraphs in this tab"),
    dry_run: bool = typer.Option(
        False,
        "--dry-run",
        help="Preview matching paragraphs without modifying the document",
    ),
):
    """Convert matching paragraphs into real Google Docs bullet items.

    This uses the Docs list API instead of inserting literal bullet characters.

    Examples:
        gsuite docs bullets "1abc123"
        gsuite docs bullets "1abc123" --match "• "
        gsuite docs bullets "1abc123" --tab-id "tab-123" --dry-run
    """
    from .client import docs_bullets

    try:
        document_id = extract_doc_id(doc_id)
        result = docs_bullets(
            document_id,
            match_prefix=match,
            bullet_preset=preset,
            tab_id=tab_id,
            dry_run=dry_run,
        )

        if result["matched_paragraphs"] == 0:
            console.print(f"[yellow]No paragraphs matched {match!r}.[/]")
            return

        action = "Would convert" if dry_run else "Converted"
        style = "cyan" if dry_run else "green"
        console.print(
            f"[{style}]✓ {action} {result['updated_paragraphs'] or result['matched_paragraphs']} paragraph(s) into Google Docs bullets[/]"
        )
        console.print(
            "[dim]Verification: "
            f"matched {result['matched_paragraphs']}, "
            f"updated {result['updated_paragraphs']}, "
            f"verified {result['verified_paragraphs']}, "
            f"already bulleted {result['already_bulleted_paragraphs']}[/]"
        )

        for paragraph in result["paragraphs"]:
            location = f"tab {paragraph['tab_id']} " if paragraph["tab_id"] else ""
            console.print(
                f"  [dim]{location}paragraph {paragraph['paragraph_index'] + 1}:[/] "
                f"{paragraph['before']} -> {paragraph['after']}"
            )

        if not dry_run and result["verified_paragraphs"] != result["updated_paragraphs"]:
            console.print(
                "[yellow]Verification warning: some updated paragraphs did not report bullet metadata on the follow-up read.[/]"
            )
    except Exception as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)


@docs_app.command("append")
def docs_append_cmd(
    doc_id: str = typer.Argument(..., help="Document ID or Google Docs URL"),
    text: str = typer.Argument(..., help="Text to append"),
):
    """Append text to the end of a Google Doc.

    Examples:
        gsuite docs append "1abc123" "New paragraph at the end."
    """
    from .client import docs_get, docs_batch_update

    try:
        document_id = extract_doc_id(doc_id)
        doc = docs_get(document_id)

        # Find end index from first tab or body
        if doc.get("tabs"):
            body = doc["tabs"][0].get("documentTab", {}).get("body", {})
        else:
            body = doc.get("body", {})

        content = body.get("content", [])
        end_index = content[-1].get("endIndex", 1) - 1 if content else 1

        requests = [{"insertText": {"location": {"index": end_index}, "text": text}}]
        docs_batch_update(document_id, requests)
        console.print("[green]✓ Text appended[/]")
    except Exception as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)


@docs_app.command("insert")
def docs_insert_cmd(
    doc_id: str = typer.Argument(..., help="Document ID or Google Docs URL"),
    text: str = typer.Argument(..., help="Text to insert"),
    index: int = typer.Argument(..., help="Position to insert at (1 = beginning)"),
):
    """Insert text at a specific position in a Google Doc.

    Examples:
        gsuite docs insert "1abc123" "Inserted text" 1
        gsuite docs insert "1abc123" "Middle text" 100
    """
    from .client import docs_insert

    try:
        document_id = extract_doc_id(doc_id)
        docs_insert(document_id, text, index)
        console.print(f"[green]✓ Text inserted at index {index}[/]")
    except Exception as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)


def _get_channel_member_emails_via_cli(channel: str) -> list[str]:
    """Get channel member emails by calling the slack CLI.

    This avoids importing the slack client directly, keeping packages independent.
    """
    import json
    import subprocess

    result = subprocess.run(
        ["slack", "channel-emails", channel, "-o", "json"],
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        raise RuntimeError(f"Failed to get channel members: {result.stderr}")

    data = json.loads(result.stdout)
    return data.get("emails", [])


@docs_app.command("create")
def docs_create_cmd(
    title: str = typer.Argument(..., help="Document title"),
    channel: str = typer.Option(..., "--channel", help="Slack channel to share with (required)"),
    owner: str = typer.Option(..., "--owner", help="Email of new owner (required)"),
    content: str = typer.Option(None, "--content", "-c", help="Initial content"),
):
    """Create a new Google Doc with automatic permission setup.

    This command:
    1. Creates the document
    2. Shares with all channel members (writer role)
    3. Transfers ownership to the specified owner

    The original owner (service account) is automatically downgraded to editor
    by Google Drive when ownership is transferred. An Okta Workflows configuration
    removes the service account's editor role permissions after 7 days.

    Examples:
        gsuite docs create "Meeting Notes" --channel eng-ai --owner alice@paradigm.xyz
        gsuite docs create "Doc Title" --channel ai-agent --owner bob@paradigm.xyz --content "Hello"
    """
    from .client import docs_create, drive_setup_channel_permissions

    try:
        result = docs_create(title, content)
        console.print(f"[green]✓ Created document: {result['title']}[/]")
        console.print(f"[cyan]URL: {result['url']}[/]", soft_wrap=True)
        console.print(f"[dim]ID: {result['document_id']}[/]")

        member_emails = _get_channel_member_emails_via_cli(channel)
        console.print(f"[dim]Setting up permissions for {len(member_emails)} channel members...[/]")

        perm_result = drive_setup_channel_permissions(
            file_id=result["document_id"],
            channel_member_emails=member_emails,
            requester_email=owner,
        )

        console.print(f"[green]✓ Shared with {len(perm_result['shared_with'])} channel members[/]")
        console.print(f"[green]✓ Ownership transferred to {owner}[/]")

    except Exception as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)


# Sheets commands


@sheets_app.command("read")
def sheets_read_cmd(
    spreadsheet_id: str = typer.Argument(..., help="Spreadsheet ID (from URL)"),
    range_notation: str = typer.Option("A1:Z1000", "--range", "-r", help="A1 notation range"),
    output_json: bool = typer.Option(False, "--json", "-o", help="Output as JSON"),
):
    """Read data from a Google Sheet.

    Examples:
        gsuite sheets read "1AgNeNaIVgWl7jIovJsvW-F1zIz150e-nCr4VCE56odE"
        gsuite sheets read "1Abc..." --range "Sheet1!A1:D10"
        gsuite sheets read "1Abc..." --json
    """
    import json
    from .client import sheets_read

    try:
        result = sheets_read(spreadsheet_id, range_notation)

        if output_json:
            console.print(json.dumps(result["rows"], indent=2))
            return

        if not result["rows"]:
            console.print("[yellow]No data found.[/]")
            raise typer.Exit()

        table = Table(title=f"Sheet Data ({len(result['rows'])} rows)")
        for header in result["headers"]:
            table.add_column(header, style="cyan", max_width=30)

        for row in result["rows"][:50]:
            values = [str(row.get(h, ""))[:30] for h in result["headers"]]
            table.add_row(*values)

        console.print(table)
        if len(result["rows"]) > 50:
            console.print(f"[dim]... and {len(result['rows']) - 50} more rows[/]")

    except Exception as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)


@sheets_app.command("update")
def sheets_update_cmd(
    spreadsheet_id: str = typer.Argument(..., help="Spreadsheet ID (from URL)"),
    range_notation: str = typer.Argument(..., help="A1 notation range to update"),
    values: str = typer.Argument(..., help="JSON array of arrays to write"),
):
    """Update data in a Google Sheet.

    Examples:
        gsuite sheets update "1Abc..." "B5" '[["New Value"]]'
        gsuite sheets update "1Abc..." "A1:C2" '[["A","B","C"],["1","2","3"]]'
    """
    import json
    from .client import sheets_update

    try:
        parsed_values = json.loads(values)
        result = sheets_update(spreadsheet_id, range_notation, parsed_values)
        console.print(f"[green]✓ Updated {result['updated_cells']} cells[/]")
        console.print(f"[dim]Range: {result['updated_range']}[/]")
    except json.JSONDecodeError:
        console.print("[red]Error: values must be valid JSON array of arrays[/]")
        raise typer.Exit(1)
    except Exception as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)


@sheets_app.command("create")
def sheets_create_cmd(
    title: str = typer.Argument(..., help="Spreadsheet title"),
    channel: str = typer.Option(..., "--channel", help="Slack channel to share with (required)"),
    owner: str = typer.Option(..., "--owner", help="Email of new owner (required)"),
):
    """Create a new Google Sheet with automatic permission setup.

    This command:
    1. Creates the spreadsheet
    2. Shares with all channel members (writer role)
    3. Transfers ownership to the specified owner

    The original owner (service account) is automatically downgraded to editor
    by Google Drive when ownership is transferred. An Okta Workflows configuration
    removes the service account's editor role permissions after 7 days.

    Examples:
        gsuite sheets create "My Spreadsheet" --channel eng-ai --owner alice@paradigm.xyz
    """
    from .client import sheets_create, drive_setup_channel_permissions

    try:
        result = sheets_create(title)
        console.print(f"[green]✓ Created spreadsheet: {result['title']}[/]")
        console.print(f"[cyan]URL: {result['url']}[/]", soft_wrap=True)
        console.print(f"[dim]ID: {result['spreadsheet_id']}[/]")

        member_emails = _get_channel_member_emails_via_cli(channel)
        console.print(f"[dim]Setting up permissions for {len(member_emails)} channel members...[/]")

        perm_result = drive_setup_channel_permissions(
            file_id=result["spreadsheet_id"],
            channel_member_emails=member_emails,
            requester_email=owner,
        )

        console.print(f"[green]✓ Shared with {len(perm_result['shared_with'])} channel members[/]")
        console.print(f"[green]✓ Ownership transferred to {owner}[/]")

    except Exception as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)


# Slides commands


@slides_app.command("create")
def slides_create_cmd(
    title: str = typer.Argument(..., help="Presentation title"),
    channel: str = typer.Option(..., "--channel", help="Slack channel to share with (required)"),
    owner: str = typer.Option(..., "--owner", help="Email of new owner (required)"),
):
    """Create a new Google Slides presentation with automatic permission setup.

    This command:
    1. Creates the presentation
    2. Shares with all channel members (writer role)
    3. Transfers ownership to the specified owner

    The original owner (service account) is automatically downgraded to editor
    by Google Drive when ownership is transferred. An Okta Workflows configuration
    removes the service account's editor role permissions after 7 days.

    Examples:
        gsuite slides create "My Presentation" --channel eng-ai --owner alice@paradigm.xyz
    """
    from .client import slides_create, drive_setup_channel_permissions

    try:
        result = slides_create(title)
        console.print(f"[green]✓ Created presentation: {result['title']}[/]")
        console.print(f"[cyan]URL: {result['url']}[/]", soft_wrap=True)
        console.print(f"[dim]ID: {result['presentation_id']}[/]")

        member_emails = _get_channel_member_emails_via_cli(channel)
        console.print(f"[dim]Setting up permissions for {len(member_emails)} channel members...[/]")

        perm_result = drive_setup_channel_permissions(
            file_id=result["presentation_id"],
            channel_member_emails=member_emails,
            requester_email=owner,
        )

        console.print(f"[green]✓ Shared with {len(perm_result['shared_with'])} channel members[/]")
        console.print(f"[green]✓ Ownership transferred to {owner}[/]")

    except Exception as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)


# Analytics commands

# Global for analytics property/site selection
_analytics_site: str | None = None
_analytics_property: str | None = None


def _setup_analytics_property():
    """Set up analytics property from site or property ID."""
    from .client import set_analytics_property, get_credentials
    from .analytics_properties import get_property_id_for_site

    if _analytics_site:
        # First try static lookup
        resolved_id = get_property_id_for_site(_analytics_site)
        if not resolved_id:
            # Try dynamic discovery with credentials
            try:
                creds = get_credentials()
                resolved_id = get_property_id_for_site(_analytics_site, credentials=creds)
            except Exception:
                pass

        if not resolved_id:
            console.print(f"[red]Could not find GA4 property for: {_analytics_site}[/]")
            console.print("[dim]Known sites: paradigm.xyz, predictions.paradigm.xyz[/]")
            console.print("[dim]Tip: Use 'gsuite analytics sites' to list known sites[/]")
            raise typer.Exit(1)

        console.print(f"[dim]Using property {resolved_id} for {_analytics_site}[/]")
        set_analytics_property(resolved_id)
    elif _analytics_property:
        set_analytics_property(_analytics_property)


@analytics_app.callback()
def analytics_main(
    site: str = typer.Option(None, "--site", "-s", help="Site name (e.g., 'paradigm.xyz')"),
    property_id: str = typer.Option(None, "--property", "-p", help="GA4 property ID"),
):
    """Google Analytics - query GA4 data.

    Use --site for known sites or --property for custom property IDs.

    Examples:
        gsuite analytics -s paradigm.xyz summary
        gsuite analytics -s predictions pages --start 7daysAgo
    """
    global _analytics_site, _analytics_property
    _analytics_site = site
    _analytics_property = property_id


@analytics_app.command("sites")
def analytics_sites():
    """List available site mappings."""
    from .analytics_properties import PROPERTY_MAPPINGS

    table = Table(title="Available Sites")
    table.add_column("Site / Alias", style="cyan")
    table.add_column("Property ID", style="green")

    # Group by property ID to show aliases together
    by_property: dict[str, list[str]] = {}
    for name, prop_id in PROPERTY_MAPPINGS.items():
        by_property.setdefault(prop_id, []).append(name)

    for prop_id, names in sorted(by_property.items()):
        canonical = max(names, key=len)
        aliases = [n for n in names if n != canonical]
        if aliases:
            display = f"{canonical} ({', '.join(aliases)})"
        else:
            display = canonical
        table.add_row(display, prop_id)

    console.print(table)
    console.print("\n[dim]Use --site or -s to select a site, e.g.:[/]")
    console.print("[dim]  gsuite analytics -s paradigm.xyz summary[/]")


@analytics_app.command("summary")
def analytics_summary(
    start: str = typer.Option("30daysAgo", "--start", help="Start date"),
    end: str = typer.Option("today", "--end", "-e", help="End date"),
    output_json: bool = typer.Option(False, "--json", "-o", help="Output as JSON"),
):
    """Get summary metrics for the property.

    Examples:
        gsuite analytics -s paradigm.xyz summary
        gsuite analytics -s paradigm.xyz summary --start 7daysAgo
    """
    import json
    from .client import analytics_get_summary

    _setup_analytics_property()

    try:
        result = analytics_get_summary(start_date=start, end_date=end)

        if output_json:
            console.print(json.dumps(result, indent=2))
            return

        table = Table(title=f"GA4 Summary ({start} to {end})")
        table.add_column("Metric", style="cyan")
        table.add_column("Value", style="green", justify="right")

        metric_labels = {
            "activeUsers": "Active Users",
            "newUsers": "New Users",
            "sessions": "Sessions",
            "screenPageViews": "Page Views",
            "bounceRate": "Bounce Rate",
            "averageSessionDuration": "Avg Session Duration (s)",
            "engagementRate": "Engagement Rate",
        }

        for key, value in result.items():
            label = metric_labels.get(key, key)
            if key in ("bounceRate", "engagementRate"):
                formatted = f"{float(value) * 100:.1f}%"
            elif key == "averageSessionDuration":
                formatted = f"{float(value):.1f}s"
            else:
                formatted = f"{int(float(value)):,}"
            table.add_row(label, formatted)

        console.print(table)
    except Exception as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)


@analytics_app.command("sources")
def analytics_sources(
    start: str = typer.Option("30daysAgo", "--start", help="Start date"),
    end: str = typer.Option("today", "--end", "-e", help="End date"),
    limit: int = typer.Option(20, "--limit", "-n", help="Max results"),
    output_json: bool = typer.Option(False, "--json", "-o", help="Output as JSON"),
):
    """Get traffic by source/medium.

    Examples:
        gsuite analytics -s paradigm.xyz sources
        gsuite analytics -s paradigm.xyz sources --start 7daysAgo -n 10
    """
    import json
    from .client import analytics_get_traffic_by_source

    _setup_analytics_property()

    try:
        result = analytics_get_traffic_by_source(start_date=start, end_date=end, limit=limit)

        if output_json:
            console.print(json.dumps(result["rows"], indent=2))
            return

        table = Table(title=f"Traffic by Source ({start} to {end})")
        table.add_column("Source / Medium", style="cyan")
        table.add_column("Sessions", style="green", justify="right")
        table.add_column("Users", style="green", justify="right")
        table.add_column("Bounce Rate", justify="right")
        table.add_column("Avg Duration", justify="right")

        for row in result["rows"]:
            dims = row["dimensions"]
            mets = row["metrics"]
            bounce = f"{float(mets.get('bounceRate', 0)) * 100:.1f}%"
            duration = f"{float(mets.get('averageSessionDuration', 0)):.0f}s"
            table.add_row(
                dims.get("sessionSourceMedium", ""),
                f"{int(float(mets.get('sessions', 0))):,}",
                f"{int(float(mets.get('activeUsers', 0))):,}",
                bounce,
                duration,
            )

        console.print(table)
    except Exception as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)


@analytics_app.command("channels")
def analytics_channels(
    start: str = typer.Option("30daysAgo", "--start", help="Start date"),
    end: str = typer.Option("today", "--end", "-e", help="End date"),
    limit: int = typer.Option(20, "--limit", "-n", help="Max results"),
    output_json: bool = typer.Option(False, "--json", "-o", help="Output as JSON"),
):
    """Get traffic by default channel grouping."""
    import json
    from .client import analytics_get_traffic_by_channel

    _setup_analytics_property()

    try:
        result = analytics_get_traffic_by_channel(start_date=start, end_date=end, limit=limit)

        if output_json:
            console.print(json.dumps(result["rows"], indent=2))
            return

        table = Table(title=f"Traffic by Channel ({start} to {end})")
        table.add_column("Channel", style="cyan")
        table.add_column("Sessions", style="green", justify="right")
        table.add_column("Users", style="green", justify="right")
        table.add_column("New Users", justify="right")
        table.add_column("Engaged", justify="right")

        for row in result["rows"]:
            dims = row["dimensions"]
            mets = row["metrics"]
            table.add_row(
                dims.get("sessionDefaultChannelGroup", ""),
                f"{int(float(mets.get('sessions', 0))):,}",
                f"{int(float(mets.get('activeUsers', 0))):,}",
                f"{int(float(mets.get('newUsers', 0))):,}",
                f"{int(float(mets.get('engagedSessions', 0))):,}",
            )

        console.print(table)
    except Exception as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)


@analytics_app.command("pages")
def analytics_pages(
    start: str = typer.Option("30daysAgo", "--start", help="Start date"),
    end: str = typer.Option("today", "--end", "-e", help="End date"),
    limit: int = typer.Option(20, "--limit", "-n", help="Max results"),
    output_json: bool = typer.Option(False, "--json", "-o", help="Output as JSON"),
):
    """Get top pages by views."""
    import json
    from .client import analytics_get_top_pages

    _setup_analytics_property()

    try:
        result = analytics_get_top_pages(start_date=start, end_date=end, limit=limit)

        if output_json:
            console.print(json.dumps(result["rows"], indent=2))
            return

        table = Table(title=f"Top Pages ({start} to {end})")
        table.add_column("Page Path", style="cyan", max_width=50)
        table.add_column("Views", style="green", justify="right")
        table.add_column("Users", justify="right")
        table.add_column("Avg Duration", justify="right")

        for row in result["rows"]:
            dims = row["dimensions"]
            mets = row["metrics"]
            path = dims.get("pagePath", "")
            if len(path) > 50:
                path = path[:47] + "..."
            duration = f"{float(mets.get('averageSessionDuration', 0)):.0f}s"
            table.add_row(
                path,
                f"{int(float(mets.get('screenPageViews', 0))):,}",
                f"{int(float(mets.get('activeUsers', 0))):,}",
                duration,
            )

        console.print(table)
    except Exception as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)


@analytics_app.command("devices")
def analytics_devices(
    start: str = typer.Option("30daysAgo", "--start", help="Start date"),
    end: str = typer.Option("today", "--end", "-e", help="End date"),
    output_json: bool = typer.Option(False, "--json", "-o", help="Output as JSON"),
):
    """Get traffic by device category."""
    import json
    from .client import analytics_get_traffic_by_device

    _setup_analytics_property()

    try:
        result = analytics_get_traffic_by_device(start_date=start, end_date=end)

        if output_json:
            console.print(json.dumps(result["rows"], indent=2))
            return

        table = Table(title=f"Traffic by Device ({start} to {end})")
        table.add_column("Device", style="cyan")
        table.add_column("Sessions", style="green", justify="right")
        table.add_column("Users", justify="right")
        table.add_column("Bounce Rate", justify="right")

        for row in result["rows"]:
            dims = row["dimensions"]
            mets = row["metrics"]
            bounce = f"{float(mets.get('bounceRate', 0)) * 100:.1f}%"
            table.add_row(
                dims.get("deviceCategory", ""),
                f"{int(float(mets.get('sessions', 0))):,}",
                f"{int(float(mets.get('activeUsers', 0))):,}",
                bounce,
            )

        console.print(table)
    except Exception as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)


@analytics_app.command("countries")
def analytics_countries(
    start: str = typer.Option("30daysAgo", "--start", help="Start date"),
    end: str = typer.Option("today", "--end", "-e", help="End date"),
    limit: int = typer.Option(20, "--limit", "-n", help="Max results"),
    output_json: bool = typer.Option(False, "--json", "-o", help="Output as JSON"),
):
    """Get traffic by country."""
    import json
    from .client import analytics_get_traffic_by_country

    _setup_analytics_property()

    try:
        result = analytics_get_traffic_by_country(start_date=start, end_date=end, limit=limit)

        if output_json:
            console.print(json.dumps(result["rows"], indent=2))
            return

        table = Table(title=f"Traffic by Country ({start} to {end})")
        table.add_column("Country", style="cyan")
        table.add_column("Sessions", style="green", justify="right")
        table.add_column("Users", justify="right")
        table.add_column("New Users", justify="right")

        for row in result["rows"]:
            dims = row["dimensions"]
            mets = row["metrics"]
            table.add_row(
                dims.get("country", ""),
                f"{int(float(mets.get('sessions', 0))):,}",
                f"{int(float(mets.get('activeUsers', 0))):,}",
                f"{int(float(mets.get('newUsers', 0))):,}",
            )

        console.print(table)
    except Exception as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)


@analytics_app.command("daily")
def analytics_daily(
    start: str = typer.Option("30daysAgo", "--start", help="Start date"),
    end: str = typer.Option("today", "--end", "-e", help="End date"),
    output_json: bool = typer.Option(False, "--json", "-o", help="Output as JSON"),
):
    """Get daily active users over time."""
    import json
    from .client import analytics_get_daily_users

    _setup_analytics_property()

    try:
        result = analytics_get_daily_users(start_date=start, end_date=end)

        if output_json:
            console.print(json.dumps(result["rows"], indent=2))
            return

        table = Table(title=f"Daily Users ({start} to {end})")
        table.add_column("Date", style="cyan")
        table.add_column("Active Users", style="green", justify="right")
        table.add_column("New Users", justify="right")
        table.add_column("Sessions", justify="right")

        for row in result["rows"]:
            dims = row["dimensions"]
            mets = row["metrics"]
            date_str = dims.get("date", "")
            formatted_date = f"{date_str[:4]}-{date_str[4:6]}-{date_str[6:]}"
            table.add_row(
                formatted_date,
                f"{int(float(mets.get('activeUsers', 0))):,}",
                f"{int(float(mets.get('newUsers', 0))):,}",
                f"{int(float(mets.get('sessions', 0))):,}",
            )

        console.print(table)
    except Exception as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)


@analytics_app.command("realtime")
def analytics_realtime(
    output_json: bool = typer.Option(False, "--json", "-o", help="Output as JSON"),
):
    """Get realtime active users (last 30 minutes)."""
    import json
    from .client import analytics_run_realtime_report

    _setup_analytics_property()

    try:
        result = analytics_run_realtime_report(
            dimensions=["country"],
            metrics=["activeUsers"],
            limit=20,
        )

        if output_json:
            console.print(json.dumps(result["rows"], indent=2))
            return

        total_users = sum(
            int(float(row["metrics"].get("activeUsers", 0))) for row in result["rows"]
        )

        table = Table(title=f"Realtime Users: {total_users} active now")
        table.add_column("Country", style="cyan")
        table.add_column("Active Users", style="green", justify="right")

        for row in result["rows"]:
            dims = row["dimensions"]
            mets = row["metrics"]
            table.add_row(
                dims.get("country", ""),
                f"{int(float(mets.get('activeUsers', 0))):,}",
            )

        console.print(table)
    except Exception as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)


@analytics_app.command("query")
def analytics_query(
    dimensions: str = typer.Argument(..., help="Comma-separated dimensions"),
    metrics: str = typer.Argument(..., help="Comma-separated metrics"),
    start: str = typer.Option("30daysAgo", "--start", help="Start date"),
    end: str = typer.Option("today", "--end", "-e", help="End date"),
    limit: int = typer.Option(100, "--limit", "-n", help="Max results"),
    output_json: bool = typer.Option(False, "--json", "-o", help="Output as JSON"),
):
    """Run a custom query with any dimensions and metrics.

    Examples:
        gsuite analytics -s paradigm.xyz query "country,deviceCategory" "sessions,activeUsers"
        gsuite analytics -s paradigm.xyz query "pagePath" "screenPageViews" -n 50

    Common dimensions: country, city, deviceCategory, browser, operatingSystem,
        pagePath, pageTitle, sessionSourceMedium, sessionDefaultChannelGroup

    Common metrics: activeUsers, newUsers, sessions, screenPageViews, bounceRate,
        averageSessionDuration, engagementRate, engagedSessions
    """
    import json
    from .client import analytics_run_report

    _setup_analytics_property()

    try:
        dim_list = [d.strip() for d in dimensions.split(",")]
        met_list = [m.strip() for m in metrics.split(",")]

        result = analytics_run_report(
            dimensions=dim_list,
            metrics=met_list,
            start_date=start,
            end_date=end,
            limit=limit,
        )

        if output_json:
            console.print(json.dumps(result["rows"], indent=2))
            return

        table = Table(title=f"Custom Query ({start} to {end})")
        for dim in dim_list:
            table.add_column(dim, style="cyan")
        for met in met_list:
            table.add_column(met, style="green", justify="right")

        for row in result["rows"]:
            values = []
            for dim in dim_list:
                values.append(row["dimensions"].get(dim, ""))
            for met in met_list:
                val = row["metrics"].get(met, "0")
                try:
                    if "." in val:
                        formatted = f"{float(val):.2f}"
                    else:
                        formatted = f"{int(float(val)):,}"
                except (ValueError, TypeError):
                    formatted = val
                values.append(formatted)
            table.add_row(*values)

        console.print(table)
        console.print(f"[dim]Total rows: {result['row_count']}[/]")
    except Exception as e:
        console.print(f"[red]Error: {e}[/]")
        raise typer.Exit(1)


if __name__ == "__main__":
    app()
