"""CLI for Attio CRM."""

import json
import sys

import typer
from dotenv import load_dotenv
from rich.console import Console
from centaur_sdk import Table

from .client import AttioClient

load_dotenv()

app = typer.Typer(name="attio", help="Attio CRM CLI for AI agents")
console = Console()


def _get_client() -> AttioClient:
    return AttioClient()


def _extract_value(val: dict | list | None) -> str:
    """Extract display value from Attio value object."""
    if val is None:
        return ""
    if isinstance(val, list):
        if not val:
            return ""
        val = val[0]
    if isinstance(val, dict):
        for key in ["value", "full_name", "name", "domain", "email_address", "phone_number"]:
            if key in val:
                return str(val[key])
        if "first_name" in val:
            return f"{val.get('first_name', '')} {val.get('last_name', '')}".strip()
    return str(val)


@app.command()
def whoami():
    """Show info about current API token."""
    client = _get_client()
    info = client.get_self()
    console.print(f"[bold]Workspace:[/] {info.get('workspace', {}).get('name', 'N/A')}")
    console.print(f"[bold]Workspace ID:[/] {info.get('workspace', {}).get('id', 'N/A')}")


@app.command()
def objects():
    """List all objects in the workspace."""
    client = _get_client()
    objs = client.list_objects()

    if not objs:
        console.print("[yellow]No objects found.[/]")
        raise typer.Exit()

    table = Table(title=f"Objects ({len(objs)})")
    table.add_column("Slug", style="cyan", max_width=25)
    table.add_column("Name", style="white", max_width=25)
    table.add_column("Type", style="green", max_width=15)

    for obj in objs:
        api_slug = obj.get("api_slug", "")
        singular = obj.get("singular_noun", "")
        obj_type = "standard" if obj.get("is_standard", False) else "custom"
        table.add_row(api_slug, singular, obj_type)

    console.print(table)


@app.command()
def attributes(
    object_slug: str = typer.Argument(..., help="Object slug (e.g., 'people', 'companies')"),
):
    """List attributes for an object."""
    client = _get_client()
    attrs = client.list_attributes(object_slug)

    if not attrs:
        console.print("[yellow]No attributes found.[/]")
        raise typer.Exit()

    table = Table(title=f"Attributes for {object_slug} ({len(attrs)})")
    table.add_column("Slug", style="cyan", max_width=25)
    table.add_column("Title", style="white", max_width=25)
    table.add_column("Type", style="green", max_width=15)
    table.add_column("Required", style="yellow", max_width=8)

    for attr in attrs:
        api_slug = attr.get("api_slug", "")
        title = attr.get("title", "")
        attr_type = attr.get("type", "")
        required = "yes" if attr.get("is_required", False) else ""
        table.add_row(api_slug, title, attr_type, required)

    console.print(table)


@app.command()
def people(
    limit: int = typer.Option(25, "--limit", "-n", help="Max results"),
    filter_name: str = typer.Option(None, "--name", help="Filter by name"),
    filter_email: str = typer.Option(None, "--email", help="Filter by email"),
    json_output: bool = typer.Option(False, "--json", "-j", help="Output as JSON"),
):
    """List people records."""
    client = _get_client()

    filter_obj = None
    if filter_name:
        filter_obj = {"name": filter_name}
    elif filter_email:
        filter_obj = {"email_addresses": filter_email}

    records = client.query_records("people", filter_obj=filter_obj, limit=limit)

    if json_output:
        print(json.dumps(records, indent=2, ensure_ascii=False), file=sys.stdout)
        raise typer.Exit()

    if not records:
        console.print("[yellow]No people found.[/]")
        raise typer.Exit()

    table = Table(title=f"People ({len(records)})")
    table.add_column("ID", style="dim", max_width=36)
    table.add_column("Name", style="cyan", max_width=30)
    table.add_column("Email", style="white", max_width=35)

    for record in records:
        record_id = record.get("id", {}).get("record_id", "")
        values = record.get("values", {})
        name = _extract_value(values.get("name"))
        email = _extract_value(values.get("email_addresses"))
        table.add_row(record_id[:8] + "...", name, email)

    console.print(table)


