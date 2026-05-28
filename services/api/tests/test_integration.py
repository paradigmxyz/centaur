"""Integration tests for the Centaur API.

Covers:
- Message buffer endpoints (POST/GET /agent/messages)
- Attachment endpoints (GET /agent/attachments, download)
- Harness protocol helpers (_flushed_to_messages, messages_to_content_blocks, build_user_input)
- Harness command builder (build_harness_cmd)
- Session context builder (_build_session_context)
- Status endpoint (GET /agent/status)

DB-backed tests require the ephemeral Postgres from conftest.py.
Pure-function tests run without any infrastructure.
"""

from __future__ import annotations

import json

import pytest


async def _seed_assignment(db_pool, thread_key: str, generation: int = 1) -> None:
    await db_pool.execute(
        "INSERT INTO agent_runtime_assignments ("
        "thread_key, assignment_generation, runtime_id, harness, engine, "
        "persona_id, prompt_ref, effective_agents_md_sha256, state"
        ") VALUES ($1, $2, $3, 'amp', 'amp', NULL, 'harness:amp', 'sha', 'active')",
        thread_key,
        generation,
        f"rt-{thread_key}-{generation}",
    )


# ── Test 1: Message buffer endpoints ─────────────────────────────────────────


class TestPostMessages:
    """Tests for POST /agent/messages endpoint."""

    @pytest.mark.asyncio
    async def test_post_single_message(self, client, db_pool, api_key):
        """POST /agent/messages with a single message."""
        thread_key = "test:msg-1"
        await _seed_assignment(db_pool, thread_key)
        resp = await client.post(
            "/agent/messages",
            headers={"Authorization": f"Bearer {api_key}"},
            json={
                "thread_key": thread_key,
                "assignment_generation": 1,
                "role": "user",
                "parts": [{"type": "text", "text": "hello"}],
            },
        )
        assert resp.status_code == 200
        data = resp.json()
        assert data["ok"] is True
        assert data["inserted"] == 1

    @pytest.mark.asyncio
    async def test_post_batch_messages(self, client, db_pool, api_key):
        """POST /agent/messages with multiple messages in batch."""
        thread_key = "test:msg-batch-1"
        await _seed_assignment(db_pool, thread_key)
        messages = [
            {
                "role": "user",
                "parts": [{"type": "text", "text": f"batch msg {i}"}],
                "user_id": "U456",
                "message_id": f"msg-batch-{i}",
            }
            for i in range(3)
        ]
        resp = await client.post(
            "/agent/messages",
            headers={"Authorization": f"Bearer {api_key}"},
            json={
                "thread_key": thread_key,
                "assignment_generation": 1,
                "messages": messages,
            },
        )
        assert resp.status_code == 200
        data = resp.json()
        assert data["ok"] is True
        assert data["inserted"] == 3

    @pytest.mark.asyncio
    async def test_get_messages_empty(self, client, api_key):
        """GET for nonexistent thread returns empty list."""
        resp = await client.get(
            "/agent/messages",
            params={"thread_key": "test:empty"},
            headers={"Authorization": f"Bearer {api_key}"},
        )
        assert resp.status_code == 200
        data = resp.json()
        assert data["messages"] == []
        assert data["has_more"] is False

    @pytest.mark.asyncio
    async def test_get_messages_roundtrip(self, client, db_pool, api_key):
        """POST then GET returns the same message."""
        import json

        thread = "test:roundtrip-1"
        await _seed_assignment(db_pool, thread)
        await client.post(
            "/agent/messages",
            headers={"Authorization": f"Bearer {api_key}"},
            json={
                "thread_key": thread,
                "assignment_generation": 1,
                "message_id": "msg-roundtrip",
                "role": "user",
                "parts": [{"type": "text", "text": "roundtrip check"}],
                "user_id": "U789",
            },
        )
        resp = await client.get(
            "/agent/messages",
            params={"thread_key": thread},
            headers={"Authorization": f"Bearer {api_key}"},
        )
        assert resp.status_code == 200
        msgs = resp.json()["messages"]
        assert len(msgs) == 1
        assert msgs[0]["role"] == "user"
        # parts may come back as a JSON string or parsed list depending on codec
        parts = msgs[0]["parts"]
        if isinstance(parts, str):
            parts = json.loads(parts)
        assert parts == [{"type": "text", "text": "roundtrip check"}]
        assert msgs[0]["user_id"] == "U789"

    @pytest.mark.asyncio
    async def test_message_dedup(self, client, db_pool, api_key):
        """Same slack_ts = same message ID = no duplicate insert."""
        thread_key = "test:dedup-1"
        await _seed_assignment(db_pool, thread_key)
        for _ in range(2):
            await client.post(
                "/agent/messages",
                headers={"Authorization": f"Bearer {api_key}"},
                json={
                    "thread_key": thread_key,
                    "assignment_generation": 1,
                    "message_id": "msg-dedup",
                    "parts": [{"type": "text", "text": "hello"}],
                    "metadata": {"slack_ts": "1234567890.123456"},
                },
            )
        resp = await client.get(
            "/agent/messages",
            params={"thread_key": thread_key},
            headers={"Authorization": f"Bearer {api_key}"},
        )
        assert len(resp.json()["messages"]) == 1

    @pytest.mark.asyncio
    async def test_missing_thread_key(self, client, api_key):
        """POST without thread_key returns 422."""
        resp = await client.post(
            "/agent/messages",
            headers={"Authorization": f"Bearer {api_key}"},
            json={
                "parts": [{"type": "text", "text": "no thread key"}],
            },
        )
        assert resp.status_code == 422

    @pytest.mark.asyncio
    async def test_loopback_bypasses_auth(self, client, db_pool):
        """ASGI transport (localhost) bypasses auth — verifies loopback trust."""
        thread_key = "test:loopback-auth"
        await _seed_assignment(db_pool, thread_key)
        resp = await client.post(
            "/agent/messages",
            json={
                "thread_key": thread_key,
                "assignment_generation": 1,
                "parts": [{"type": "text", "text": "no key"}],
            },
        )
        assert resp.status_code == 200

    @pytest.mark.asyncio
    async def test_get_messages_pagination(self, client, db_pool, api_key):
        """GET with limit returns paginated results."""
        thread = "test:pagination-1"
        await _seed_assignment(db_pool, thread)
        for i in range(5):
            await client.post(
                "/agent/messages",
                headers={"Authorization": f"Bearer {api_key}"},
                json={
                    "thread_key": thread,
                    "assignment_generation": 1,
                    "message_id": f"msg-page-{i}",
                    "parts": [{"type": "text", "text": f"page msg {i}"}],
                    "metadata": {"slack_ts": f"1000000000.{i:06d}"},
                },
            )
        resp = await client.get(
            "/agent/messages",
            params={"thread_key": thread, "limit": "2"},
            headers={"Authorization": f"Bearer {api_key}"},
        )
        assert resp.status_code == 200
        data = resp.json()
        assert len(data["messages"]) == 2
        assert data["has_more"] is True


