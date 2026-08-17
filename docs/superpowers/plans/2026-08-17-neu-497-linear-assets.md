# NEU-497 Linear evidence upload implementation plan

> **For agentic workers:** Parallel execution: use `ultrapowers:ultrapowers` (this plan carries ultraplan markers). The workflow tool is unavailable in this environment, so execute sequentially with `subagent-driven-development` and independent review. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a fail-closed Linear CLI upload operation for PNG screenshots and WebM recordings, with optional Markdown evidence comment, without leaking Linear credentials into presigned PUT requests.

**Architecture:** Keep GraphQL authentication in the existing Linear client, but introduce a separate bare HTTP uploader. Validate a file beneath `~/uploads`, request Linear's `fileUpload` target, validate the returned upload and asset URLs plus provider headers, PUT bytes with no Authorization injection, then optionally create one issue comment. Return a stable redacted result and preserve the asset URL if comment creation fails.

**Tech Stack:** Python 3.12, httpx, Typer, pytest, Linear GraphQL API.

**Acceptance:** suite - focused upload/CLI/client tests, the full Linear tool test suite, manifest assertions, system-prompt assertions, and independent security review are the acceptance evidence. A live smoke test is a separate manual gate.

## Global Constraints

- Build on upstream Centaur `36f2a71196ad9c1259d6ca6e221c3e5b9b402dc7`, the NEU-492 / PR #20 baseline.
- Accept only a non-symlink regular file beneath `~/uploads`; reject traversal, symlink ancestors, directories, devices, and FIFOs.
- Accept PNG up to 10 MiB with a valid PNG signature, or WebM up to 50 MiB with an EBML header and `DocType=webm`.
- Accept only exact HTTPS `uploads.linear.app` upload and asset hosts; no wildcard or redirect expansion.
- Never forward `Authorization`, `Proxy-Authorization`, `Cookie`, `Set-Cookie`, or `Host` from Linear's returned headers.
- A presigned PUT uses a separate client and carries no Linear Authorization header.
- Upload before commenting. If comment creation fails, report a partial failure with the stable asset URL and never retry the upload automatically.
- Never return the presigned URL, signed headers, tokens, or file bytes.
- Use failing tests before each behavior change.
- Do not run the live upload smoke until a user-authorized Linear credential is available through the normal secret path.

---

### Task 1: Validate evidence files and signed targets

**Type:** implementation
**Depends-on:** none
**Review:** adversarial

**Files:**
- Create: `tools/productivity/linear/uploads.py`
- Create: `tools/productivity/linear/test_uploads.py`

**Interfaces:**
- Produces: `validate_upload_file(path, uploads_root=None) -> UploadFile`, `validate_upload_target(upload_file) -> ValidatedTarget`, and typed upload/partial-failure errors.

- [ ] **Step 1: Write failing validation tests**

Cover valid PNG/WebM, outside-root traversal, final symlink and symlink ancestor, directory/non-regular file, empty file, size caps, extension/signature mismatch, fake WebM, non-HTTPS URLs, wrong hosts, URL credentials, redirects, and forbidden returned headers.

- [ ] **Step 2: Run RED**

Run: `python -m pytest tools/productivity/linear/test_uploads.py -q`

Expected: import failure because `uploads.py` does not exist.

- [ ] **Step 3: Implement minimal validators**

Resolve the default root from `Path.home() / "uploads"`, use `lstat`/resolved-parent checks, read bounded headers, parse EBML enough to locate the WebM DocType, and normalize only safe provider headers.

- [ ] **Step 4: Run GREEN**

Run the focused test and require all validation cases to pass.

### Task 2: Add GraphQL allocation and bare-byte upload

**Type:** implementation
**Depends-on:** 1
**Review:** adversarial

**Files:**
- Modify: `tools/productivity/linear/graphql.py`
- Modify: `tools/productivity/linear/client.py`
- Modify: `tools/productivity/linear/test_client.py`
- Modify: `tools/productivity/linear/test_uploads.py`