@app.command()
def companies(
    limit: int = typer.Option(25, "--limit", "-n", help="Max results"),
    filter_name: str = typer.Option(None, "--name", help="Filter by name"),
    filter_domain: str = typer.Option(None, "--domain", help="Filter by domain"),
    json_output: bool = typer.Option(False, "--json", "-j", help="Output as JSON"),
):
    """List company records."""
    client = _get_client()

    filter_obj = None
    if filter_name:
        filter_obj = {"name": filter_name}
    elif filter_domain:
        filter_obj = {"domains": filter_domain}

    records = client.query_records("companies", filter_obj=filter_obj, limit=limit)

    if json_output:
        print(json.dumps(records, indent=2, ensure_ascii=False), file=sys.stdout)
        raise typer.Exit()

    if not records:
        console.print("[yellow]No companies found.[/]")
        raise typer.Exit()

    table = Table(title=f"Companies ({len(records)})")
    table.add_column("ID", style="dim", max_width=36)
    table.add_column("Name", style="cyan", max_width=30)
    table.add_column("Domain", style="white", max_width=30)

    for record in records:
        record_id = record.get("id", {}).get("record_id", "")
        values = record.get("values", {})
        name = _extract_value(values.get("name"))
        domain = _extract_value(values.get("domains"))
        table.add_row(record_id[:8] + "...", name, domain)

    console.print(table)


@app.command()
def records(
    object_slug: str = typer.Argument(..., help="Object slug (e.g., 'people', 'companies')"),
    limit: int = typer.Option(25, "--limit", "-n", help="Max results"),
    filter_json: str = typer.Option(None, "--filter", "-f", help="Filter as JSON"),
    json_output: bool = typer.Option(False, "--json", "-j", help="Output as JSON"),
):
    """Query records for any object."""
    client = _get_client()

    filter_obj = json.loads(filter_json) if filter_json else None
    records_list = client.query_records(object_slug, filter_obj=filter_obj, limit=limit)

    if json_output:
        print(json.dumps(records_list, indent=2, ensure_ascii=False), file=sys.stdout)
        raise typer.Exit()

    if not records_list:
        console.print("[yellow]No records found.[/]")
        raise typer.Exit()

    console.print(f"[bold]{object_slug}[/] ({len(records_list)} records)\n")
    for record in records_list:
        record_id = record.get("id", {}).get("record_id", "")
        console.print(f"[cyan]{record_id}[/]")
        values = record.get("values", {})
        for key, val in list(values.items())[:5]:
            console.print(f"  {key}: {_extract_value(val)}")
        console.print()


@app.command()
def get(
    object_slug: str = typer.Argument(..., help="Object slug"),
    record_id: str = typer.Argument(..., help="Record ID"),
    json_output: bool = typer.Option(False, "--json", "-j", help="Output as JSON"),
):
    """Get a single record by ID."""
    client = _get_client()
    record = client.get_record(object_slug, record_id)

    if json_output:
        print(json.dumps(record, indent=2, ensure_ascii=False), file=sys.stdout)
        raise typer.Exit()

    console.print(f"[bold]{object_slug}[/] Record")
    console.print(f"[cyan]ID:[/] {record.get('id', {}).get('record_id', '')}\n")

    values = record.get("values", {})
    for key, val in values.items():
        console.print(f"[bold]{key}:[/] {_extract_value(val)}")


@app.command()
def create(
    object_slug: str = typer.Argument(..., help="Object slug (e.g., 'people', 'companies')"),
    values_json: str = typer.Argument(..., help="Record values as JSON"),
):
    """Create a new record.

    Examples:
        attio create people '{"name": [{"first_name": "John", "last_name": "Doe"}]}'
        attio create companies '{"name": [{"value": "Acme Inc"}], "domains": [{"domain": "acme.com"}]}'
    """
    client = _get_client()
    values = json.loads(values_json)
    record = client.create_record(object_slug, values)

    record_id = record.get("id", {}).get("record_id", "")
    console.print(f"[green]✓ Created {object_slug} record[/]")
    console.print(f"[cyan]ID:[/] {record_id}")