# ── Test 2: Attachment endpoints ─────────────────────────────────────────────


class TestAttachments:
    """Tests for GET /agent/attachments and download."""

    @pytest.mark.asyncio
    async def test_list_attachments_empty(self, client, api_key):
        """Listing attachments for unknown thread returns empty list."""
        resp = await client.get(
            "/agent/attachments",
            params={"thread_key": "test:no-att"},
            headers={"Authorization": f"Bearer {api_key}"},
        )
        assert resp.status_code == 200
        assert resp.json() == []

    @pytest.mark.asyncio
    async def test_download_attachment_not_found(self, client, api_key):
        """Downloading a nonexistent attachment returns 404."""
        resp = await client.get(
            "/agent/attachments/nonexistent/download",
            headers={"Authorization": f"Bearer {api_key}"},
        )
        assert resp.status_code == 404


# ── Test 3: Harness protocol integration ─────────────────────────────────────


class TestFlushedToMessages:
    """Tests for _flushed_to_messages helper."""

    def test_basic_conversion(self):
        """DB rows with list parts convert correctly."""
        from api.agent import _flushed_to_messages

        rows = [
            {"role": "user", "parts": [{"type": "text", "text": "hi"}], "user_id": "U123"},
        ]
        msgs = _flushed_to_messages(rows)
        assert len(msgs) == 1
        assert msgs[0]["role"] == "user"
        assert msgs[0]["parts"] == [{"type": "text", "text": "hi"}]
        assert msgs[0]["user_id"] == "U123"

    def test_string_parts_parsed(self):
        """Parts stored as JSON string are parsed."""
        from api.agent import _flushed_to_messages

        rows = [
            {"role": "user", "parts": '[{"type": "text", "text": "world"}]', "user_id": None},
        ]
        msgs = _flushed_to_messages(rows)
        assert msgs[0]["parts"] == [{"type": "text", "text": "world"}]
        assert "user_id" not in msgs[0]

    def test_mixed_rows(self):
        """Rows with and without user_id, list and string parts."""
        from api.agent import _flushed_to_messages

        rows = [
            {"role": "user", "parts": [{"type": "text", "text": "hi"}], "user_id": "U123"},
            {"role": "user", "parts": '[{"type": "text", "text": "world"}]', "user_id": None},
        ]
        msgs = _flushed_to_messages(rows)
        assert len(msgs) == 2
        assert msgs[0]["user_id"] == "U123"
        assert msgs[1]["parts"] == [{"type": "text", "text": "world"}]


