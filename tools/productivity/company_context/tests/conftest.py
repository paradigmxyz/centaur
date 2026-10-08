"""Real Postgres behind the pinned iron-proxy for company_context tests.

Set COMPANY_CONTEXT_TEST_DATABASE_URL to a superuser URL for a ParadeDB server
(see .github/scripts/start-test-postgres.sh); Docker must be available. Each
session migrates two databases, one per api-rs text-search backend, and starts
the iron-proxy image pinned by services/iron-proxy/Dockerfile in front of them
with the pg_dsn role and settings this tool declares. Tests seed data directly
and read it through the proxy, as a sandbox does.
"""

from __future__ import annotations

import asyncio
import json
import os
import re
import secrets
import socket
import subprocess
import time
import tomllib
import uuid
from collections.abc import Iterator
from dataclasses import dataclass
from datetime import datetime
from pathlib import Path
from typing import Any
from urllib.parse import urlparse, urlunparse

import asyncpg
import pytest

DATABASE_URL_ENV = "COMPANY_CONTEXT_TEST_DATABASE_URL"
TOOL_DIR = Path(__file__).resolve().parents[1]
REPO_ROOT = Path(__file__).resolve().parents[4]
SESSION_SQLX = REPO_ROOT / "services/api-rs/crates/centaur-session-sqlx"
COMPANY_CONTEXT_SERVICE_MIGRATIONS = REPO_ROOT / "services/company-context/migrations"
IRON_PROXY_DOCKERFILE = REPO_ROOT / "services/iron-proxy/Dockerfile"
EMBEDDING_DIMENSIONS = 1_536
EMBEDDINGS_MODEL = "text-embedding-3-small"

# The requesting principal. Seed helpers default to rows it can see.
TEAM_ID = "T_HOME"
USER_ID = "U_SELF"
USER_EMAIL = "self@example.com"
GOOGLE_SUBJECT = "subject-self"
CHANNEL_ID = "C_HOME"
PRINCIPAL = {
    "slack_channel_id": CHANNEL_ID,
    "slack_team_id": TEAM_ID,
    "slack_history_channel_ids": "[]",
    "slack_user_id": USER_ID,
    "google_subject": GOOGLE_SUBJECT,
}

# Rows seeded by tests; cleared between tests. CASCADE reaches the documents.
SEEDED_TABLES = (
    "slack_sync_channels",
    "slack_sync_users",
    "company_context_documents",
    "google_docs_sync_files",
    "granola_sync_notes",
    "slack_private_sync_conversations",
)


def embedding(*weights: float) -> list[float]:
    """Return a stored-width vector whose leading components are ``weights``."""
    return [*weights, *([0.0] * (EMBEDDING_DIMENSIONS - len(weights)))]


def _database_url(url: str, database: str) -> str:
    return urlunparse(urlparse(url)._replace(path=f"/{database}"))


def _migration_version(path: Path) -> int:
    return int(path.name.split("_", 1)[0])


async def _migrate(url: str, backend: str) -> None:
    """Apply the migrations that api-rs and, on ParadeDB, the company-context service run."""
    conn = await asyncpg.connect(url)
    try:
        migrations = [
            *(SESSION_SQLX / "migrations").glob("*.sql"),
            *(SESSION_SQLX / "search-migrations" / backend).glob("*.sql"),
        ]
        for path in sorted(migrations, key=_migration_version):
            await conn.execute(path.read_text())
        if backend == "paradedb":
            # The service needs pg_search and migrates with this search_path.
            await conn.execute(
                "CREATE SCHEMA IF NOT EXISTS company_context_system;"
                "SET search_path TO company_context_system, public"
            )
            for path in sorted(COMPANY_CONTEXT_SERVICE_MIGRATIONS.glob("*.sql")):
                await conn.execute(path.read_text())
    finally:
        await conn.close()


def _pg_dsn_secret() -> dict[str, Any]:
    pyproject = tomllib.loads((TOOL_DIR / "pyproject.toml").read_text())
    return next(
        secret for secret in pyproject["tool"]["centaur"]["secrets"] if secret["type"] == "pg_dsn"
    )