@app.command()
def update(
    object_slug: str = typer.Argument(..., help="Object slug"),
    record_id: str = typer.Argument(..., help="Record ID"),
    values_json: str = typer.Argument(..., help="Values to update as JSON"),
):
    """Update an existing record.

    Examples:
        attio update people abc123 '{"email_addresses": [{"email_address": "new@email.com"}]}'
    """
    client = _get_client()
    values = json.loads(values_json)
    client.update_record(object_slug, record_id, values)

    console.print(f"[green]✓ Updated {object_slug} record {record_id[:8]}...[/]")


@app.command()
def delete(
    object_slug: str = typer.Argument(..., help="Object slug"),
    record_id: str = typer.Argument(..., help="Record ID"),
    confirm: bool = typer.Option(False, "--yes", "-y", help="Skip confirmation"),
):
    """Delete a record."""
    if not confirm:
        typer.confirm(f"Delete {object_slug} record {record_id}?", abort=True)

    client = _get_client()
    client.delete_record(object_slug, record_id)
    console.print(f"[green]✓ Deleted {object_slug} record {record_id[:8]}...[/]")


@app.command()
def upsert(
    object_slug: str = typer.Argument(..., help="Object slug"),
    matching_attr: str = typer.Argument(
        ..., help="Attribute to match on (e.g., 'email_addresses')"
    ),
    values_json: str = typer.Argument(..., help="Record values as JSON"),
):
    """Create or update a record based on matching attribute.

    Examples:
        attio upsert people email_addresses '{"email_addresses": [{"email_address": "john@example.com"}], "name": [{"first_name": "John"}]}'
    """
    client = _get_client()
    values = json.loads(values_json)
    record = client.assert_record(object_slug, matching_attr, values)

    record_id = record.get("id", {}).get("record_id", "")
    console.print(f"[green]✓ Upserted {object_slug} record[/]")
    console.print(f"[cyan]ID:[/] {record_id}")


@app.command()
def lists():
    """List all lists in the workspace."""
    client = _get_client()
    lists_data = client.list_lists()

    if not lists_data:
        console.print("[yellow]No lists found.[/]")
        raise typer.Exit()

    table = Table(title=f"Lists ({len(lists_data)})")
    table.add_column("ID", style="dim", max_width=36)
    table.add_column("Name", style="cyan", max_width=25)
    table.add_column("Object", style="white", max_width=20)

    for lst in lists_data:
        list_id = lst.get("id", {}).get("list_id", "")
        name = lst.get("name", "")
        parent_object = lst.get("parent_object", [])
        if isinstance(parent_object, list):
            parent_object = ", ".join(parent_object)
        table.add_row(list_id[:8] + "...", name, parent_object)

    console.print(table)


@app.command()
def entries(
    list_id: str = typer.Argument(..., help="List ID or slug"),
    limit: int = typer.Option(25, "--limit", "-n", help="Max results"),
    json_output: bool = typer.Option(False, "--json", "-j", help="Output as JSON"),
):
    """Query entries in a list."""
    client = _get_client()
    entries_list = client.query_entries(list_id, limit=limit)

    if json_output:
        print(json.dumps(entries_list, indent=2, ensure_ascii=False), file=sys.stdout)
        raise typer.Exit()

    if not entries_list:
        console.print("[yellow]No entries found.[/]")
        raise typer.Exit()

    console.print(f"[bold]List Entries[/] ({len(entries_list)})\n")
    for entry in entries_list:
        entry_id = entry.get("id", {}).get("entry_id", "")
        record_id = entry.get("id", {}).get("record_id", "")
        console.print(f"[cyan]Entry:[/] {entry_id[:8]}... [dim](record: {record_id[:8]}...)[/]")
        values = entry.get("entry_values", {})
        for key, val in list(values.items())[:3]:
            console.print(f"  {key}: {_extract_value(val)}")
        console.print()