class TestMessagesToContentBlocksWithAttachmentRef:
    """Integration-level tests for messages_to_content_blocks."""

    def test_attachment_ref_download_instruction(self):
        """DB message with attachment_ref → download instruction."""
        from api.sandbox.harness_protocol import messages_to_content_blocks

        msgs = [{
            "role": "user",
            "parts": [
                {"type": "text", "text": "check this"},
                {
                    "type": "attachment_ref",
                    "id": "att-1",
                    "name": "doc.pdf",
                    "mime_type": "application/pdf",
                },
            ],
            "user_id": "U999",
        }]
        blocks = messages_to_content_blocks(msgs)
        assert len(blocks) == 2
        assert blocks[0]["text"] == "<@U999>: check this"
        assert "/agent/attachments/att-1/download" in blocks[1]["text"]
        assert "att-1" in blocks[1]["text"]


# ── Test 4: build_user_input ─────────────────────────────────────────────────


class TestBuildUserInput:
    """Tests for build_user_input format."""

    def test_build_user_input_format(self):
        from api.sandbox.harness_protocol import build_user_input

        result = build_user_input([{"type": "text", "text": "hello"}])
        assert result == {
            "type": "user",
            "message": {
                "role": "user",
                "content": [{"type": "text", "text": "hello"}],
            },
        }


# ── Test 5: build_harness_cmd ───────────────────────────────────────────────


class TestBuildHarnessCmd:
    """Tests for build_harness_cmd."""

    def test_codex(self):
        from api.sandbox.config import build_harness_cmd

        cmd = build_harness_cmd("codex")
        assert cmd == ["codex-app-wrapper"]

    def test_claude_code(self):
        from api.sandbox.config import build_harness_cmd

        cmd = build_harness_cmd("claude-code")
        assert cmd == ["claude-app-wrapper"]

    def test_claude_code_with_model(self):
        from api.sandbox.config import build_harness_cmd

        # Model selection is wired through the CLAUDE_MODEL env var injected
        # by the sandbox backend so the wrapper can apply it to --model.
        cmd = build_harness_cmd("claude-code", model="opus")
        assert cmd == ["claude-app-wrapper"]

    def test_unknown_engine(self):
        from api.sandbox.config import build_harness_cmd

        cmd = build_harness_cmd("pi-mono")
        assert cmd == ["sleep", "infinity"]

    def test_container_env_honors_amp_mode_override(
        self,
        monkeypatch: pytest.MonkeyPatch,
    ):
        from api.sandbox.config import container_env

        monkeypatch.delenv("AGENT_LOCAL_DEV", raising=False)
        monkeypatch.setenv("AGENT_API_URL", "http://api.internal:8000")
        monkeypatch.setenv("FIREWALL_HOST", "firewall.internal")
        monkeypatch.setenv("AMP_MODE", "smart")

        env = container_env("thread-key", "sandbox-id", "firewall.internal")

        assert "AMP_MODE=smart" in env

    def test_container_env_includes_claude_hardening(
        self,
        monkeypatch: pytest.MonkeyPatch,
    ):
        from api.sandbox.config import container_env

        monkeypatch.delenv("AGENT_LOCAL_DEV", raising=False)
        monkeypatch.setenv("AGENT_API_URL", "http://api.internal:8000")
        monkeypatch.setenv("FIREWALL_HOST", "firewall.internal")

        env = container_env("thread-key", "sandbox-id", "firewall.internal")

        assert "ANTHROPIC_API_KEY=ANTHROPIC_API_KEY" in env
        assert "CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY=1" in env
        assert "CLAUDE_CODE_DISABLE_OFFICIAL_MARKETPLACE_AUTOINSTALL=1" in env
        assert "DISABLE_ERROR_REPORTING=1" in env
        assert "DISABLE_FEEDBACK_COMMAND=1" in env
        assert "DISABLE_GROWTHBOOK=1" in env
        assert "DISABLE_UPDATES=1" in env
        assert "CLAUDE_CODE_PROXY_RESOLVES_HOSTS=1" in env
        assert "CLAUDE_CODE_CERT_STORE=bundled,system" in env
        assert "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1" not in env
        assert "CLAUDE_CODE_DISABLE_EXPERIMENTAL_BETAS=1" not in env
        assert "CLAUDE_CODE_ENABLE_OPUS_4_7_FAST_MODE=1" not in env