**Interfaces:**
- Consumes: `fileUpload(filename, contentType, size)` response.
- Produces: `LinearClient.upload_evidence(issue_id, path, comment=None) -> dict` and a bare `put_upload(target, file)` helper.

- [ ] **Step 1: Write failing protocol tests**

Assert the exact mutation variables, PUT method/body/Content-Type/Cache-Control plus allowed provider headers, absence of Authorization on PUT, upload-before-comment ordering, no comment after upload failure, and partial failure after comment failure.

- [ ] **Step 2: Run RED**

Expected: missing mutation and upload interfaces.

- [ ] **Step 3: Implement allocation, target validation, PUT, and optional comment**

Use the authenticated GraphQL transport only for allocation and comment creation. Use an independently constructed bare `httpx.Client(follow_redirects=False)` for the PUT. A success result is exactly:

```json
{"ok":true,"tool":"linear","issue_id":"NEU-497","asset_url":"https://uploads.linear.app/...","filename":"evidence.png","mime_type":"image/png","size_bytes":123,"comment_id":"..."}
```

Return `comment_id: null` when no comment was requested. On comment failure return structured `ok:false`, `stage:"comment"`, and the same safe asset metadata.

- [ ] **Step 4: Run focused GREEN tests**

Run client and upload tests and inspect captured requests for credential leakage.

### Task 3: Expose the upload CLI and tighten the secret contract

**Type:** implementation
**Depends-on:** 1, 2
**Review:** adversarial

**Files:**
- Modify: `tools/productivity/linear/cli.py`
- Modify: `tools/productivity/linear/test_cli.py`
- Modify: `tools/productivity/linear/pyproject.toml`
- Modify: `services/sandbox/SYSTEM_PROMPT.md`

**Interfaces:**
- Produces: `linear upload ISSUE_ID FILE [--comment TEXT] [--json]`.

- [ ] **Step 1: Write failing CLI tests**

Cover JSON success, human success, validation failure, upload failure, partial comment failure, exit codes, stable field names, and absence of signed URLs/headers/token/bytes in stdout and stderr.

- [ ] **Step 2: Run RED**

Expected: Typer reports that the upload command does not exist.

- [ ] **Step 3: Implement the command and manifest boundary**

Add the command using the client interface. Change the secret definition from unconditional injection to replacement of an explicit `Authorization` placeholder on `api.linear.app` and `uploads.linear.app`, so the bare PUT cannot acquire the API key.

- [ ] **Step 4: Correct the sandbox prompt**

Replace the stale claim that Linear has no upload surface with the exact evidence-upload capability and restrictions. Do not imply support for arbitrary attachments or file types.

- [ ] **Step 5: Run focused GREEN tests and static secret assertions**

Assert the manifest uses the supported replace-only header contract and no uploader call constructs an Authorization placeholder.

### Task 4: Verify upstream implementation and isolate the live gate

**Type:** gate
**Depends-on:** 1, 2, 3
**Review:** adversarial

**Files:**
- Test: `tools/productivity/linear/test_uploads.py`
- Test: `tools/productivity/linear/test_client.py`
- Test: `tools/productivity/linear/test_cli.py`
- Test: `tools/productivity/linear/test_readonly.py`

**Interfaces:**
- Consumes: Tasks 1-3.
- Produces: an upstream commit, security review, and a separate live-smoke instruction.

- [ ] **Step 1: Run the complete Linear tool suite**

Run: `python -m pytest tools/productivity/linear -q`

- [ ] **Step 2: Run static disclosure checks**

Search results and error formatting for upload URL, signed headers, Authorization, cookies, and raw bytes. Verify exact host checks and `follow_redirects=False`.

- [ ] **Step 3: Request independent read-only security review**

The reviewer must trace the authenticated GraphQL request and bare PUT separately, attack file confinement/URL/header validation, and verify partial-failure semantics.

- [ ] **Step 4: Record the live smoke as human input**

Once the normal Centaur Linear credential is provisioned, upload one disposable PNG to NEU-497, add a disposable evidence comment, verify the asset resolves, and remove only the disposable comment if policy permits. Never paste the credential or signed URL into chat or a report.