@app.command()
def notes(
    object_slug: str = typer.Argument(..., help="Parent object slug"),
    record_id: str = typer.Argument(..., help="Parent record ID"),
    json_output: bool = typer.Option(False, "--json", "-j", help="Output as JSON"),
):
    """List notes for a record."""
    client = _get_client()
    notes_list = client.list_notes(object_slug, record_id)

    if json_output:
        print(json.dumps(notes_list, indent=2, ensure_ascii=False), file=sys.stdout)
        raise typer.Exit()

    if not notes_list:
        console.print("[yellow]No notes found.[/]")
        raise typer.Exit()

    console.print(f"[bold]Notes[/] ({len(notes_list)})\n")
    for note in notes_list:
        note_id = note.get("id", {}).get("note_id", "")
        title = note.get("title", "Untitled")
        console.print(f"[cyan]{title}[/] [dim]({note_id[:8]}...)[/]")


@app.command("list-threads")
def list_threads(
    object_slug: str = typer.Option(None, "--object", "-o", help="Filter by linked object"),
    record_id: str = typer.Option(None, "--record", "-r", help="Filter by linked record ID"),
    list_id: str = typer.Option(None, "--list", "-l", help="Filter by linked list"),
    entry_id: str = typer.Option(None, "--entry", "-e", help="Filter by linked entry"),
    limit: int = typer.Option(25, "--limit", "-n", help="Max results"),
    offset: int = typer.Option(0, "--offset", help="Result offset"),
    json_output: bool = typer.Option(False, "--json", "-j", help="Output as JSON"),
):
    """List comment threads for a record or list entry."""
    client = _get_client()
    threads = client.list_threads(
        object_slug=object_slug,
        record_id=record_id,
        list_id=list_id,
        entry_id=entry_id,
        limit=limit,
        offset=offset,
    )

    if json_output:
        print(json.dumps(threads, indent=2, ensure_ascii=False), file=sys.stdout)
        raise typer.Exit()

    if not threads:
        console.print("[yellow]No threads found.[/]")
        raise typer.Exit()

    table = Table(title=f"Threads ({len(threads)})")
    table.add_column("ID", style="dim", max_width=36)
    table.add_column("Title", style="cyan", max_width=50)
    table.add_column("Created", style="white", max_width=20)

    for thread in threads:
        thread_id = thread.get("id", {}).get("thread_id", "") or thread.get("id", "")
        title = thread.get("title") or thread.get("subject") or ""
        created = thread.get("created_at", "")[:19]
        table.add_row(thread_id[:8] + "...", title, created)

    console.print(table)


@app.command("get-thread")
def get_thread(
    thread_id: str = typer.Argument(..., help="Thread ID"),
    json_output: bool = typer.Option(False, "--json", "-j", help="Output as JSON"),
):
    """Get a thread and its comments."""
    client = _get_client()
    thread = client.get_thread(thread_id)

    if json_output:
        print(json.dumps(thread, indent=2, ensure_ascii=False), file=sys.stdout)
        raise typer.Exit()

    console.print(f"[bold]Thread[/] [cyan]{thread_id}[/]\n")
    console.print(json.dumps(thread, indent=2, ensure_ascii=False))