class TestResolveHarnessProfile:
    def test_persona_uses_declared_engine_unless_harness_overrides(self, monkeypatch):
        import sys
        from types import ModuleType, SimpleNamespace

        from api.agent import _resolve_harness_profile

        personas = {
            "invest": SimpleNamespace(
                name="invest",
                engine="codex",
                default_repo="centaur-overlay",
            )
        }
        app_module = ModuleType("api.app")
        app_module.get_tool_manager = lambda: SimpleNamespace(
            get_persona=lambda name: personas.get(name)
        )
        monkeypatch.setitem(sys.modules, "api.app", app_module)

        assert _resolve_harness_profile(None, persona="invest") == (
            "codex",
            "invest",
            "centaur-overlay",
        )
        assert _resolve_harness_profile("claude-code", persona="invest") == (
            "claude-code",
            "invest",
            "centaur-overlay",
        )

    def test_engine_resolution_precedence_matrix(self, monkeypatch):
        """Exhaustive precedence matrix for the engine-resolution model.

        Both `harness` and `engine` are functionally one concept (the
        LLM-wrapping CLI binary). The resolver collapses them with this
        precedence (highest wins):

            1. engine_override
            2. harness (when DIFFERENT from persona's declared engine)
            3. persona.engine
            4. deployment default (CENTAUR_DEFAULT_HARNESS, falling back to "codex")

        Regression-locked: the previous resolver hardcoded engine="codex"
        for harness=="amp" regardless of persona, silently dropping the
        invest persona's declared engine="amp" — that exact failure mode
        is asserted below.
        """
        import sys
        from types import ModuleType, SimpleNamespace

        from api.agent import _resolve_harness_profile as resolve

        personas = {
            "invest": SimpleNamespace(name="invest", engine="amp", default_repo=None),
            "eng": SimpleNamespace(name="eng", engine="codex", default_repo=None),
        }
        app_module = ModuleType("api.app")
        app_module.get_tool_manager = lambda: SimpleNamespace(
            get_persona=lambda name: personas.get(name)
        )
        monkeypatch.setitem(sys.modules, "api.app", app_module)

        # Rule 1: engine_override always wins.
        assert resolve("amp", persona="invest", engine_override="codex")[0] == "codex"
        assert resolve(None, persona="invest", engine_override="claude-code")[0] == "claude-code"
        assert resolve("codex", persona=None, engine_override="amp")[0] == "amp"

        # Rule 2: explicit harness arg DIFFERENT from persona's engine wins
        # (this is how --invest --claude-code routes invest to a non-amp engine).
        assert resolve("claude-code", persona="invest")[0] == "claude-code"
        assert resolve("amp", persona="eng")[0] == "amp"

        # Rule 3: harness arg matching persona.engine (or no harness arg) →
        # persona's declared engine. This is the regression path: the workflow
        # forwards `effective_harness = persona.engine`, which used to silently
        # become "codex" for any persona whose engine == "amp".
        assert resolve("amp", persona="invest")[0] == "amp"
        assert resolve(None, persona="invest")[0] == "amp"
        assert resolve("codex", persona="eng")[0] == "codex"
        assert resolve(None, persona="eng")[0] == "codex"

        # Rule 4: no override, no persona → harness if given, else system default.
        assert resolve("amp", persona=None)[0] == "amp"
        assert resolve("claude-code", persona=None)[0] == "claude-code"
        monkeypatch.delenv("CENTAUR_DEFAULT_HARNESS", raising=False)
        assert resolve(None, persona=None)[0] == "codex"
        monkeypatch.setenv("CENTAUR_DEFAULT_HARNESS", "claude")
        assert resolve(None, persona=None)[0] == "claude-code"

    def test_unknown_harness_or_persona_is_rejected(self, monkeypatch):
        import sys
        from types import ModuleType, SimpleNamespace

        from api.agent import _resolve_harness_profile

        app_module = ModuleType("api.app")
        app_module.get_tool_manager = lambda: SimpleNamespace(
            get_persona=lambda _name: None
        )
        monkeypatch.setitem(sys.modules, "api.app", app_module)

        with pytest.raises(ValueError, match="Unknown harness"):
            _resolve_harness_profile("invest")
        with pytest.raises(ValueError, match="Unknown persona"):
            _resolve_harness_profile("codex", persona="invest")