def _proxy_settings(secret: dict[str, Any]) -> list[dict[str, str]]:
    """Resolve the tool's declared session settings for the test principal."""
    settings = []
    for setting in secret.get("settings", []):
        value = setting.get("value")
        if value is None:
            source = setting["value_from"]
            value = PRINCIPAL[source.get("principal_field") or source["principal_label"]]
        settings.append({"name": setting["name"], "value": value})
    return settings


def _iron_proxy_image() -> str:
    match = re.search(r"^FROM\s+(ironsh/iron-proxy\S+)", IRON_PROXY_DOCKERFILE.read_text(), re.M)
    assert match, f"no iron-proxy base image in {IRON_PROXY_DOCKERFILE}"
    return match.group(1)


def _free_port() -> int:
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


async def _wait_for_proxy(dsn: str, container: str) -> None:
    deadline = time.monotonic() + 60
    while True:
        try:
            conn = await asyncpg.connect(dsn, timeout=2)
        except (TimeoutError, OSError, asyncpg.PostgresError):
            if time.monotonic() > deadline:
                logs = subprocess.run(
                    ["docker", "logs", container], capture_output=True, text=True, check=False
                )
                pytest.fail(f"iron-proxy did not accept connections:\n{logs.stdout}{logs.stderr}")
            await asyncio.sleep(0.25)
        else:
            await conn.close()
            return


@dataclass(frozen=True)
class _Server:
    admin_urls: dict[str, str]
    proxy_dsns: dict[str, str]


@pytest.fixture(scope="session")
def _server() -> Iterator[_Server]:
    admin_url = os.environ.get(DATABASE_URL_ENV, "").strip()  # noqa: TID251 - test configuration
    if not admin_url:
        message = f"set {DATABASE_URL_ENV} to a ParadeDB superuser URL"
        if os.environ.get("CI"):  # noqa: TID251 - test configuration
            pytest.fail(message)
        pytest.skip(message)

    suffix = uuid.uuid4().hex[:12]
    databases = {
        backend: f"company_context_{backend}_{suffix}" for backend in ("paradedb", "postgres")
    }
    container = f"company-context-iron-proxy-{suffix}"
    secret = _pg_dsn_secret()
    password = secrets.token_hex(16)
    port = _free_port()

    async def create() -> None:
        admin = await asyncpg.connect(admin_url)
        try:
            for database in databases.values():
                await admin.execute(f'CREATE DATABASE "{database}"')
        finally:
            await admin.close()
        for backend, database in databases.items():
            await _migrate(_database_url(admin_url, database), backend)

    async def drop() -> None:
        admin = await asyncpg.connect(admin_url)
        try:
            for database in databases.values():
                await admin.execute(f'DROP DATABASE IF EXISTS "{database}" WITH (FORCE)')
        finally:
            await admin.close()

    upstream_env = {
        f"UPSTREAM_DSN_{backend.upper()}": _database_url(admin_url, database)
        for backend, database in databases.items()
    }
    config = {
        # Only the Postgres listener matters; keep the HTTP side off public ports.
        "dns": {"enabled": False},
        "proxy": {"http_listen": "127.0.0.1:0", "https_listen": "127.0.0.1:0"},
        "tls": {"mode": "sni-only"},
        "metrics": {"listen": "127.0.0.1:0"},
        "postgres": {
            "listen": f"127.0.0.1:{port}",
            "client": {"user": "sandbox", "password_env": "PG_CLIENT_PASSWORD"},
            "upstreams": [
                {
                    "database": database,
                    "dsn": {"type": "env", "var": f"UPSTREAM_DSN_{backend.upper()}"},
                    "role": secret["role"],
                    "settings": _proxy_settings(secret),
                }
                for backend, database in databases.items()
            ],
        },
    }
    env = {"PG_CLIENT_PASSWORD": password, "PROXY_CONFIG": json.dumps(config), **upstream_env}

    asyncio.run(create())
    try:
        subprocess.run(
            [
                "docker",
                "run",
                "--detach",
                "--rm",
                "--name",
                container,
                "--network",
                "host",
                *(arg for name in env for arg in ("--env", name)),
                "--entrypoint",
                "sh",
                _iron_proxy_image(),
                "-c",
                'printf "%s" "$PROXY_CONFIG" > /tmp/proxy.yaml && exec iron-proxy -config /tmp/proxy.yaml',
            ],
            check=True,
            capture_output=True,
            env={**os.environ, **env},
        )
        try:
            proxy_dsns = {
                backend: f"postgresql://sandbox:{password}@127.0.0.1:{port}/{database}"
                for backend, database in databases.items()
            }
            for dsn in proxy_dsns.values():
                asyncio.run(_wait_for_proxy(dsn, container))
            yield _Server(
                admin_urls={
                    backend: _database_url(admin_url, database)
                    for backend, database in databases.items()
                },
                proxy_dsns=proxy_dsns,
            )
        finally:
            subprocess.run(["docker", "rm", "--force", container], capture_output=True, check=False)
    finally:
        asyncio.run(drop())