@app.command("list-meetings")
def list_meetings(
    limit: int = typer.Option(50, "--limit", "-n", help="Max results"),
    cursor: str = typer.Option(None, "--cursor", help="Pagination cursor"),
    linked_object: str = typer.Option(None, "--linked-object", help="Linked object slug"),
    linked_record_id: str = typer.Option(None, "--linked-record-id", help="Linked record ID"),
    participants: str = typer.Option(
        None, "--participants", help="Comma-separated participant emails or record IDs"
    ),
    sort: str = typer.Option(None, "--sort", help="Sort order supported by Attio"),
    ends_from: str = typer.Option(None, "--ends-from", help="Filter by end time lower bound"),
    starts_before: str = typer.Option(
        None, "--starts-before", help="Filter by start time upper bound"
    ),
    timezone: str = typer.Option(None, "--timezone", help="Timezone for date filters"),
    json_output: bool = typer.Option(False, "--json", "-j", help="Output as JSON"),
):
    """List meetings, optionally filtered to a linked Attio record."""
    client = _get_client()
    participant_values = (
        [part.strip() for part in participants.split(",") if part.strip()]
        if participants
        else None
    )
    result = client.list_meetings(
        limit=limit,
        cursor=cursor,
        linked_object=linked_object,
        linked_record_id=linked_record_id,
        participants=participant_values,
        sort=sort,
        ends_from=ends_from,
        starts_before=starts_before,
        timezone=timezone,
    )

    if json_output:
        print(json.dumps(result, indent=2, ensure_ascii=False), file=sys.stdout)
        raise typer.Exit()

    meetings = result.get("data", [])
    if not meetings:
        console.print("[yellow]No meetings found.[/]")
        raise typer.Exit()

    table = Table(title=f"Meetings ({len(meetings)})")
    table.add_column("ID", style="dim", max_width=36)
    table.add_column("Title", style="cyan", max_width=50)
    table.add_column("Start", style="white", max_width=20)
    table.add_column("End", style="white", max_width=20)

    for meeting in meetings:
        meeting_id = meeting.get("id", {}).get("meeting_id", "") or meeting.get("id", "")
        title = meeting.get("title") or meeting.get("subject") or meeting.get("name") or ""
        start = meeting.get("start_at") or meeting.get("starts_at") or meeting.get("start_time") or ""
        end = meeting.get("end_at") or meeting.get("ends_at") or meeting.get("end_time") or ""
        table.add_row(meeting_id[:8] + "...", title, str(start)[:19], str(end)[:19])

    console.print(table)


@app.command("get-meeting")
def get_meeting(
    meeting_id: str = typer.Argument(..., help="Meeting ID"),
    json_output: bool = typer.Option(False, "--json", "-j", help="Output as JSON"),
):
    """Get a single meeting by ID."""
    client = _get_client()
    meeting = client.get_meeting(meeting_id)

    if json_output:
        print(json.dumps(meeting, indent=2, ensure_ascii=False), file=sys.stdout)
        raise typer.Exit()

    console.print(f"[bold]Meeting[/] [cyan]{meeting_id}[/]\n")
    console.print(json.dumps(meeting, indent=2, ensure_ascii=False))


@app.command("list-call-recordings")
def list_call_recordings(
    meeting_id: str = typer.Argument(..., help="Meeting ID"),
    limit: int = typer.Option(50, "--limit", "-n", help="Max results"),
    cursor: str = typer.Option(None, "--cursor", help="Pagination cursor"),
    json_output: bool = typer.Option(False, "--json", "-j", help="Output as JSON"),
):
    """List call recordings for a meeting."""
    client = _get_client()
    result = client.list_call_recordings(meeting_id, limit=limit, cursor=cursor)

    if json_output:
        print(json.dumps(result, indent=2, ensure_ascii=False), file=sys.stdout)
        raise typer.Exit()

    recordings = result.get("data", [])
    if not recordings:
        console.print("[yellow]No call recordings found.[/]")
        raise typer.Exit()

    table = Table(title=f"Call Recordings ({len(recordings)})")
    table.add_column("ID", style="dim", max_width=36)
    table.add_column("Title", style="cyan", max_width=50)
    table.add_column("Created", style="white", max_width=20)

    for recording in recordings:
        recording_id = (
            recording.get("id", {}).get("call_recording_id", "") or recording.get("id", "")
        )
        title = recording.get("title") or recording.get("name") or ""
        created = recording.get("created_at", "")[:19]
        table.add_row(recording_id[:8] + "...", title, created)

    console.print(table)


@app.command("get-call-transcript")
def get_call_transcript(
    meeting_id: str = typer.Argument(..., help="Meeting ID"),
    call_recording_id: str = typer.Argument(..., help="Call recording ID"),
    cursor: str = typer.Option(None, "--cursor", help="Pagination cursor"),
    json_output: bool = typer.Option(False, "--json", "-j", help="Output as JSON"),
):
    """Get a call recording transcript."""
    client = _get_client()
    transcript = client.get_call_transcript(meeting_id, call_recording_id, cursor=cursor)

    if json_output:
        print(json.dumps(transcript, indent=2, ensure_ascii=False), file=sys.stdout)
        raise typer.Exit()

    console.print(json.dumps(transcript, indent=2, ensure_ascii=False))