class TestSpawnAndMessageAlias:
    @pytest.mark.asyncio
    async def test_message_alias(self, client, db_pool, api_key):
        thread_key = "test:message-alias-1"
        await _seed_assignment(db_pool, thread_key)
        resp = await client.post(
            "/agent/message",
            headers={"Authorization": f"Bearer {api_key}"},
            json={
                "thread_key": thread_key,
                "assignment_generation": 1,
                "role": "user",
                "parts": [{"type": "text", "text": "hello alias"}],
            },
        )
        assert resp.status_code == 200
        assert resp.json()["ok"] is True


# ── Test 6: _build_session_context ───────────────────────────────────────────


class TestBuildSessionContext:
    """Tests for _build_session_context."""

    def test_slack_platform(self):
        from api.agent import _build_session_context

        ctx = _build_session_context("test:1", platform="slack", user_id="U123")
        assert "Slack Formatting Rules" in ctx
        assert "<@U123>" not in ctx
        assert "Do not @-mention or tag the requester" in ctx
        assert "test:1" in ctx

    def test_no_platform(self):
        from api.agent import _build_session_context

        ctx = _build_session_context("test:1")
        assert "Slack" not in ctx
        assert "test:1" in ctx

    def test_slack_no_user_id(self):
        from api.agent import _build_session_context

        ctx = _build_session_context("test:1", platform="slack")
        assert "Slack Formatting Rules" in ctx
        assert "their real Slack mention" not in ctx
        assert "Do not @-mention or tag the requester" in ctx

    def test_slack_bot_user_id_not_mentioned(self):
        from api.agent import _build_session_context

        ctx = _build_session_context("test:1", platform="slack", user_id="B123")
        assert "Slack Formatting Rules" in ctx
        assert "<@B123>" not in ctx
        assert "Do not @-mention or tag the requester" in ctx

    def test_contains_timestamp(self):
        from api.agent import _build_session_context

        ctx = _build_session_context("test:1")
        assert "Date/Time" in ctx

    def test_requester_identity_with_github_handle(self):
        from api.agent import _build_session_context

        ctx = _build_session_context(
            "test:1",
            platform="slack",
            user_id="U123",
            requester_identity={
                "slack_user_id": "U123",
                "slack_mention": "<@U123>",
                "github_handle": "@alice",
                "github_handle_source": 'Slack profile custom field "GitHub"',
                "github_handle_verified": True,
            },
        )

        assert "Requester Identity" in ctx
        assert "GitHub handle from Slack profile: @alice" in ctx
        assert "GitHub handle verified: yes" in ctx
        assert "GitHub PR Attribution" in ctx
        assert "Prompted by: @alice" in ctx
        assert "Assign the PR to the requester when possible: `alice`" in ctx
        assert "not a Slack response mention rule" in ctx

    def test_requester_identity_without_github_handle(self):
        from api.agent import _build_session_context

        ctx = _build_session_context(
            "test:1",
            platform="slack",
            user_id="U123",
            requester_identity={
                "slack_user_id": "U123",
                "slack_mention": "<@U123>",
                "github_handle_verified": False,
                "github_handle_unavailable_reason": (
                    "no GitHub custom field found on Slack profile"
                ),
            },
        )

        assert "GitHub handle from Slack profile: unavailable" in ctx
        assert "no GitHub custom field found on Slack profile" in ctx
        assert "GitHub handle verified: no" in ctx
        assert "GitHub PR Attribution" in ctx
        assert "do not infer a GitHub username" in ctx
        assert "Omit the `Prompted by` line" in ctx

    def test_extract_github_handle_from_slack_profile(self):
        from api.agent import _extract_github_handle_from_slack_profile

        handle, source, reason = _extract_github_handle_from_slack_profile(
            {"custom_fields": {"Affiliations": "GitHub: test-user"}}
        )

        assert handle == "@test-user"
        assert source == 'Slack profile custom field "Affiliations"'
        assert reason == ""

    def test_extract_github_handle_unavailable_reason(self):
        from api.agent import _extract_github_handle_from_slack_profile

        handle, source, reason = _extract_github_handle_from_slack_profile(
            {"custom_fields": {"Telegram": "@alice"}}
        )

        assert handle is None
        assert source is None
        assert reason == "no GitHub custom field found on Slack profile"

    @pytest.mark.asyncio
    async def test_latest_thread_user_id_reads_message_metadata(self, db_pool):
        from api.agent import _get_latest_thread_user_id

        thread_key = "test:requester-fallback"
        await db_pool.execute(
            "INSERT INTO chat_messages (id, thread_key, role, parts, metadata) "
            "VALUES ($1, $2, 'user', '[]'::jsonb, $3::jsonb)",
            "msg-requester-fallback",
            thread_key,
            '{"user_id": "U123"}',
        )

        assert await _get_latest_thread_user_id(thread_key) == "U123"

    @pytest.mark.asyncio
    async def test_latest_thread_user_id_reads_execution_delivery(self, db_pool):
        from api.agent import _get_latest_thread_user_id

        thread_key = "test:requester-execution-fallback"
        await db_pool.execute(
            "INSERT INTO agent_execution_requests ("
            "execution_id, thread_key, assignment_generation, execute_id, request_hash, "
            "status, delivery, metadata"
            ") VALUES ($1, $2, 1, $3, 'hash', 'queued', $4::jsonb, '{}'::jsonb)",
            "exe-requester-fallback",
            thread_key,
            "exec-requester-fallback",
            '{"platform": "slack", "recipient_user_id": "U456"}',
        )

        assert await _get_latest_thread_user_id(thread_key) == "U456"

    @pytest.mark.asyncio
    async def test_latest_thread_user_id_reads_slack_workflow_input(self, db_pool):
        from api.agent import _get_latest_thread_user_id

        thread_key = "test:requester-workflow-fallback"
        await db_pool.execute(
            "INSERT INTO workflow_runs ("
            "run_id, workflow_name, workflow_version, request_hash, root_run_id, "
            "thread_key, status, input_json"
            ") VALUES ($1, 'slack_thread_turn', 'test', 'hash', $1, $2, 'queued', $3::jsonb)",
            "wfr-requester-fallback",
            thread_key,
            '{"delivery": {"recipient_user_id": "U789"}}',
        )

        assert await _get_latest_thread_user_id(thread_key) == "U789"

    @pytest.mark.asyncio
    async def test_insert_system_message_uses_thread_user_id_fallback(
        self, db_pool, monkeypatch
    ):
        from api import agent

        thread_key = "test:requester-context-fallback"
        await db_pool.execute(
            "INSERT INTO chat_messages (id, thread_key, role, parts, user_id, metadata) "
            "VALUES ($1, $2, 'user', '[]'::jsonb, 'U123', '{}'::jsonb)",
            "msg-requester-context-fallback",
            thread_key,
        )

        async def fake_resolve_requester_identity(*, platform, user_id):
            assert platform == "slack"
            assert user_id == "U123"
            return {
                "slack_user_id": user_id,
                "slack_mention": f"<@{user_id}>",
                "github_handle": "@alice",
                "github_handle_source": 'Slack profile custom field "GitHub"',
                "github_handle_verified": True,
            }

        monkeypatch.setattr(
            agent,
            "_resolve_requester_identity",
            fake_resolve_requester_identity,
        )

        await agent._insert_system_message(thread_key, "slack")

        row = await db_pool.fetchrow(
            "SELECT parts FROM chat_messages WHERE id = $1",
            f"system-{thread_key}-slack",
        )
        parts = row["parts"]
        if isinstance(parts, str):
            parts = json.loads(parts)
        text = parts[0]["text"]
        assert "Requester Identity" in text
        assert "Slack user ID: U123" in text
        assert "GitHub handle from Slack profile: @alice" in text

    @pytest.mark.asyncio
    async def test_insert_system_message_infers_slack_platform_from_thread_key(
        self, db_pool, monkeypatch
    ):
        from api import agent

        thread_key = "slack:C123:1712345678.000100"
        await db_pool.execute(
            "INSERT INTO chat_messages (id, thread_key, role, parts, user_id, metadata) "
            "VALUES ($1, $2, 'user', '[]'::jsonb, 'U123', '{}'::jsonb)",
            "msg-requester-context-infer-platform",
            thread_key,
        )

        async def fake_resolve_requester_identity(*, platform, user_id):
            assert platform == "slack"
            assert user_id == "U123"
            return {
                "slack_user_id": user_id,
                "slack_mention": f"<@{user_id}>",
                "github_handle_verified": False,
                "github_handle_unavailable_reason": "no GitHub custom field found",
            }

        monkeypatch.setattr(
            agent,
            "_resolve_requester_identity",
            fake_resolve_requester_identity,
        )

        await agent._insert_system_message(thread_key, None)

        row = await db_pool.fetchrow(
            "SELECT parts FROM chat_messages WHERE id = $1",
            f"system-{thread_key}-slack",
        )
        parts = row["parts"]
        if isinstance(parts, str):
            parts = json.loads(parts)
        assert "Requester Identity" in parts[0]["text"]
        assert "Slack user ID: U123" in parts[0]["text"]

    @pytest.mark.asyncio
    async def test_insert_system_message_preserves_created_at_on_refresh(
        self, db_pool, monkeypatch
    ):
        from api import agent

        thread_key = "test:requester-context-order"

        async def fake_resolve_requester_identity(*, platform, user_id):
            return {
                "slack_user_id": user_id,
                "slack_mention": f"<@{user_id}>",
                "github_handle": "@alice",
                "github_handle_source": 'Slack profile custom field "GitHub"',
                "github_handle_verified": True,
            }

        monkeypatch.setattr(
            agent,
            "_resolve_requester_identity",
            fake_resolve_requester_identity,
        )

        await agent._insert_system_message(thread_key, "slack", user_id="U123")
        first_created_at = await db_pool.fetchval(
            "SELECT created_at FROM chat_messages WHERE id = $1",
            f"system-{thread_key}-slack",
        )

        await db_pool.execute(
            "UPDATE chat_messages SET created_at = NOW() + INTERVAL '1 minute' "
            "WHERE id = $1",
            f"system-{thread_key}-slack",
        )
        expected_created_at = await db_pool.fetchval(
            "SELECT created_at FROM chat_messages WHERE id = $1",
            f"system-{thread_key}-slack",
        )

        await agent._insert_system_message(thread_key, "slack", user_id="U123")
        refreshed_created_at = await db_pool.fetchval(
            "SELECT created_at FROM chat_messages WHERE id = $1",
            f"system-{thread_key}-slack",
        )

        assert first_created_at is not None
        assert refreshed_created_at == expected_created_at


