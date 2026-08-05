---
name: docsend
description: "Use the DocSend CLI to recover and download standalone DocSend documents or files from DocSend Spaces, including email or passcode gates, resumable verification, folder traversal, and returning files. Trigger whenever a user shares a DocSend URL or asks to access, inspect, summarize, download, or recover a DocSend document or Space."
---

# DocSend

Use the `docsend` CLI to download standalone documents and files from DocSend Spaces. Progress goes to stderr and every command prints JSON to stdout.

Before starting, check whether the thread already contains the requested file, an `attachment_ref`, or a usable upload. If it does, use that file instead of opening DocSend again.

## Choose The Command From The URL

- If the URL path contains `/view/s/`, it is a DocSend Space. Start with `docsend login`.
- For any other DocSend view URL, it is a standalone document. Start with `docsend download`.

Do not run `download` first for a Space. Do not run `login` for a standalone document.

## Standalone Documents

Use `download` for a regular DocSend URL:

```bash
docsend download '<docsend-url>' \
  --email '<email>' \
  --output '<output.pdf>'
```

Add `--passcode '<passcode>'` only when the user supplied the document passcode. Use `--session-timeout 1800` on the initial command when the verification handoff may need the maximum supported time.

On `status: ok`, use the file at the returned `output` path.

## Email Verification

When `download` or `login` returns `status: verification_link_required`:

1. Preserve `resume_session_id` and `expires_at` from the JSON response.
2. Obtain the newest matching DocSend verification URL using the Gmail workflow below or another authorized email tool. Extract it without opening it.
3. Resume the same session before it expires:

```bash
DOCSEND_VERIFICATION_LINK='<full-verification-url>' \
  docsend resume '<resume-session-id>' \
  --output '<output.pdf>'
```

Omit `--output` when resuming a Space. A successful Space resume returns `status: deal_room_ready` and its root inventory.

If the link is invalid, retry `resume` with the correct link and the same session ID. If the session expired, start one new `download` or `login` command. Never open a verification URL outside `docsend resume` and never start duplicate sessions while one is active.

### Find The Link In Gmail

When `gsuite` is available, search the Gmail account connected to the current Centaur user. Gmail operations use `userId=me`; never search another mailbox or ask for Gmail credentials.

1. Search for recent DocSend mail and inspect results newest first:

```bash
centaur-tools call gsuite gmail_search \
  '{"query":"newer_than:1d from:docsend.com","max_results":5}'
```

2. Read the newest candidate received after the current verification request:

```bash
centaur-tools call gsuite gmail_get '{"message_id":"<message-id>"}'
```

3. Extract the full HTTPS DocSend verification URL from the message body or HTML. Confirm the hostname is `docsend.com` or a subdomain. Do not click it, request it, preview it, or pass it to any browser or HTTP tool.
4. If no matching message has arrived, repeat the search up to three times at roughly 10-second intervals, always checking the newest results first. Do not submit the DocSend email gate again while polling.
5. Pass the extracted URL through `DOCSEND_VERIFICATION_LINK` to `docsend resume`, as shown above. Do not put verification URLs in command arguments or logs. The resume command is the only operation allowed to open it.

If Gmail is unavailable, unauthorized, or still has no message after the bounded polling, preserve the session details and use another authorized email tool or ask the user for the link. Do not start another Browserbase session while the existing one remains valid.

## Spaces

Start a `/view/s/` URL with `login`:

```bash
docsend login '<docsend-space-url>' --email '<email>'
```

After login or verification returns `deal_room_ready`, keep its `session_id` for every following command.

List the Space root:

```bash
docsend list '<session-id>'
```

Open a folder using its item ID:

```bash
docsend open-folder '<session-id>' --folder-id '<folder-item-id>'
```

Both commands return only one directory level. Read each folder's ID from the preceding JSON response and call `open-folder` again to traverse deeper.

Download the selected file using its returned item ID:

```bash
docsend fetch '<session-id>' \
  --item-id '<file-item-id>' \
  --output '<output-path>'
```

The downloaded filename may differ from the visible Space title. Fetch files sequentially, and do not pass folder or external URL item IDs to `fetch`.

Close the session when finished:

```bash
docsend close '<session-id>'
```

## Handling Results

- Parse stdout as JSON even when the command exits nonzero.
- For `email_required`, ask for an email unless the user already supplied one.
- For `passcode_required`, ask only for the document passcode.
- Preserve session fields on verification and failure responses.
- Report statuses such as `blocked`, `expired`, `item_not_found`, `not_downloadable`, and `download_unavailable` exactly. Do not bypass access or download controls.
- Do not expose email addresses, passcodes, verification URLs, API keys, or downloaded file data in the final response.