@app.command("add-note")
def add_note(
    object_slug: str = typer.Argument(..., help="Parent object slug"),
    record_id: str = typer.Argument(..., help="Parent record ID"),
    title: str = typer.Argument(..., help="Note title"),
    content: str = typer.Argument(..., help="Note content"),
):
    """Add a note to a record."""
    client = _get_client()
    note = client.create_note(object_slug, record_id, title, content)
    note_id = note.get("id", {}).get("note_id", "")
    console.print("[green]✓ Created note[/]")
    console.print(f"[cyan]ID:[/] {note_id}")


@app.command()
def tasks(
    object_slug: str = typer.Option(None, "--object", "-o", help="Filter by linked object"),
    record_id: str = typer.Option(None, "--record", "-r", help="Filter by linked record ID"),
    completed: bool = typer.Option(None, "--completed", "-c", help="Filter by completion"),
    limit: int = typer.Option(25, "--limit", "-n", help="Max results"),
    json_output: bool = typer.Option(False, "--json", "-j", help="Output as JSON"),
):
    """List tasks."""
    client = _get_client()
    tasks_list = client.list_tasks(
        linked_object=object_slug,
        linked_record_id=record_id,
        is_completed=completed,
        limit=limit,
    )

    if json_output:
        print(json.dumps(tasks_list, indent=2, ensure_ascii=False), file=sys.stdout)
        raise typer.Exit()

    if not tasks_list:
        console.print("[yellow]No tasks found.[/]")
        raise typer.Exit()

    table = Table(title=f"Tasks ({len(tasks_list)})")
    table.add_column("ID", style="dim", max_width=10)
    table.add_column("Content", style="white", max_width=50)
    table.add_column("Status", style="green", max_width=10)
    table.add_column("Deadline", style="yellow", max_width=12)

    for task in tasks_list:
        task_id = task.get("id", {}).get("task_id", "")
        content = task.get("content_plaintext", "")[:50]
        is_completed = "✓" if task.get("is_completed") else ""
        deadline = task.get("deadline_at", "")[:10] if task.get("deadline_at") else ""
        table.add_row(task_id[:8] + "..", content, is_completed, deadline)

    console.print(table)


@app.command("add-task")
def add_task(
    content: str = typer.Argument(..., help="Task content"),
    deadline: str = typer.Option(None, "--deadline", "-d", help="Deadline (ISO format)"),
    assignee: str = typer.Option(None, "--assignee", "-a", help="Workspace member ID"),
):
    """Create a new task."""
    client = _get_client()
    assignees = [assignee] if assignee else None
    task = client.create_task(content, deadline=deadline, assignees=assignees)

    task_id = task.get("id", {}).get("task_id", "")
    console.print("[green]✓ Created task[/]")
    console.print(f"[cyan]ID:[/] {task_id}")


@app.command()
def members():
    """List workspace members."""
    client = _get_client()
    members_list = client.list_workspace_members()

    if not members_list:
        console.print("[yellow]No members found.[/]")
        raise typer.Exit()

    table = Table(title=f"Workspace Members ({len(members_list)})")
    table.add_column("ID", style="dim", max_width=10)
    table.add_column("Name", style="cyan", max_width=25)
    table.add_column("Email", style="white", max_width=35)
    table.add_column("Role", style="green", max_width=15)

    for member in members_list:
        member_id = member.get("id", {}).get("workspace_member_id", "")
        name = f"{member.get('first_name', '')} {member.get('last_name', '')}".strip()
        email = member.get("email_address", "")
        role = member.get("access_level", "")
        table.add_row(member_id[:8] + "..", name, email, role)

    console.print(table)


if __name__ == "__main__":
    app()