# ── Test 7: Status endpoint ──────────────────────────────────────────────────


class TestStatus:
    """Tests for GET /agent/status."""

    @pytest.mark.asyncio
    async def test_status_not_found(self, client, api_key):
        """Status for unknown thread returns not_found."""
        resp = await client.get(
            "/agent/status",
            params={"key": "test:nonexistent"},
            headers={"Authorization": f"Bearer {api_key}"},
        )
        assert resp.status_code == 200
        data = resp.json()
        assert data["status"] == "not_found"

    @pytest.mark.asyncio
    async def test_status_pending_messages_count(self, client, db_pool, api_key):
        """Status includes pending_messages count."""
        thread = "test:status-pending-1"
        await _seed_assignment(db_pool, thread)
        await client.post(
            "/agent/messages",
            headers={"Authorization": f"Bearer {api_key}"},
            json={
                "thread_key": thread,
                "assignment_generation": 1,
                "parts": [{"type": "text", "text": "pending msg"}],
            },
        )
        resp = await client.get(
            "/agent/status",
            params={"key": thread},
            headers={"Authorization": f"Bearer {api_key}"},
        )
        assert resp.status_code == 200
        data = resp.json()
        assert data["pending_messages"] >= 1


class TestDeprecatedEndpoints:
    @pytest.mark.asyncio
    async def test_legacy_agent_connect_returns_gone(self, client):
        resp = await client.post("/agent/connect")
        assert resp.status_code == 410
        assert resp.json()["code"] == "LEGACY_ENDPOINT_REMOVED"

    @pytest.mark.asyncio
    async def test_legacy_agent_reconnect_returns_gone(self, client):
        resp = await client.post("/agent/reconnect")
        assert resp.status_code == 410
        assert resp.json()["code"] == "LEGACY_ENDPOINT_REMOVED"
