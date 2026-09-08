from __future__ import annotations

import asyncio
import json
import os
import sys
import tempfile
import unittest
import uuid
from pathlib import Path
from unittest.mock import patch
from urllib.parse import parse_qsl, urlencode, urlsplit, urlunsplit

import asyncpg

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from api.workflow_db import MigrationError, WorkflowDatabase

DATABASE_URL = os.environ.get("WORKFLOW_DATABASE_TEST_URL")


class WorkflowDatabaseConfigurationTests(unittest.IsolatedAsyncioTestCase):
    async def test_database_access_requires_configuration(self):
        db = WorkflowDatabase(None)
        with self.assertRaisesRegex(RuntimeError, "DATABASE_URL"):
            await db.fetchval("SELECT 1")
        with self.assertRaisesRegex(RuntimeError, "DATABASE_URL"):
            await db.migrate("migrations", schema="example")


@unittest.skipUnless(DATABASE_URL, "WORKFLOW_DATABASE_TEST_URL is not configured")
class WorkflowDatabaseTests(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        self.environment = patch.dict(os.environ, {"DATABASE_URL": DATABASE_URL})
        self.environment.start()
        self.addCleanup(self.environment.stop)
        self.pool = await asyncpg.create_pool(DATABASE_URL, min_size=1, max_size=6)
        self.addAsyncCleanup(self.pool.close)
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.directory = Path(self.tmp.name)
        self.migrations = self.directory / "migrations"
        self.migrations.mkdir()
        self.schema = "workflow_test_" + uuid.uuid4().hex
        self.addAsyncCleanup(self.drop_schema, self.schema)
        self.db = WorkflowDatabase(
            self.pool, source_path=self.directory / "workflow.py"
        )

    async def drop_schema(self, schema):
        quoted = '"' + schema.replace('"', '""') + '"'
        await self.pool.execute(f"DROP SCHEMA IF EXISTS {quoted} CASCADE")

    def write(self, filename, sql):
        (self.migrations / filename).write_text(sql, encoding="utf-8")

    def initial_migration(self):
        self.write(
            "001_checkpoints.sql",
            f"CREATE TABLE {self.schema}.checkpoints (id text PRIMARY KEY, cursor bigint);"
            f"INSERT INTO {self.schema}.checkpoints VALUES ('source', 42);",
        )

    async def migrate(self, **kwargs):
        return await self.db.migrate("./migrations", schema=self.schema, **kwargs)

    async def history(self):
        rows = await self.pool.fetch(
            f"SELECT version FROM {self.schema}._sqlx_migrations ORDER BY version"
        )
        return [row["version"] for row in rows]

    async def test_upgrade_preserves_data_and_replay_skips_applied_files(self):
        self.initial_migration()
        self.assertEqual(await self.migrate(), [1])
        self.assertEqual(await self.migrate(), [])
        self.write(
            "002_label.sql",
            f"ALTER TABLE {self.schema}.checkpoints ADD COLUMN label text;",
        )
        self.assertEqual(await self.migrate(), [2])
        row = await self.db.fetchrow(f"SELECT * FROM {self.schema}.checkpoints")
        self.assertEqual(dict(row), {"id": "source", "cursor": 42, "label": None})
        async with self.db.acquire() as connection, connection.transaction():
            await connection.execute(
                f"UPDATE {self.schema}.checkpoints SET cursor = 43"
            )
        self.assertEqual(
            await self.db.fetchval(f"SELECT cursor FROM {self.schema}.checkpoints"), 43
        )
        self.assertEqual(
            len(await self.db.fetch(f"SELECT * FROM {self.schema}.checkpoints")), 1
        )
        await self.db.execute(
            f"UPDATE {self.schema}.checkpoints SET label = $1", "updated"
        )
        # A renamed/moved workflow retains history because identity is the schema.
        moved = WorkflowDatabase(self.pool, source_path=self.directory / "renamed.py")
        self.assertEqual(await moved.migrate(self.migrations, schema=self.schema), [])
        # An older deployed bundle can still use a forward-compatible schema.
        (self.migrations / "002_label.sql").unlink()
        self.assertEqual(await self.migrate(), [])

    async def test_sqlx_rejects_changed_applied_migration(self):
        self.initial_migration()
        await self.migrate()
        self.write(
            "001_checkpoints.sql", f"UPDATE {self.schema}.checkpoints SET cursor = 99;"
        )
        with self.assertRaisesRegex(MigrationError, "previously applied.*modified"):
            await self.migrate()
        self.assertEqual(
            await self.db.fetchval(f"SELECT cursor FROM {self.schema}.checkpoints"), 42
        )

    async def test_concurrent_first_runs_apply_each_file_once(self):
        self.initial_migration()
        results = await asyncio.wait_for(
            asyncio.gather(*(self.migrate() for _ in range(6))), 10
        )
        self.assertEqual(sum(len(result) for result in results), 1)
        self.assertEqual(await self.history(), [1])
        self.assertEqual(
            await self.pool.fetchval(f"SELECT count(*) FROM {self.schema}.checkpoints"),
            1,
        )

    async def test_versions_sort_numerically(self):
        self.write("2_create.sql", f"CREATE TABLE {self.schema}.items (id int);")
        self.write("10_insert.sql", f"INSERT INTO {self.schema}.items VALUES (7);")
        self.assertEqual(await self.migrate(), [2, 10])
        self.assertEqual(
            await self.db.fetchval(f"SELECT id FROM {self.schema}.items"), 7
        )

    async def test_schema_history_is_isolated_from_public_sqlx_history(self):
        # A deliberately incompatible stand-in catches accidental public lookup.
        await self.pool.execute("CREATE TABLE public._sqlx_migrations (marker text)")
        self.addAsyncCleanup(self.pool.execute, "DROP TABLE public._sqlx_migrations")
        await self.pool.execute(
            "INSERT INTO public._sqlx_migrations VALUES ('control-plane')"
        )
        self.write(
            "001_items.sql",
            "CREATE TABLE items (id int); INSERT INTO items VALUES (7);",
        )
        self.assertEqual(await self.migrate(), [1])
        self.assertEqual(
            await self.db.fetchval(f"SELECT id FROM {self.schema}.items"), 7
        )
        self.assertEqual(
            await self.pool.fetchval("SELECT marker FROM public._sqlx_migrations"),
            "control-plane",
        )
        self.assertEqual(await self.history(), [1])

    async def test_preprovisioned_schema_works_without_database_create_privilege(self):
        role = "migration_role_" + uuid.uuid4().hex
        await self.pool.execute(f"CREATE ROLE {role}")

        async def drop_role():
            await self.drop_schema(self.schema)
            await self.pool.execute(f"DROP ROLE {role}")

        self.addAsyncCleanup(drop_role)
        await self.pool.execute(f"CREATE SCHEMA {self.schema} AUTHORIZATION {role}")
        restricted = await asyncpg.create_pool(
            DATABASE_URL, min_size=1, max_size=1, server_settings={"role": role}
        )
        self.addAsyncCleanup(restricted.close)
        self.assertFalse(
            await restricted.fetchval(
                "SELECT has_database_privilege(current_user, current_database(), 'CREATE')"
            )
        )
        self.initial_migration()
        db = WorkflowDatabase(restricted)
        parts = urlsplit(DATABASE_URL)
        query = dict(parse_qsl(parts.query))
        query["options"] = f"-c role={role}"
        restricted_url = urlunsplit(parts._replace(query=urlencode(query)))
        with patch.dict(os.environ, {"DATABASE_URL": restricted_url}):
            self.assertEqual(await db.migrate(self.migrations, schema=self.schema), [1])
        self.assertEqual(
            await db.fetchval(f"SELECT cursor FROM {self.schema}.checkpoints"), 42
        )

    async def test_failure_rolls_back_only_failed_file_and_retry_resumes(self):
        self.initial_migration()
        self.write(
            "002_broken.sql",
            f"CREATE TABLE {self.schema}.pending (id int); SELECT 1/0;",
        )
        with self.assertRaisesRegex(MigrationError, "migration 2"):
            await self.migrate()
        self.assertEqual(await self.history(), [1])
        self.assertIsNone(
            await self.pool.fetchval("SELECT to_regclass($1)", f"{self.schema}.pending")
        )
        self.write("002_broken.sql", f"CREATE TABLE {self.schema}.pending (id int);")
        self.assertEqual(await self.migrate(), [2])

    async def test_sql_functions_comments_and_quoted_schema_names(self):
        schema = self.schema + '"odd'
        self.addAsyncCleanup(self.drop_schema, schema)
        quoted = '"' + schema.replace('"', '""') + '"'
        self.write(
            "001_function.sql",
            f"CREATE FUNCTION {quoted}.message() RETURNS text LANGUAGE sql AS $$\n"
            "SELECT $workflow_migration$COMMIT; 'hello'; \\path; café$workflow_migration$\n"
            "$$;\n-- COMMIT is text, not a transaction statement here.\n"
            f"CREATE TABLE {quoted}.results AS SELECT {quoted}.message() AS value;",
        )
        await self.db.migrate(self.migrations, schema=schema)
        self.assertEqual(
            await self.pool.fetchval(f"SELECT value FROM {quoted}.results"),
            "COMMIT; 'hello'; \\path; café",
        )

    async def wait_for_sleeping_migration(self):
        async with asyncio.timeout(5):
            while True:
                pid = await self.pool.fetchval(
                    "SELECT pid FROM pg_stat_activity WHERE pid <> pg_backend_pid() "
                    "AND wait_event = 'PgSleep' AND strpos(query, $1) > 0",
                    self.schema,
                )
                if pid:
                    return pid
                await asyncio.sleep(0.01)

    async def start_sleeping_migration(self):
        self.write(
            "001_wait.sql",
            f"CREATE TABLE {self.schema}.pending (id int); SELECT pg_sleep(30);",
        )
        task = asyncio.create_task(self.migrate())

        async def stop_task():
            if not task.done():
                task.cancel()
            await asyncio.gather(task, return_exceptions=True)

        self.addAsyncCleanup(stop_task)
        pid = await self.wait_for_sleeping_migration()
        return task, pid

    async def test_cancellation_rolls_back_and_releases_lock(self):
        task, _ = await self.start_sleeping_migration()
        task.cancel()
        with self.assertRaises(asyncio.CancelledError):
            await task
        self.assertEqual(await self.history(), [])
        self.write("001_wait.sql", f"CREATE TABLE {self.schema}.pending (id int);")
        self.assertEqual(await asyncio.wait_for(self.migrate(), 5), [1])

    async def test_lost_database_connection_rolls_back_and_retry_recovers(self):
        task, pid = await self.start_sleeping_migration()
        self.assertTrue(
            await self.pool.fetchval("SELECT pg_terminate_backend($1)", pid)
        )
        with self.assertRaises(MigrationError):
            await task
        self.assertEqual(await self.history(), [])
        self.write("001_wait.sql", f"CREATE TABLE {self.schema}.pending (id int);")
        self.assertEqual(await asyncio.wait_for(self.migrate(), 5), [1])

    async def test_sqlx_lock_serializes_schemas_and_times_out(self):
        task, _ = await self.start_sleeping_migration()
        with self.assertRaisesRegex(MigrationError, "timed out"):
            await self.migrate(lock_timeout=0.05)
        other = self.schema + "_other"
        self.addAsyncCleanup(self.drop_schema, other)
        directory = self.directory / "other"
        directory.mkdir()
        (directory / "001_wait.sql").write_text(f"CREATE TABLE {other}.items(id int);")
        with self.assertRaisesRegex(MigrationError, "timed out"):
            await self.db.migrate(directory, schema=other, lock_timeout=0.05)
        task.cancel()
        with self.assertRaises(asyncio.CancelledError):
            await task
        self.assertEqual(await self.db.migrate(directory, schema=other), [1])
        self.write("001_wait.sql", f"CREATE TABLE {self.schema}.pending (id int);")
        self.assertEqual(await asyncio.wait_for(self.migrate(), 5), [1])

    async def start_host(self):
        host = Path(__file__).resolve().parents[1] / "workflow_host.py"
        return await asyncio.create_subprocess_exec(
            sys.executable,
            str(host),
            cwd=self.directory.parent,
            stdin=asyncio.subprocess.PIPE,
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE,
            env={
                **os.environ,
                "DATABASE_URL": DATABASE_URL,
                "WORKFLOW_DIRS": str(self.directory),
                "WORKFLOW_ENABLE_MODE": "all",
            },
        )

    async def run_host(self):
        process = await self.start_host()
        try:
            stdout, stderr = await asyncio.wait_for(
                process.communicate(
                    json.dumps(
                        {
                            "type": "workflow.start",
                            "workflow_name": "database_test",
                            "run_id": "test-run",
                            "task_id": "test-task",
                            "input": {},
                        }
                    ).encode()
                    + b"\n"
                ),
                15,
            )
        finally:
            if process.returncode is None:
                process.kill()
                await process.wait()
        self.assertEqual(process.returncode, 0, stderr.decode())
        # Parsing the whole stdout also proves migration output did not corrupt NDJSON.
        return json.loads(stdout)

    async def test_abrupt_host_exit_stops_runner_and_allows_retry(self):
        self.write(
            "001_wait.sql",
            f"CREATE TABLE {self.schema}.pending (id int); SELECT pg_sleep(30);",
        )
        (self.directory / "workflow.py").write_text(
            'WORKFLOW_NAME = "database_test"\n'
            "async def handler(inp, ctx):\n"
            f'    await ctx.db.migrate("./migrations", schema="{self.schema}")\n'
        )
        process = await self.start_host()
        try:
            process.stdin.write(
                b'{"type":"workflow.start","workflow_name":"database_test"}\n'
            )
            await process.stdin.drain()
            await self.wait_for_sleeping_migration()
        finally:
            if process.returncode is None:
                process.kill()
            await process.communicate()
        self.write("001_wait.sql", f"CREATE TABLE {self.schema}.pending (id int);")
        self.assertEqual(await asyncio.wait_for(self.migrate(), 5), [1])

    async def test_real_host_resolves_paths_and_reports_results_and_migration_failure(
        self,
    ):
        self.initial_migration()
        (self.directory / "workflow.py").write_text(
            'WORKFLOW_NAME = "database_test"\n'
            "async def handler(inp, ctx):\n"
            f'    applied = await ctx.db.migrate("./migrations", schema="{self.schema}")\n'
            f'    cursor = await ctx.db.fetchval("SELECT cursor FROM {self.schema}.checkpoints")\n'
            '    return {"applied": applied, "cursor": cursor}\n'
        )
        first = await self.run_host()
        self.assertEqual(first["type"], "workflow.result")
        self.assertEqual(first["result"], {"applied": [1], "cursor": 42})
        second = await self.run_host()
        self.assertEqual(second["result"], {"applied": [], "cursor": 42})
        self.write("002_broken.sql", "SELECT 1/0;")
        failure = await self.run_host()
        self.assertEqual(failure["type"], "workflow.error")
        self.assertIn("migration 2", failure["message"])
        self.assertEqual(await self.history(), [1])