class Database:
    """One migrated database: seed it directly, read it through iron-proxy."""

    def __init__(self, *, backend: str, admin_url: str, dsn: str) -> None:
        self.backend = backend
        self.admin_url = admin_url
        self.dsn = dsn

    def execute(self, sql: str, *args: Any) -> None:
        async def run() -> None:
            conn = await asyncpg.connect(self.admin_url)
            try:
                await conn.execute(sql, *args)
            finally:
                await conn.close()

        asyncio.run(run())

    def clear(self) -> None:
        self.execute(f"TRUNCATE {', '.join(SEEDED_TABLES)} CASCADE")

    def add_slack_channel(self, channel_id: str = CHANNEL_ID, *, is_private: bool = False) -> None:
        self.execute(
            "INSERT INTO slack_sync_channels (channel_id, channel_name, is_private) "
            "VALUES ($1, $1, $2) ON CONFLICT (channel_id) DO NOTHING",
            channel_id,
            is_private,
        )

    def add_document(
        self,
        document_id: str,
        *,
        title: str = "",
        body: str = "",
        source: str = "slack",
        source_type: str = "slack_thread",
        channel_id: str = CHANNEL_ID,
        source_document_id: str | None = None,
        parent_document_id: str | None = None,
        url: str = "",
        author_name: str = "",
        access_scope: str = "company",
        occurred_at: datetime | None = None,
        source_updated_at: datetime | None = None,
        metadata: dict[str, Any] | None = None,
        embedding: list[float] | None = None,
    ) -> None:
        """Insert a company_context_documents row; Slack rows join a public channel."""
        if source == "slack":
            self.add_slack_channel(channel_id)
        self.execute(
            "INSERT INTO company_context_documents (document_id, source, source_type, "
            "source_document_id, parent_document_id, title, body, url, author_name, "
            "access_scope, occurred_at, source_updated_at, metadata) "
            "VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13::jsonb)",
            document_id,
            source,
            source_type,
            source_document_id or document_id,
            parent_document_id,
            title,
            body,
            url,
            author_name,
            access_scope,
            occurred_at,
            source_updated_at,
            json.dumps({"channel_id": channel_id, **(metadata or {})}),
        )
        if embedding is not None:
            self.add_embedding("company_context_document_id", document_id, embedding)

    def add_google_doc(
        self,
        document_id: str,
        *,
        file_id: str,
        title: str = "",
        body: str = "",
        subject: str = GOOGLE_SUBJECT,
        created_at: datetime | None = None,
        modified_at: datetime | None = None,
        embedding: list[float] | None = None,
    ) -> None:
        """Insert an OAuth Google Docs chunk observed by ``subject``."""
        self.execute(
            "INSERT INTO google_docs_sync_files (file_id, name) VALUES ($1, $2) "
            "ON CONFLICT (file_id) DO NOTHING",
            file_id,
            title,
        )
        self.execute(
            "INSERT INTO google_docs_sync_file_observations "
            "(broker_credential_id, observed_file_id, file_id, provider_subject) "
            "VALUES ($1, $2, $2, $1) ON CONFLICT DO NOTHING",
            subject,
            file_id,
        )
        self.execute(
            "INSERT INTO google_docs_context_documents (document_id, file_id, chunk_id, title, "
            "body, url, provider_author_name, mime_type, source_created_at, source_modified_at) "
            "VALUES ($1, $2, '0', $3, $4, $5, 'Alice', 'application/vnd.google-apps.document', "
            "$6, $7)",
            document_id,
            file_id,
            title,
            body,
            f"https://docs.google.com/document/d/{file_id}/edit",
            created_at,
            modified_at,
        )
        if embedding is not None:
            self.add_embedding("google_docs_context_document_id", document_id, embedding)

    def add_granola_note(
        self,
        note_id: str,
        *,
        title: str = "",
        body: str = "",
        access_emails: tuple[str, ...] = (USER_EMAIL,),
        occurred_at: datetime | None = None,
        embedding: list[float] | None = None,
    ) -> None:
        """Sync a Granola note shared with ``access_emails``; api-rs projects it."""
        # Granola visibility resolves the principal's email from its Slack profile.
        self.execute(
            "INSERT INTO slack_sync_users (user_id, team_id, raw_payload) "
            "VALUES ($1, $2, $3::jsonb) ON CONFLICT (user_id) DO NOTHING",
            USER_ID,
            TEAM_ID,
            json.dumps({"profile": {"email": USER_EMAIL}}),
        )
        self.execute(
            "INSERT INTO granola_sync_notes (note_id, title, content_text, url, owner_name, "
            "access_emails, source_created_at, source_updated_at) "
            "VALUES ($1, $2, $3, $4, 'Alice', $5, $6, $6)",
            note_id,
            title,
            body,
            f"https://app.granola.ai/notes/{note_id}",
            list(access_emails),
            occurred_at,
        )
        if embedding is not None:
            self.add_embedding("granola_context_document_id", f"granola:note:{note_id}", embedding)

    def add_slack_conversation(
        self,
        conversation_id: str,
        *,
        conversation_type: str = "im",
        member: bool = True,
        participants: dict[str, str] | None = None,
        last_seen_at: datetime | None = None,
    ) -> None:
        """Sync a private Slack conversation; api-rs projects it for search.

        ``participants`` maps other members' user ids to display names.
        """
        self.execute(
            "INSERT INTO slack_private_sync_conversations "
            "(home_team_id, conversation_id, conversation_type, last_seen_at) "
            "VALUES ($1, $2, $3, COALESCE($4, now()))",
            TEAM_ID,
            conversation_id,
            conversation_type,
            last_seen_at,
        )
        members = dict(participants or {})
        if member:
            members.setdefault(USER_ID, USER_ID)
        for user_id, display_name in members.items():
            self.execute(
                "INSERT INTO slack_private_sync_conversation_members "
                "(home_team_id, conversation_id, user_id, raw_payload) "
                "VALUES ($1, $2, $3, $4::jsonb)",
                TEAM_ID,
                conversation_id,
                user_id,
                json.dumps({"display_name": display_name}),
            )

    def add_slack_message(
        self,
        conversation_id: str,
        message_ts: str,
        *,
        body: str,
        user_id: str = USER_ID,
        occurred_at: datetime | None = None,
    ) -> None:
        """Sync a message in a conversation from add_slack_conversation."""
        self.execute(
            "INSERT INTO slack_private_sync_messages "
            "(home_team_id, conversation_id, message_ts, occurred_at, user_id, text, permalink) "
            "VALUES ($1, $2, $3, $4, $5, $6, $7)",
            TEAM_ID,
            conversation_id,
            message_ts,
            occurred_at,
            user_id,
            body,
            f"https://slack.example/archives/{conversation_id}/p{message_ts.replace('.', '')}",
        )

    def add_embedding(self, column: str, document_id: str, vector: list[float]) -> None:
        self.execute(
            f"INSERT INTO company_context_document_embeddings ({column}, model, content_hash, "
            "embedding) VALUES ($1, $2, 'hash', $3::text::vector)",
            document_id,
            EMBEDDINGS_MODEL,
            json.dumps(vector),
        )


def _database(server: _Server, backend: str) -> Iterator[Database]:
    database = Database(
        backend=backend,
        admin_url=server.admin_urls[backend],
        dsn=server.proxy_dsns[backend],
    )
    try:
        yield database
    finally:
        database.clear()


@pytest.fixture(params=["paradedb", "postgres"])
def database(request, _server: _Server) -> Iterator[Database]:
    """A database migrated for each api-rs text-search backend."""
    yield from _database(_server, request.param)
