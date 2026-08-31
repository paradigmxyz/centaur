from __future__ import annotations

import hashlib
import json
import os
import sys
import unittest
import uuid
from pathlib import Path
from urllib.parse import urlsplit, urlunsplit

import asyncpg

WORKFLOW_PYTHON = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(WORKFLOW_PYTHON))

from api.heartbeat import HeartbeatState, _uuid

DATABASE_URL = os.getenv("HEARTBEAT_TEST_DATABASE_URL")
MIGRATIONS = (
    Path(__file__).resolve().parents[2]
    / "api-rs"
    / "crates"
    / "centaur-session-sqlx"
    / "migrations"
)


@unittest.skipUnless(
    DATABASE_URL, "set HEARTBEAT_TEST_DATABASE_URL to run Postgres tests"
)
class HeartbeatPostgresTests(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self) -> None:
        assert DATABASE_URL is not None
        self.pool = await asyncpg.create_pool(DATABASE_URL, min_size=1, max_size=1)
        for migration in (
            "0053_heartbeat_state.sql",
            "0054_memory_facts.sql",
            "0055_heartbeat_workflow_roles.sql",
            "0056_heartbeat_memory_lifecycle.sql",
            "0057_heartbeat_rls_principal_scope.sql",
            "0058_heartbeat_memory_and_draft_facades.sql",
            "0059_heartbeat_feedback_semantics.sql",
            "0060_heartbeat_draft_grant_replay.sql",
            "0061_heartbeat_synthesis_attempt_artifact.sql",
            "0062_heartbeat_memory_completion.sql",
        ):
            if migration == "0062_heartbeat_memory_completion.sql":
                await self.pool.execute(
                    """
                    do $$ begin
                        if not exists (select 1 from pg_roles where rolname = 'centaur_readonly') then
                            create role centaur_readonly nologin;
                        end if;
                        if not exists (select 1 from pg_roles where rolname = 'heartbeat_readonly_client') then
                            create role heartbeat_readonly_client login password 'heartbeat-test';
                        end if;
                    end $$
                    """
                )
                await self.pool.execute(
                    """
                    create table if not exists company_context_documents (
                        document_id text primary key,
                        source text not null,
                        source_type text not null,
                        source_document_id text not null,
                        source_chunk_id text not null default '',
                        parent_document_id text,
                        title text not null default '',
                        body text not null default '',
                        url text not null default '',
                        author_id text not null default '',
                        author_name text not null default '',
                        access_scope text not null default 'company',
                        occurred_at timestamptz,
                        source_updated_at timestamptz,
                        content_hash text not null default '',
                        metadata jsonb not null default '{}'::jsonb,
                        created_at timestamptz not null default now(),
                        updated_at timestamptz not null default now(),
                        unique (source, source_type, source_document_id, source_chunk_id)
                    )
                    """
                )
                await self.pool.execute(
                    "alter table company_context_documents enable row level security"
                )
                await self.pool.execute(
                    "alter table company_context_documents force row level security"
                )
            await self.pool.execute((MIGRATIONS / migration).read_text())
        await self.pool.execute(
            "grant centaur_readonly to heartbeat_readonly_client"
        )
        await self.pool.execute(
            "grant select on company_context_documents to centaur_readonly"
        )
        await self.pool.execute(
            "select heartbeat_bind_workflow_principal($1::name, $2)",
            "centaur_heartbeat_run",
            "workflow-heartbeat-run",
        )
        await self.pool.execute(
            "select heartbeat_bind_workflow_principal($1::name, $2)",
            "centaur_heartbeat_feedback",
            "workflow-heartbeat-feedback",
        )
        await self.pool.execute(
            "select heartbeat_bind_workflow_principal($1::name, $2)",
            "centaur_heartbeat_prepare_action",
            "workflow-heartbeat-prepare-action",
        )
        await self.pool.execute(
            """
            do $$ begin
                if not exists (select 1 from pg_roles where rolname = 'heartbeat_test_client') then
                    create role heartbeat_test_client login password 'heartbeat-test';
                end if;
                if not exists (select 1 from pg_roles where rolname = 'heartbeat_feedback_client') then
                    create role heartbeat_feedback_client login password 'heartbeat-test';
                end if;
                if not exists (select 1 from pg_roles where rolname = 'heartbeat_other_client') then
                    create role heartbeat_other_client login password 'heartbeat-test';
                end if;
                if not exists (select 1 from pg_roles where rolname = 'centaur_heartbeat_run_other') then
                    create role centaur_heartbeat_run_other nologin;
                end if;
            end $$
            """
        )
        await self.pool.execute(
            "grant centaur_heartbeat_run to heartbeat_test_client"
        )
        await self.pool.execute(
            "grant centaur_heartbeat_feedback to heartbeat_feedback_client"
        )
        await self.pool.execute(
            "grant centaur_heartbeat_run_other to heartbeat_other_client"
        )
        self.pool.terminate()

        async def assume_run_role(connection: asyncpg.Connection) -> None:
            await connection.execute("set role centaur_heartbeat_run")
            await connection.execute(
                "set centaur.workflow_principal = 'workflow-heartbeat-run'"
            )

        database_url = urlsplit(DATABASE_URL)
        client_database_url = urlunsplit(
            (
                database_url.scheme,
                f"heartbeat_test_client:heartbeat-test@{database_url.hostname}"
                + (f":{database_url.port}" if database_url.port else ""),
                database_url.path,
                database_url.query,
                database_url.fragment,
            )
        )
        self.client_database_url = client_database_url
        self.pool = await asyncpg.create_pool(
            client_database_url, min_size=1, max_size=1, setup=assume_run_role
        )

    async def asyncTearDown(self) -> None:
        self.pool.terminate()

    def state(
        self,
        *,
        run_id: uuid.UUID,
        task_id: uuid.UUID | None = None,
        workflow_name: str = "heartbeat_run",
        principal: str = "workflow-heartbeat-run",
    ) -> HeartbeatState:
        return HeartbeatState(
            self.pool,
            workflow_name=workflow_name,
            workflow_run_id=str(run_id),
            workflow_task_id=str(task_id or uuid.uuid4()),
            workflow_principal=principal,
        )

    async def test_feedback_role_cannot_read_memory(self) -> None:
        async def assume_feedback_role(connection: asyncpg.Connection) -> None:
            await connection.execute("set role centaur_heartbeat_feedback")

        feedback_pool = await asyncpg.create_pool(
            self.client_database_url.replace(
                "heartbeat_test_client", "heartbeat_feedback_client"
            ),
            min_size=1,
            max_size=1,
            setup=assume_feedback_role,
        )
        try:
            self.assertEqual(
                await feedback_pool.fetchval("select count(*) from heartbeat_items"),
                0,
            )
            with self.assertRaises(asyncpg.InsufficientPrivilegeError):
                await feedback_pool.fetchval("select count(*) from memory_facts")
        finally:
            feedback_pool.terminate()

        async with self.pool.acquire() as connection:
            async with connection.transaction():
                await connection.execute("set local role centaur_heartbeat_run")
                self.assertEqual(await connection.fetchval("select count(*) from memory_facts"), 0)
                with self.assertRaises(asyncpg.InsufficientPrivilegeError):
                    await connection.execute("delete from memory_facts")

    async def test_replay_action_and_memory_proposal_are_idempotent_and_authorized(
        self,
    ) -> None:
        first_run_id = uuid.uuid4()
        first = self.state(run_id=first_run_id)
        definition = {
            "namespace": "default",
            "name": "test-profile",
            "scope_kind": "team",
            "scope_ref": "gtm",
            "definition_hash": "definition-v1",
            "definition_version": 1,
            "destination": {"kind": "slack", "ref": "C123"},
            "required_sources": ["linear"],
            "optional_sources": [],
            "delivery_policy": {"posture": "read_and_draft_only"},
            "reviewer_refs": ["U-REVIEWER"],
            "enabled": True,
        }
        profile = await first.register_profile(definition)
        second_principal = "workflow-heartbeat-other"
        second_role = "centaur_heartbeat_run_other"
        second_profile_id = uuid.uuid4()
        admin_pool = await asyncpg.create_pool(
            DATABASE_URL, min_size=1, max_size=1
        )
        try:
            await admin_pool.execute(
                f"do $$ begin if not exists (select 1 from pg_roles where rolname = '{second_role}') then create role {second_role} nologin; end if; end $$"
            )
            await admin_pool.execute(
                f"grant centaur_heartbeat_run to {second_role}"
            )
            await admin_pool.execute(
                "select heartbeat_bind_workflow_principal($1::name, $2)",
                second_role,
                second_principal,
            )
            await admin_pool.execute(
                """
                insert into heartbeat_profiles (
                    profile_id, namespace, name, scope_kind, scope_ref,
                    workflow_name, executor_principal_foreign_id, definition_hash,
                    definition_version, destination, required_sources,
                    optional_sources, delivery_policy, enabled
                ) values ($1, 'default', 'other-principal-profile', 'team', 'gtm',
                    'heartbeat_run', $2, 'definition-v1', 1, '{}'::jsonb,
                    '{}', '{}', '{}'::jsonb, true)
                """,
                second_profile_id,
                second_principal,
            )
        finally:
            admin_pool.terminate()
        run = await first.begin_run(
            profile_id=str(profile["profile_id"]),
            trigger="replay",
            definition_hash="definition-v1",
            prompt_version="test-v1",
        )
        other_profile = await first.register_profile(
            {**definition, "name": "other-test-profile"}
        )
        with self.assertRaises(PermissionError):
            await first.commit_source_batch(
                profile_id=str(other_profile["profile_id"]),
                run_id=str(run["run_id"]),
                source_key="linear",
                observations=[],
                items=[],
            )
        observation = {
            "source_object_id": "ENG-1",
            "source_revision": "2026-08-30T00:00:00Z",
            "source_updated_at": "2026-08-30T00:00:00Z",
            "content_hash": hashlib.sha256(b"issue-v1").hexdigest(),
            "entity_keys": ["account:acme"],
            "title": "Acme review",
            "source_url": "https://linear.app/acme/ENG-1",
            "payload": {"status": "In Progress"},
            "sensitivity": "internal",
        }
        item = {
            "story_key": "linear:ENG-1",
            "material_hash": observation["content_hash"],
            "title": "Acme review",
            "item_type": "work",
            "entity_keys": ["account:acme"],
            "priority_tier": 1,
            "observation_refs": [
                {
                    "source_object_id": observation["source_object_id"],
                    "source_revision": observation["source_revision"],
                    "relation": "primary",
                }
            ],
        }
        committed = await first.commit_source_batch(
            profile_id=str(profile["profile_id"]),
            run_id=str(run["run_id"]),
            source_key="linear",
            observations=[observation],
            items=[item],
        )
        self.assertEqual(committed["inserted_observations"], 1)
        self.assertEqual(committed["changed_items"], 1)
        item_uuid = await self.pool.fetchval(
            "select item_id from heartbeat_items where profile_id = $1 and story_key = $2",
            profile["profile_id"],
            item["story_key"],
        )
        await self.pool.execute(
            """
            update heartbeat_items set status = 'snoozed', snooze_until = now() + interval '1 day',
                   version = version + 1 where item_id = $1
            """,
            item_uuid,
        )
        with self.assertRaisesRegex(RuntimeError, "changed immutable content"):
            await first.commit_source_batch(
                profile_id=str(profile["profile_id"]),
                run_id=str(run["run_id"]),
                source_key="linear",
                observations=[
                    {
                        **observation,
                        "content_hash": hashlib.sha256(b"issue-mutated").hexdigest(),
                    }
                ],
                items=[],
                expected_checkpoint_version=1,
            )
        revised_observation = {
            **observation,
            "source_revision": "2026-08-30T01:00:00Z",
            "source_updated_at": "2026-08-30T01:00:00Z",
            "content_hash": hashlib.sha256(b"issue-v2").hexdigest(),
            "payload": {"status": "Done"},
        }
        revised_item = {
            **item,
            "material_hash": revised_observation["content_hash"],
            "observation_refs": [
                {
                    "source_object_id": revised_observation["source_object_id"],
                    "source_revision": revised_observation["source_revision"],
                    "relation": "primary",
                }
            ],
        }
        await first.commit_source_batch(
            profile_id=str(profile["profile_id"]),
            run_id=str(run["run_id"]),
            source_key="linear",
            observations=[revised_observation],
            items=[revised_item],
            expected_checkpoint_version=1,
        )
        self.assertEqual(
            await self.pool.fetchval(
                "select status from heartbeat_items where item_id = $1", item_uuid
            ),
            "open",
        )
        artifact = await first.put_artifact(
            run_id=str(run["run_id"]),
            artifact_kind="source_input",
            artifact_key="linear",
            content={"observations": [observation], "items": [item]},
        )
        replayed_artifact = await first.put_artifact(
            run_id=str(run["run_id"]),
            artifact_kind="source_input",
            artifact_key="linear",
            content={"observations": [observation], "items": [item]},
        )
        self.assertEqual(artifact["artifact_id"], replayed_artifact["artifact_id"])
        attempt = await first.put_artifact(
            run_id=str(run["run_id"]),
            artifact_kind="synthesis_attempt",
            artifact_key="attempt-1",
            content={"attempt": 1, "outcome": "retry"},
        )
        replayed_attempt = await first.put_artifact(
            run_id=str(run["run_id"]),
            artifact_kind="synthesis_attempt",
            artifact_key="attempt-1",
            content={"attempt": 1, "outcome": "retry"},
        )
        self.assertEqual(attempt["artifact_id"], replayed_attempt["artifact_id"])
        self.assertEqual(len(await first.list_artifacts(run_id=str(run["run_id"]))), 2)
        with self.assertRaisesRegex(RuntimeError, "differs from the original"):
            await first.put_artifact(
                run_id=str(run["run_id"]),
                artifact_kind="source_input",
                artifact_key="linear",
                content={"observations": [], "items": []},
            )
        with self.assertRaisesRegex(RuntimeError, "differs from the original"):
            await first.put_artifact(
                run_id=str(run["run_id"]),
                artifact_kind="synthesis_attempt",
                artifact_key="attempt-1",
                content={"attempt": 2, "outcome": "retry"},
            )

        candidates = await first.list_candidates(profile_id=str(profile["profile_id"]))
        self.assertEqual(len(candidates), 1)
        self.assertTrue(candidates[0]["changed_in_run"])
        evidence_id = str(candidates[0]["observations"][0]["observation_id"])
        synthesized = {
            "item_id": str(candidates[0]["item_id"]),
            "expected_version": int(candidates[0]["version"]),
            "headline": "Review Acme now",
            "summary": "The review remains open.",
            "why_now": "The deadline is near.",
            "recommendation": "Prepare a response for approval.",
            "recommended_disposition": "prepare_draft",
            "evidence_observation_ids": [evidence_id],
            "uncertainties": [],
        }
        synthesis_items = [synthesized]
        memory_proposals = [
            {
                "subject_key": "account:acme",
                "predicate": "review_owner",
                "value": {"team": "gtm"},
                "canonical_text": "GTM owns the Acme review.",
                "sensitivity": "internal",
                "evidence_observation_ids": [evidence_id],
            }
        ]
        with self.assertRaisesRegex(ValueError, "outside the item"):
            await first.commit_synthesis(
                profile_id=str(profile["profile_id"]),
                run_id=str(run["run_id"]),
                items=[
                    {
                        **synthesized,
                        "evidence_observation_ids": [str(uuid.uuid4())],
                    }
                ],
            )
        await first.commit_synthesis(
            profile_id=str(profile["profile_id"]),
            run_id=str(run["run_id"]),
            items=synthesis_items,
            memory_proposals=memory_proposals,
        )
        await first.commit_synthesis(
            profile_id=str(profile["profile_id"]),
            run_id=str(run["run_id"]),
            items=synthesis_items,
            memory_proposals=memory_proposals,
        )
        self.assertEqual(
            await self.pool.fetchval("select status from memory_facts limit 1"),
            "proposed",
        )
        self.assertEqual(
            await self.pool.fetchval("select count(*) from memory_facts"), 1
        )
        self.assertEqual(
            await self.pool.fetchval("select count(*) from memory_fact_events"), 1
        )
        proposed_fact = await first.list_memory_facts(
            actor_ref="U-REVIEWER", include_nonconfirmed=True
        )
        self.assertEqual(len(proposed_fact), 1)
        fact_id = str(proposed_fact[0]["fact_id"])
        self.assertEqual(len(proposed_fact[0]["evidence"]), 1)
        confirmed = await first.confirm_memory_fact(
            fact_id=fact_id,
            actor_ref="U-REVIEWER",
            expected_revision=1,
            reason="reviewed against the source observation",
        )
        self.assertEqual(confirmed["status"], "confirmed")
        self.assertEqual(confirmed["revision"], 2)
        workflow_memory = await first.retrieve_confirmed_memory(
            profile_id=str(profile["profile_id"]),
            entity_keys=["account:acme"],
            max_sensitivity="internal",
        )
        self.assertEqual(len(workflow_memory), 1)
        self.assertEqual(workflow_memory[0]["fact_id"], uuid.UUID(fact_id))
        self.assertEqual(
            len((await first.retrieve_memory_facts(actor_ref="U-REVIEWER"))), 1
        )
        corrected = await first.correct_memory_fact(
            fact_id=fact_id,
            actor_ref="U-REVIEWER",
            expected_revision=2,
            canonical_text="The GTM team owns the Acme review.",
            value={"team": "gtm", "confirmed": True},
            evidence=[
                {
                    "evidence_kind": "user_statement",
                    "evidence_ref": "reviewer:U-REVIEWER:1",
                    "excerpt": "GTM owns the Acme review.",
                }
            ],
            reason="corrected wording and retained the review provenance",
        )
        self.assertEqual(corrected["status"], "confirmed")
        self.assertEqual(corrected["revision"], 3)
        history = await first.retrieve_memory_fact(
            fact_id=str(corrected["fact_id"]),
            actor_ref="U-REVIEWER",
            include_history=True,
        )
        self.assertEqual(len(history["history"]), 2)
        disputed = await first.dispute_memory_fact(
            fact_id=str(corrected["fact_id"]),
            actor_ref="U-REVIEWER",
            expected_revision=3,
            reason="the review owner is not canonical yet",
            evidence=[
                {
                    "evidence_kind": "decision_record",
                    "evidence_ref": "decision:acme-owner:1",
                }
            ],
        )
        self.assertEqual(disputed["status"], "disputed")
        forgotten = await first.forget_memory_fact(
            fact_id=str(corrected["fact_id"]),
            actor_ref="U-REVIEWER",
            expected_revision=4,
            reason="reviewer requested erasure",
        )
        self.assertEqual(forgotten["status"], "forgotten")
        self.assertEqual(forgotten["canonical_text"], "[forgotten]")
        self.assertEqual(forgotten["evidence"], [])
        forgotten_ancestor = await first.retrieve_memory_fact(
            fact_id=fact_id, actor_ref="U-REVIEWER"
        )
        self.assertEqual(forgotten_ancestor["status"], "forgotten")
        self.assertEqual(forgotten_ancestor["canonical_text"], "[forgotten]")
        self.assertEqual(forgotten_ancestor["value"], {})
        self.assertEqual(forgotten_ancestor["evidence"], [])
        self.assertEqual(
            await first.list_memory_facts(actor_ref="U-REVIEWER"), []
        )
        with self.assertRaises(PermissionError):
            await first.retrieve_memory_fact(
                fact_id=str(corrected["fact_id"]), actor_ref="U-NOT-A-REVIEWER"
            )
        async def assume_other_role(connection: asyncpg.Connection) -> None:
            await connection.execute("set role centaur_heartbeat_run_other")
            await connection.execute(
                "set centaur.workflow_principal = 'workflow-heartbeat-other'"
            )

        other_pool = await asyncpg.create_pool(
            self.client_database_url.replace(
                "heartbeat_test_client", "heartbeat_other_client"
            ),
            min_size=1,
            max_size=1,
            setup=assume_other_role,
        )
        try:
            other = HeartbeatState(
                other_pool,
                workflow_name="heartbeat_run",
                workflow_run_id=str(uuid.uuid4()),
                workflow_task_id=str(uuid.uuid4()),
                workflow_principal="workflow-heartbeat-other",
            )
            self.assertEqual(
                await other.list_memory_facts(actor_ref="U-REVIEWER"), []
            )
            with self.assertRaises(PermissionError):
                await other.retrieve_memory_fact(
                    fact_id=str(corrected["fact_id"]), actor_ref="U-REVIEWER"
                )
            await other_pool.execute(
                "update memory_facts set canonical_text = 'forged' where fact_id = $1",
                corrected["fact_id"],
            )
            self.assertEqual(
                await other_pool.fetchval(
                    "select count(*) from memory_facts where fact_id = $1",
                    corrected["fact_id"],
                ),
                0,
            )
            self.assertEqual(
                (await first.retrieve_memory_fact(
                    fact_id=str(corrected["fact_id"]), actor_ref="U-REVIEWER"
                ))["canonical_text"],
                "[forgotten]",
            )
            with self.assertRaises(asyncpg.PostgresError):
                await self.pool.execute(
                    "update memory_facts set owner_principal = 'workflow-heartbeat-other' where fact_id = $1",
                    corrected["fact_id"],
                )
            value_hash = hashlib.sha256(
                b'{"team":"gtm"}'
            ).hexdigest()
            self.assertNotEqual(
                fact_id,
                str(
                    _uuid(
                        "memory-fact",
                        "workflow-heartbeat-other",
                        "default",
                        "team",
                        "gtm",
                        "account:acme",
                        "review_owner",
                        value_hash,
                    )
                ),
            )
        finally:
            other_pool.terminate()
        async with self.pool.acquire() as connection:
            async with connection.transaction():
                await connection.execute("set local role centaur_heartbeat_run")
                with self.assertRaises(asyncpg.InsufficientPrivilegeError):
                    await connection.execute(
                        f"set local role {second_role}"
                    )

        async with self.pool.acquire() as connection:
            async with connection.transaction():
                await connection.execute("reset centaur.workflow_principal")
                self.assertEqual(
                    await connection.fetchval("select count(*) from heartbeat_profiles"),
                    0,
                )

                await connection.execute(
                    "set local centaur.workflow_principal = 'forged-principal'"
                )
                self.assertEqual(
                    await connection.fetchval("select count(*) from heartbeat_profiles"),
                    0,
                )
                await connection.execute(
                    "set local centaur.workflow_principal = 'workflow-heartbeat-run'"
                )
                self.assertEqual(
                    await connection.fetchval(
                        "select count(*) from heartbeat_profiles where profile_id = $1",
                        profile["profile_id"],
                    ),
                    1,
                )
                self.assertEqual(
                    await connection.fetchval("select count(*) from heartbeat_profiles"),
                    2,
                )
                self.assertEqual(
                    await connection.fetchval(
                        "select count(*) from heartbeat_profiles where profile_id = $1",
                        second_profile_id,
                    ),
                    0,
                )
                await connection.execute(
                    "set local centaur.workflow_principal = 'workflow-heartbeat-other'"
                )
                self.assertEqual(
                    await connection.fetchval("select count(*) from heartbeat_profiles"),
                    0,
                )

        delivery = await first.prepare_delivery(
            run_id=str(run["run_id"]),
            destination_kind="slack",
            destination_ref="C123",
            rendered_payload={"text": "Review Acme now"},
            item_actions=[
                {
                    "item_id": synthesized["item_id"],
                    "item_version": synthesized["expected_version"],
                    "action": "approve",
                    "payload": {},
                }
            ],
        )
        raw_token = delivery["tokens"][0]["token"]
        self.assertNotEqual(
            raw_token,
            await self.pool.fetchval(
                "select token_hash from heartbeat_action_tokens limit 1"
            ),
        )
        replayed_delivery = await first.prepare_delivery(
            run_id=str(run["run_id"]),
            destination_kind="slack",
            destination_ref="C123",
            rendered_payload={"text": "Review Acme now"},
            item_actions=[
                {
                    "item_id": synthesized["item_id"],
                    "item_version": synthesized["expected_version"],
                    "action": "approve",
                    "payload": {},
                }
            ],
        )
        self.assertEqual(replayed_delivery["delivery_id"], delivery["delivery_id"])
        self.assertEqual(replayed_delivery["tokens"], delivery["tokens"])
        self.assertTrue(replayed_delivery["replayed"])
        self.assertEqual(
            await self.pool.fetchval(
                "select count(*) from heartbeat_action_tokens where delivery_id = $1",
                uuid.UUID(delivery["delivery_id"]),
            ),
            1,
        )
        await first.mark_delivery_sent(
            delivery_id=delivery["delivery_id"],
            provider_message_id="1710000000.000100",
            surfaced_item_ids=[synthesized["item_id"]],
        )

        async def assume_feedback_role(connection: asyncpg.Connection) -> None:
            await connection.execute("set role centaur_heartbeat_feedback")

        feedback_pool = await asyncpg.create_pool(
            self.client_database_url.replace(
                "heartbeat_test_client", "heartbeat_feedback_client"
            ),
            min_size=1,
            max_size=1,
            setup=assume_feedback_role,
        )
        try:
            self.assertEqual(
                await feedback_pool.fetchval("select count(*) from heartbeat_items"),
                0,
            )
            feedback = HeartbeatState(
                feedback_pool,
                workflow_name="heartbeat_feedback",
                workflow_run_id=str(uuid.uuid4()),
                workflow_task_id=str(uuid.uuid4()),
                workflow_principal="workflow-heartbeat-feedback",
            )
            action = await feedback.apply_action(
                token=raw_token,
                actor_ref="U-REVIEWER",
                provider_event_key="slack-action-1",
            )
            self.assertEqual(action["status"], "resolved")
            replayed_action = await feedback.apply_action(
                token=raw_token,
                actor_ref="U-REVIEWER",
                provider_event_key="slack-action-1",
            )
            self.assertEqual(replayed_action, action)
        finally:
            feedback_pool.terminate()
        await first.complete_run(
            run_id=str(run["run_id"]), status="completed", outcome="attention"
        )

        second_run_id = uuid.uuid4()
        second = self.state(run_id=second_run_id)
        second_profile = await second.register_profile(definition)
        second_run = await second.begin_run(
            profile_id=str(second_profile["profile_id"]),
            trigger="replay",
            definition_hash="definition-v1",
            prompt_version="test-v1",
        )
        replay = await second.commit_source_batch(
            profile_id=str(second_profile["profile_id"]),
            run_id=str(second_run["run_id"]),
            source_key="linear",
            observations=[revised_observation],
            items=[revised_item],
            expected_checkpoint_version=2,
        )
        self.assertEqual(replay["inserted_observations"], 0)
        self.assertEqual(replay["changed_items"], 0)
        self.assertTrue(await second.fail_current_run(RuntimeError("test failure")))
        self.assertEqual(
            await self.pool.fetchval(
                "select status from heartbeat_runs where run_id = $1", second_run_id
            ),
            "failed",
        )

        unauthorized = self.state(
            run_id=uuid.uuid4(),
            workflow_name="other_workflow",
            principal="workflow-other",
        )
        with self.assertRaises(PermissionError):
            await unauthorized.list_candidates(profile_id=str(profile["profile_id"]))

    async def test_z_memory_delivery_facade_is_run_bound_and_correct_is_request_only(self) -> None:
        state = self.state(run_id=uuid.uuid4())
        profile = await state.register_profile(
            {
                "namespace": "default",
                "name": "memory-facade-profile",
                "scope_kind": "team",
                "scope_ref": "memory-facade",
                "definition_hash": "memory-v1",
                "definition_version": 1,
                "destination": {"kind": "slack", "ref": "C-memory"},
                "required_sources": [],
                "optional_sources": [],
                "delivery_policy": {"posture": "read_and_draft_only"},
                "reviewer_refs": ["U-REVIEWER"],
                "enabled": True,
            }
        )
        other_profile = await state.register_profile(
            {
                "namespace": "default",
                "name": "memory-facade-other-profile",
                "scope_kind": "team",
                "scope_ref": "memory-facade",
                "definition_hash": "memory-v1",
                "definition_version": 1,
                "destination": {"kind": "slack", "ref": "C-memory-other"},
                "required_sources": [],
                "optional_sources": [],
                "delivery_policy": {"posture": "read_and_draft_only"},
                "reviewer_refs": ["U-OTHER"],
                "enabled": True,
            }
        )
        run = await state.begin_run(
            profile_id=str(profile["profile_id"]),
            trigger="replay",
            definition_hash="memory-v1",
            prompt_version="test-v1",
        )
        fact_id = uuid.uuid4()
        async with self.pool.acquire() as connection:
            await connection.execute(
                """
                insert into memory_facts(
                    fact_id, owner_principal, namespace, scope_kind, scope_ref,
                    subject_key, predicate, value, canonical_text, status, sensitivity,
                    proposed_by_principal
                ) values ($1, 'workflow-heartbeat-run', 'default', 'team', 'memory-facade',
                    'account:memory', 'owner', '{"team":"gtm"}', 'GTM owns the review',
                    'proposed', 'internal', 'workflow-heartbeat-run')
                """,
                fact_id,
            )
            await connection.execute(
                "insert into heartbeat_run_memory_facts(run_id, fact_id) values($1,$2)",
                run["run_id"], fact_id,
            )
        delivery = await state.prepare_delivery(
            run_id=str(run["run_id"]),
            destination_kind="slack",
            destination_ref="C-memory",
            rendered_payload={"text": "Review memory"},
            memory_actions=[
                {
                    "memory_fact_id": str(fact_id),
                    "expected_revision": 1,
                    "action": "correct",
                    "payload": {},
                }
            ],
        )
        replay = await state.prepare_delivery(
            run_id=str(run["run_id"]),
            destination_kind="slack",
            destination_ref="C-memory",
            rendered_payload={"text": "Review memory"},
            memory_actions=[
                {
                    "memory_fact_id": str(fact_id),
                    "expected_revision": 1,
                    "action": "correct",
                    "payload": {},
                }
            ],
        )
        self.assertEqual(replay["tokens"], delivery["tokens"])
        await state.mark_delivery_sent(
            delivery_id=delivery["delivery_id"],
            provider_message_id="memory-only",
            surfaced_item_ids=[],
        )

        async def assume_feedback(connection: asyncpg.Connection) -> None:
            await connection.execute("set role centaur_heartbeat_feedback")
            await connection.execute(
                "set centaur.workflow_principal = 'workflow-heartbeat-feedback'"
            )

        feedback_pool = await asyncpg.create_pool(
            self.client_database_url.replace(
                "heartbeat_test_client", "heartbeat_feedback_client"
            ),
            min_size=1,
            max_size=1,
            setup=assume_feedback,
        )
        try:
            feedback = HeartbeatState(
                feedback_pool,
                workflow_name="heartbeat_feedback",
                workflow_run_id=str(uuid.uuid4()),
                workflow_task_id=str(uuid.uuid4()),
                workflow_principal="workflow-heartbeat-feedback",
            )
            with self.assertRaises(PermissionError):
                await feedback.request_memory_correction(
                    token=delivery["tokens"][0]["token"],
                    fact_id=str(fact_id),
                    actor_ref="U-OTHER",
                    provider_event_key="memory-correction-other-profile",
                )
            result = await feedback.request_memory_correction(
                token=delivery["tokens"][0]["token"],
                fact_id=str(fact_id),
                actor_ref="U-REVIEWER",
                provider_event_key="memory-correction-1",
            )
            self.assertEqual(result["status"], "correction_requested")
            self.assertEqual(result["fact_status"], "proposed")
            fact = await self.pool.fetchrow(
                "select status, revision, canonical_text from memory_facts where fact_id=$1",
                fact_id,
            )
            self.assertEqual(tuple(fact), ("proposed", 1, "GTM owns the review"))
            admin_connection = await asyncpg.connect(DATABASE_URL)
            try:
                self.assertEqual(
                    await admin_connection.fetchval(
                        "select count(*) from memory_correction_requests where fact_id=$1",
                        fact_id,
                    ),
                    1,
                )
            finally:
                await admin_connection.close()
            victim_id = uuid.uuid4()
            foreign_id = uuid.uuid4()
            foreign_evidence_id = uuid.uuid4()
            await self.pool.execute(
                """
                insert into memory_facts(
                    fact_id, owner_principal, namespace, scope_kind, scope_ref,
                    subject_key, predicate, value, canonical_text, status, sensitivity,
                    proposed_by_principal
                ) values ($1, 'workflow-heartbeat-run', 'default', 'team', 'memory-facade',
                    'account:victim', 'owner', '{"team":"gtm"}', 'Victim fact',
                    'proposed', 'internal', 'workflow-heartbeat-run')
                """,
                victim_id,
            )
            await self.pool.execute(
                "insert into heartbeat_run_memory_facts(run_id, fact_id) values($1,$2)",
                run["run_id"],
                victim_id,
            )
            admin_connection = await asyncpg.connect(DATABASE_URL)
            try:
                await admin_connection.execute(
                    """
                    insert into memory_facts(
                        fact_id, owner_principal, namespace, scope_kind, scope_ref,
                        subject_key, predicate, value, canonical_text, status, sensitivity,
                        proposed_by_principal
                    ) values ($1, 'workflow-heartbeat-other', 'default', 'team', 'other-scope',
                        'account:foreign', 'owner', '{"team":"foreign"}', 'Foreign fact',
                        'proposed', 'internal', 'workflow-heartbeat-other')
                    """,
                    foreign_id,
                )
                await admin_connection.execute(
                    "insert into memory_fact_evidence(evidence_id,fact_id,evidence_kind,evidence_ref) values($1,$2,'user_statement','foreign-proof')",
                    foreign_evidence_id,
                    foreign_id,
                )
                await admin_connection.execute(
                    "update memory_facts set supersedes_fact_id=$2 where fact_id=$1",
                    victim_id,
                    foreign_id,
                )
            finally:
                await admin_connection.close()
            forget_delivery = await state.prepare_delivery(
                run_id=str(run["run_id"]),
                destination_kind="slack",
                destination_ref="C-memory-forget",
                rendered_payload={"text": "Forget memory"},
                memory_actions=[
                    {
                        "memory_fact_id": str(victim_id),
                        "expected_revision": 1,
                        "action": "forget",
                        "payload": {},
                    }
                ],
            )
            forgotten = await feedback.apply_memory_action(
                token=forget_delivery["tokens"][0]["token"],
                actor_ref="U-REVIEWER",
                provider_event_key="memory-forget-lineage",
            )
            self.assertEqual(forgotten["status"], "forgotten")
            admin_connection = await asyncpg.connect(DATABASE_URL)
            try:
                foreign = await admin_connection.fetchrow(
                    "select status, canonical_text, value, (select count(*) from memory_fact_evidence where fact_id=$1) from memory_facts where fact_id=$1",
                    foreign_id,
                )
                self.assertEqual(foreign["status"], "proposed")
                self.assertEqual(foreign["canonical_text"], "Foreign fact")
                self.assertEqual(json.loads(foreign["value"]), {"team": "foreign"})
                self.assertEqual(foreign["count"], 1)
            finally:
                await admin_connection.close()
        finally:
            feedback_pool.terminate()
        feedback_pool = await asyncpg.create_pool(
            self.client_database_url.replace(
                "heartbeat_test_client", "heartbeat_feedback_client"
            ),
            min_size=1,
            max_size=1,
            setup=lambda connection: connection.execute(
                "set role centaur_heartbeat_feedback"
            ),
        )
        feedback = HeartbeatState(
            feedback_pool,
            workflow_name="heartbeat_feedback",
            workflow_run_id=str(uuid.uuid4()),
            workflow_task_id=str(uuid.uuid4()),
            workflow_principal="workflow-heartbeat-feedback",
        )
        item_id = uuid.uuid4()
        async with self.pool.acquire() as connection:
            await connection.execute(
                """
                insert into heartbeat_items(item_id, profile_id, story_key, item_type, title, material_hash)
                values($1, $2, 'memory-draft-item', 'work', 'Draft item', 'draft-hash')
                """,
                item_id,
                profile["profile_id"],
            )
        item_delivery = await state.prepare_delivery(
            run_id=str(run["run_id"]),
            destination_kind="slack",
            destination_ref="C-draft",
            rendered_payload={"text": "Prepare draft"},
            item_actions=[
                {
                    "item_id": str(item_id),
                    "item_version": 1,
                    "action": "prepare_draft",
                    "payload": {},
                }
            ],
        )
        assignment_item_id = uuid.uuid4()
        async with self.pool.acquire() as connection:
            await connection.execute(
                """
                insert into heartbeat_items(item_id, profile_id, story_key, item_type, title, material_hash)
                values($1, $2, 'assignment-item', 'work', 'Assignment item', 'assignment-hash')
                """,
                assignment_item_id,
                profile["profile_id"],
            )
        assignment_delivery = await state.prepare_delivery(
            run_id=str(run["run_id"]),
            destination_kind="slack",
            destination_ref="C-assignment",
            rendered_payload={"text": "Assign"},
            item_actions=[
                {
                    "item_id": str(assignment_item_id),
                    "item_version": 1,
                    "action": "assign",
                    "payload": {},
                }
            ],
        )
        assignment_result = await feedback.apply_action(
            token=assignment_delivery["tokens"][0]["token"],
            actor_ref="U-REVIEWER",
            provider_event_key="assignment-1",
        )
        self.assertEqual(assignment_result["status"], "open")
        self.assertNotIn("draft_grant", assignment_result)
        draft_result = await feedback.apply_action(
            token=item_delivery["tokens"][0]["token"],
            actor_ref="U-REVIEWER",
            provider_event_key="draft-prepare-1",
        )
        replayed_draft = await feedback.apply_action(
            token=item_delivery["tokens"][0]["token"],
            actor_ref="U-REVIEWER",
            provider_event_key="draft-prepare-1",
        )
        self.assertEqual(replayed_draft["draft_grant"], draft_result["draft_grant"])
        with self.assertRaises(PermissionError):
            await feedback.apply_action(
                token=item_delivery["tokens"][0]["token"],
                actor_ref="U-REVIEWER",
                provider_event_key="draft-prepare-other-event",
            )
        admin_connection = await asyncpg.connect(DATABASE_URL)
        try:
            persisted_action = await admin_connection.fetchval(
                "select result::text from heartbeat_action_tokens where token_hash=$1",
                hashlib.sha256(item_delivery["tokens"][0]["token"].encode()).hexdigest(),
            )
            self.assertNotIn("draft_grant", persisted_action)
            self.assertNotIn(draft_result["draft_grant"], persisted_action)
            persisted_grant = await admin_connection.fetchval(
                "select grant_hash from heartbeat_draft_grants where item_id=$1",
                item_id,
            )
            self.assertEqual(
                persisted_grant,
                hashlib.sha256(draft_result["draft_grant"].encode()).hexdigest(),
            )
        finally:
            await admin_connection.close()
        admin_connection = await asyncpg.connect(DATABASE_URL)
        try:
            await admin_connection.execute(
                "do $$ begin if not exists (select 1 from pg_roles where rolname = 'heartbeat_prepare_client') then create role heartbeat_prepare_client login password 'heartbeat-test'; end if; end $$"
            )
            await admin_connection.execute(
                "grant centaur_heartbeat_prepare_action to heartbeat_prepare_client"
            )
        finally:
            await admin_connection.close()
        prepare_pool = await asyncpg.create_pool(
            self.client_database_url.replace(
                "heartbeat_test_client", "heartbeat_prepare_client"
            ),
            min_size=1,
            max_size=1,
            setup=lambda connection: connection.execute(
                "set role centaur_heartbeat_prepare_action"
            ),
        )
        try:
            with self.assertRaises(asyncpg.InsufficientPrivilegeError):
                await prepare_pool.fetchval("select count(*) from heartbeat_items")
            with self.assertRaises(asyncpg.InsufficientPrivilegeError):
                await prepare_pool.fetchval("select count(*) from memory_facts")
            with self.assertRaises(asyncpg.InsufficientPrivilegeError):
                await prepare_pool.fetchval("select count(*) from heartbeat_draft_grants")
            prepare = HeartbeatState(
                prepare_pool,
                workflow_name="heartbeat_prepare_action",
                workflow_run_id=str(uuid.uuid4()),
                workflow_task_id=str(uuid.uuid4()),
                workflow_principal="workflow-heartbeat-prepare-action",
            )
            grant = draft_result["draft_grant"]
            draft_item = await prepare.get_item(
                draft_grant=grant, item_id=str(item_id), expected_version=2
            )
            self.assertEqual(str(draft_item["item_id"]), str(item_id))
            self.assertEqual(draft_item["version"], 2)
            artifact = await prepare.put_draft_artifact(
                draft_grant=grant,
                item_id=str(item_id),
                item_version=2,
                draft={"writes": [], "notes": ["review required"]},
            )
            self.assertEqual(str(artifact["item_id"]), str(item_id))
            with self.assertRaises(PermissionError):
                await prepare.put_draft_artifact(
                    draft_grant=grant,
                    item_id=str(item_id),
                    item_version=2,
                    draft={"writes": []},
                )
        finally:
            prepare_pool.terminate()
            feedback_pool.terminate()

    async def test_zz_memory_projection_is_bounded_and_lifecycle_reconciled(self) -> None:
        run_id = uuid.uuid4()
        state = self.state(run_id=run_id)
        await state.register_profile(
            {
                "namespace": "projection-test",
                "name": "organization",
                "scope_kind": "organization",
                "scope_ref": "org-1",
                "definition_hash": "projection-v1",
                "definition_version": 1,
                "reviewer_refs": ["U-REVIEWER"],
                "enabled": True,
            }
        )
        fact_id = uuid.uuid4()
        await self.pool.execute(
            """
            insert into memory_facts(
                fact_id, namespace, scope_kind, scope_ref, subject_key,
                predicate, value, canonical_text, status, sensitivity,
                revision, proposed_by_principal, owner_principal
            ) values($1, 'projection-test', 'organization', 'org-1',
                     'account:acme', 'owner', '{\"team\":\"gtm\"}',
                     'GTM owns the account.', 'proposed', 'internal', 1,
                     'workflow-heartbeat-run', 'workflow-heartbeat-run')
            """,
            fact_id,
        )
        confirmed = await state.confirm_memory_fact(
            fact_id=str(fact_id), actor_ref="U-REVIEWER", expected_revision=1
        )
        self.assertEqual(confirmed["status"], "confirmed")
        long_fact_id = uuid.uuid4()
        long_metadata_fact_id = uuid.uuid4()
        await self.pool.execute(
            """
            insert into memory_facts(
                fact_id, namespace, scope_kind, scope_ref, subject_key,
                predicate, value, canonical_text, status, sensitivity,
                revision, proposed_by_principal, owner_principal
            ) values
                ($1, 'projection-test', 'organization', 'org-1',
                 'account:long', 'owner', '{\"team\":\"gtm\"}', $2,
                 'proposed', 'internal', 1,
                 'workflow-heartbeat-run', 'workflow-heartbeat-run'),
                ($3, 'projection-test', 'organization', 'org-1', $4,
                 'owner', '{\"team\":\"gtm\"}', 'bounded metadata',
                 'proposed', 'internal', 1,
                 'workflow-heartbeat-run', 'workflow-heartbeat-run')
            """,
            long_fact_id,
            "x" * 1001,
            long_metadata_fact_id,
            "s" * 257,
        )
        await state.confirm_memory_fact(
            fact_id=str(long_fact_id), actor_ref="U-REVIEWER", expected_revision=1
        )
        await state.confirm_memory_fact(
            fact_id=str(long_metadata_fact_id),
            actor_ref="U-REVIEWER",
            expected_revision=1,
        )
        admin = await asyncpg.connect(DATABASE_URL)
        try:
            document = await admin.fetchrow(
                """
                select document_id, body, access_scope, metadata, content_hash
                  from company_context_documents
                 where source_document_id = $1
                """,
                str(fact_id),
            )
            self.assertIsNotNone(document)
            assert document is not None
            metadata = (
                json.loads(document["metadata"])
                if isinstance(document["metadata"], str)
                else document["metadata"]
            )
            self.assertEqual(document["document_id"], f"memory_fact:{fact_id}:2")
            self.assertEqual(document["body"], "GTM owns the account.")
            self.assertEqual(document["access_scope"], "organization:projection-test:org-1")
            self.assertEqual(metadata["scope_ref"], "org-1")
            self.assertEqual(metadata["subject_key"], "account:acme")
            self.assertEqual(len(document["content_hash"]), 64)
            self.assertIsNone(
                await admin.fetchrow(
                    "select 1 from company_context_documents where source_document_id = $1",
                    str(long_fact_id),
                )
            )
            self.assertIsNone(
                await admin.fetchrow(
                    "select 1 from company_context_documents where source_document_id = $1",
                    str(long_metadata_fact_id),
                )
            )

            readonly_pool = await asyncpg.create_pool(
                self.client_database_url.replace(
                    "heartbeat_test_client", "heartbeat_readonly_client"
                ),
                min_size=1,
                max_size=1,
                setup=lambda connection: connection.execute(
                    "set role centaur_readonly"
                ),
            )
            try:
                self.assertEqual(
                    await readonly_pool.fetchval(
                        "select count(*) from company_context_documents where source = 'heartbeat_memory'"
                    ),
                    0,
                )
            finally:
                readonly_pool.terminate()

            await self.pool.execute(
                "update memory_facts set status = 'disputed', revision = revision + 1 where fact_id = $1",
                fact_id,
            )
            self.assertIsNone(
                await admin.fetchrow(
                    "select 1 from company_context_documents where source_document_id = $1",
                    str(fact_id),
                )
            )
            with self.assertRaises(ValueError):
                await state._insert_memory_evidence(
                    self.pool,
                    fact_id=fact_id,
                    evidence=[{"evidence_ref": f"memory_fact:{fact_id}"}],
                )
        finally:
            await admin.close()

    async def test_zz_memory_event_idempotency_is_namespaced_by_fact(self) -> None:
        state = self.state(run_id=uuid.uuid4())
        profile = await state.register_profile(
            {
                "namespace": "event-key-test",
                "name": "team",
                "scope_kind": "team",
                "scope_ref": "team-1",
                "definition_hash": "event-key-v1",
                "definition_version": 1,
                "reviewer_refs": ["U-REVIEWER"],
                "enabled": True,
            }
        )
        fact_one = uuid.uuid4()
        fact_two = uuid.uuid4()
        await self.pool.execute(
            """
            insert into memory_facts(
                fact_id, namespace, scope_kind, scope_ref, subject_key,
                predicate, value, canonical_text, status, sensitivity,
                revision, proposed_by_principal, owner_principal
            ) values
                ($1, 'event-key-test', 'team', 'team-1', 'account:one',
                 'owner', '{\"team\":\"gtm\"}', 'First owner',
                 'proposed', 'internal', 1, 'workflow-heartbeat-run',
                 'workflow-heartbeat-run'),
                ($2, 'event-key-test', 'team', 'team-1', 'account:two',
                 'owner', '{\"team\":\"sales\"}', 'Second owner',
                 'proposed', 'internal', 1, 'workflow-heartbeat-run',
                 'workflow-heartbeat-run')
            """,
            fact_one,
            fact_two,
        )
        await state.confirm_memory_fact(
            fact_id=str(fact_one),
            actor_ref="U-REVIEWER",
            expected_revision=1,
            idempotency_key="same-transition-key",
        )
        await state.confirm_memory_fact(
            fact_id=str(fact_two),
            actor_ref="U-REVIEWER",
            expected_revision=1,
            idempotency_key="same-transition-key",
        )
        self.assertEqual(
            await self.pool.fetchval(
                "select count(*) from memory_fact_events where event_type = 'confirmed' and fact_id = any($1::uuid[])",
                [fact_one, fact_two],
            ),
            2,
        )
        evidence_one = uuid.uuid4()
        evidence_two = uuid.uuid4()
        await self.pool.execute(
            """
            insert into memory_fact_evidence(
                evidence_id, fact_id, evidence_kind, evidence_ref
            ) values
                ($1, $2, 'user_statement', 'event-key:one'),
                ($3, $4, 'user_statement', 'event-key:two')
            """,
            evidence_one,
            fact_one,
            evidence_two,
            fact_two,
        )
        corrected_one = await state.correct_memory_fact(
            fact_id=str(fact_one),
            actor_ref="U-REVIEWER",
            expected_revision=2,
            canonical_text="First owner corrected",
            value={"team": "gtm", "corrected": True},
            evidence=[
                {
                    "evidence_kind": "user_statement",
                    "evidence_ref": "event-key:one-correction",
                }
            ],
            idempotency_key="same-correction-key",
        )
        corrected_two = await state.correct_memory_fact(
            fact_id=str(fact_two),
            actor_ref="U-REVIEWER",
            expected_revision=2,
            canonical_text="Second owner corrected",
            value={"team": "sales", "corrected": True},
            evidence=[
                {
                    "evidence_kind": "user_statement",
                    "evidence_ref": "event-key:two-correction",
                }
            ],
            idempotency_key="same-correction-key",
        )
        self.assertNotEqual(corrected_one["fact_id"], corrected_two["fact_id"])
        with self.assertRaises(PermissionError):
            await state.correct_memory_fact(
                fact_id=str(fact_one),
                actor_ref="U-NOT-A-REVIEWER",
                expected_revision=2,
                canonical_text="unauthorized replay",
                value={"team": "gtm"},
                evidence=[
                    {
                        "evidence_kind": "user_statement",
                        "evidence_ref": "event-key:unauthorized",
                    }
                ],
                idempotency_key="same-correction-key",
            )
        with self.assertRaisesRegex(ValueError, "canonical_text exceeds"):
            await state.correct_memory_fact(
                fact_id=str(fact_two),
                actor_ref="U-REVIEWER",
                expected_revision=2,
                canonical_text="x" * 1001,
                value={"team": "sales"},
                evidence=[
                    {
                        "evidence_kind": "user_statement",
                        "evidence_ref": "event-key:oversized",
                    }
                ],
                idempotency_key="oversized-correction-key",
            )

        admin = await asyncpg.connect(DATABASE_URL)
        try:
            await admin.execute(
                "delete from memory_facts where fact_id = any($1::uuid[])",
                [
                    fact_one,
                    fact_two,
                    corrected_one["fact_id"],
                    corrected_two["fact_id"],
                ],
            )
            await admin.execute(
                "delete from heartbeat_profiles where profile_id = $1",
                profile["profile_id"],
            )
            self.assertEqual(
                await admin.fetchval(
                    "select count(*) from memory_facts where fact_id = any($1::uuid[])",
                    [fact_one, fact_two, corrected_one["fact_id"], corrected_two["fact_id"]],
                ),
                0,
            )
        finally:
            await admin.close()

    async def test_zz_retention_policy_is_strict_and_returns_fixed_counts(self) -> None:
        state = self.state(run_id=uuid.uuid4())
        base = {
            "namespace": "retention-test",
            "scope_kind": "team",
            "scope_ref": "team-1",
            "definition_hash": "retention-v1",
            "definition_version": 1,
            "enabled": True,
        }
        for invalid in (
            {"observation_days": 1, "run_snapshot_days": 1, "delivery_days": 1, "extra": 1},
            {"observation_days": "1", "run_snapshot_days": 1, "delivery_days": 1},
            {"observation_days": 0, "run_snapshot_days": 1, "delivery_days": 1},
        ):
            with self.assertRaises(ValueError):
                await state.register_profile({**base, "name": str(uuid.uuid4()), "retention_policy": invalid})
        profile = await state.register_profile(
            {
                **base,
                "name": "valid",
                "retention_policy": {
                    "observation_days": 1,
                    "run_snapshot_days": 1,
                    "delivery_days": 1,
                },
            }
        )
        expired_proposed_id = uuid.uuid4()
        expired_confirmed_id = uuid.uuid4()
        await self.pool.execute(
            """
            insert into memory_facts(
                fact_id, namespace, scope_kind, scope_ref, subject_key,
                predicate, value, canonical_text, status, sensitivity,
                valid_until, revision, proposed_by_principal, owner_principal
            ) values
                ($1, 'retention-test', 'team', 'team-1', 'account:old-proposed',
                 'owner', '{\"team\":\"gtm\"}', 'Old proposed fact',
                 'proposed', 'internal', now() - interval '1 day', 1,
                 'workflow-heartbeat-run', 'workflow-heartbeat-run'),
                ($2, 'retention-test', 'team', 'team-1', 'account:old-confirmed',
                 'owner', '{\"team\":\"gtm\"}', 'Old confirmed fact',
                 'confirmed', 'internal', now() - interval '1 day', 1,
                 'workflow-heartbeat-run', 'workflow-heartbeat-run')
            """,
            expired_proposed_id,
            expired_confirmed_id,
        )
        result = await state.apply_retention(str(profile["profile_id"]))
        self.assertEqual(
            set(result),
            {
                "observations_scrubbed",
                "artifacts_scrubbed",
                "run_snapshots_scrubbed",
                "deliveries_scrubbed",
                "action_tokens_deleted",
                "memory_tokens_deleted",
                "draft_artifacts_deleted",
                "draft_grants_deleted",
                "facts_expired",
            },
        )
        self.assertEqual(result["facts_expired"], 2)
        self.assertEqual(
            await self.pool.fetchval(
                """
                select count(*) from memory_facts
                 where fact_id = any($1::uuid[]) and status = 'expired'
                """,
                [expired_proposed_id, expired_confirmed_id],
            ),
            2,
        )
        replay = await state.apply_retention(str(profile["profile_id"]))
        self.assertEqual(replay["facts_expired"], 0)

    async def test_zz_memory_proposal_cannot_downgrade_observation_sensitivity(self) -> None:
        run_id = uuid.uuid4()
        state = self.state(run_id=run_id)
        profile = await state.register_profile(
            {
                "namespace": "sensitivity-test",
                "name": "team",
                "scope_kind": "team",
                "scope_ref": "team-1",
                "definition_hash": "sensitivity-v1",
                "definition_version": 1,
                "enabled": True,
            }
        )
        await state.begin_run(
            profile_id=str(profile["profile_id"]),
            trigger="replay",
            definition_hash="sensitivity-v1",
            prompt_version="test",
        )
        observation = {
            "source_object_id": "item-1",
            "source_revision": "1",
            "content_hash": hashlib.sha256(b"internal").hexdigest(),
            "title": "Internal item",
            "sensitivity": "internal",
            "payload": {"status": "open"},
        }
        item = {
            "story_key": "linear:item-1",
            "material_hash": observation["content_hash"],
            "title": "Internal item",
            "item_type": "work",
            "observation_refs": [
                {"source_object_id": "item-1", "source_revision": "1"}
            ],
        }
        await state.commit_source_batch(
            profile_id=str(profile["profile_id"]),
            run_id=str(run_id),
            source_key="linear",
            observations=[observation],
            items=[item],
        )
        candidate = (await state.list_candidates(profile_id=str(profile["profile_id"])))[0]
        with self.assertRaisesRegex(ValueError, "below its evidence"):
            await state.commit_synthesis(
                profile_id=str(profile["profile_id"]),
                run_id=str(run_id),
                items=[],
                memory_proposals=[
                    {
                        "subject_key": "account:acme",
                        "predicate": "owner",
                        "value": {"team": "gtm"},
                        "canonical_text": "The account owner is GTM.",
                        "sensitivity": "public",
                        "evidence_observation_ids":[
                            str(candidate["observations"][0]["observation_id"])
                        ],
                    }
                ],
            )
        evidence_id = str(candidate["observations"][0]["observation_id"])
        proposal = {
            "subject_key": "account:acme",
            "predicate": "owner",
            "value": {"team": "gtm"},
            "canonical_text": "The account owner is GTM.",
            "sensitivity": "internal",
            "evidence_observation_ids": [evidence_id],
        }
        for invalid_proposal, expected_error in (
            (
                {**proposal, "subject_key": "s" * 257},
                "subject_key exceeds",
            ),
            ({**proposal, "predicate": "p" * 257}, "predicate exceeds"),
            ({**proposal, "canonical_text": "c" * 1001}, "canonical_text exceeds"),
            ({**proposal, "value": {"payload": "v" * 2100}}, "value exceeds"),
            (
                {**proposal, "evidence_observation_ids": [evidence_id] * 11},
                "at most 10 evidence",
            ),
        ):
            with self.assertRaisesRegex(ValueError, expected_error):
                await state.commit_synthesis(
                    profile_id=str(profile["profile_id"]),
                    run_id=str(run_id),
                    items=[],
                    memory_proposals=[invalid_proposal],
                )

    async def test_zz_memory_promotion_is_authorized_widening_and_idempotent(self) -> None:
        run_state = self.state(run_id=uuid.uuid4())
        source = await run_state.register_profile(
            {
                "namespace": "promotion-test",
                "name": "personal",
                "scope_kind": "personal",
                "scope_ref": "U-OWNER",
                "definition_hash": "promotion-v1",
                "definition_version": 1,
                "reviewer_refs": ["U-ADMIN"],
                "enabled": True,
            }
        )
        target = await run_state.register_profile(
            {
                "namespace": "promotion-test",
                "name": "organization",
                "scope_kind": "organization",
                "scope_ref": "org-1",
                "definition_hash": "promotion-v1",
                "definition_version": 1,
                "enabled": True,
            }
        )
        target_two = await run_state.register_profile(
            {
                "namespace": "promotion-test",
                "name": "organization-two",
                "scope_kind": "organization",
                "scope_ref": "org-2",
                "definition_hash": "promotion-v1",
                "definition_version": 1,
                "enabled": True,
            }
        )
        same_scope = await run_state.register_profile(
            {
                "namespace": "promotion-test",
                "name": "same-scope",
                "scope_kind": "personal",
                "scope_ref": "U-OTHER",
                "definition_hash": "promotion-v1",
                "definition_version": 1,
                "enabled": True,
            }
        )
        cross_namespace = await run_state.register_profile(
            {
                "namespace": "other-namespace",
                "name": "organization",
                "scope_kind": "organization",
                "scope_ref": "org-2",
                "definition_hash": "promotion-v1",
                "definition_version": 1,
                "enabled": True,
            }
        )
        fact_id = uuid.uuid4()
        await self.pool.execute(
            """
            insert into memory_facts(
                fact_id, namespace, scope_kind, scope_ref, subject_key,
                predicate, value, canonical_text, status, sensitivity,
                revision, proposed_by_principal, confirmed_by_principal,
                owner_principal
            ) values($1, 'promotion-test', 'personal', 'U-OWNER',
                     'account:acme', 'owner', '{\"team\":\"gtm\"}',
                     'GTM owns the account.', 'confirmed', 'internal', 2,
                     'workflow-heartbeat-run', 'U-ADMIN', 'workflow-heartbeat-run')
            """,
            fact_id,
        )
        await self.pool.execute(
            """
            insert into memory_fact_evidence(
                evidence_id, fact_id, evidence_kind, evidence_ref,
                source_url, excerpt, content_hash
            ) values($1, $2, 'user_statement', 'statement:acme',
                     'https://example.test/acme', 'bounded', 'hash')
            """,
            uuid.uuid4(),
            fact_id,
        )
        admin = await asyncpg.connect(DATABASE_URL)
        await admin.execute(
            """
            insert into heartbeat_profile_grants(
                profile_id, subject_kind, subject_ref, permission,
                granted_by_principal
            ) values
                ($1, 'principal', 'U-ADMIN', 'admin', 'operator'),
                ($2, 'principal', 'U-ADMIN', 'admin', 'operator'),
                ($3, 'principal', 'U-ADMIN', 'admin', 'operator'),
                ($4, 'principal', 'U-ADMIN', 'admin', 'operator'),
                ($5, 'principal', 'U-SOURCE-ONLY', 'review', 'operator')
            on conflict do nothing
            """,
            target["profile_id"],
            same_scope["profile_id"],
            cross_namespace["profile_id"],
            target_two["profile_id"],
            source["profile_id"],
        )
        await admin.close()

        with self.assertRaises(PermissionError):
            await run_state.promote_memory_fact(
                fact_id=str(fact_id),
                target_profile_id=str(target["profile_id"]),
                actor_ref="U-ADMIN",
                expected_revision=2,
            )

        async def assume_feedback(connection: asyncpg.Connection) -> None:
            await connection.execute("set role centaur_heartbeat_feedback")
            await connection.execute(
                "set centaur.workflow_principal = 'workflow-heartbeat-feedback'"
            )

        feedback_pool = await asyncpg.create_pool(
            self.client_database_url.replace(
                "heartbeat_test_client", "heartbeat_feedback_client"
            ),
            min_size=1,
            max_size=1,
            setup=assume_feedback,
        )
        try:
            feedback = HeartbeatState(
                feedback_pool,
                workflow_name="heartbeat_feedback",
                workflow_run_id=str(uuid.uuid4()),
                workflow_task_id=str(uuid.uuid4()),
                workflow_principal="workflow-heartbeat-feedback",
            )
            with self.assertRaisesRegex(RuntimeError, "precondition"):
                await feedback.promote_memory_fact(
                    fact_id=str(fact_id),
                    target_profile_id=str(target["profile_id"]),
                    actor_ref="U-ADMIN",
                    expected_revision=1,
                )
            with self.assertRaisesRegex(PermissionError, "not authorized"):
                await feedback.promote_memory_fact(
                    fact_id=str(fact_id),
                    target_profile_id=str(target["profile_id"]),
                    actor_ref="U-NO-SOURCE-REVIEW",
                    expected_revision=2,
                )
            with self.assertRaisesRegex(PermissionError, "not authorized"):
                await feedback.promote_memory_fact(
                    fact_id=str(fact_id),
                    target_profile_id=str(target["profile_id"]),
                    actor_ref="U-SOURCE-ONLY",
                    expected_revision=2,
                )
            with self.assertRaisesRegex(PermissionError, "not authorized"):
                await feedback.promote_memory_fact(
                    fact_id=str(fact_id),
                    target_profile_id=str(same_scope["profile_id"]),
                    actor_ref="U-ADMIN",
                    expected_revision=2,
                )
            with self.assertRaisesRegex(PermissionError, "not authorized"):
                await feedback.promote_memory_fact(
                    fact_id=str(fact_id),
                    target_profile_id=str(cross_namespace["profile_id"]),
                    actor_ref="U-ADMIN",
                    expected_revision=2,
                )
            promoted = await feedback.promote_memory_fact(
                fact_id=str(fact_id),
                target_profile_id=str(target["profile_id"]),
                actor_ref="U-ADMIN",
                expected_revision=2,
                idempotency_key="shared-promotion-key",
            )
            replayed = await feedback.promote_memory_fact(
                fact_id=str(fact_id),
                target_profile_id=str(target["profile_id"]),
                actor_ref="U-ADMIN",
                expected_revision=2,
                idempotency_key="shared-promotion-key",
            )
            self.assertEqual(promoted["fact_id"], replayed["fact_id"])
            second_target = await feedback.promote_memory_fact(
                fact_id=str(fact_id),
                target_profile_id=str(target_two["profile_id"]),
                actor_ref="U-ADMIN",
                expected_revision=2,
                idempotency_key="shared-promotion-key",
            )
            self.assertNotEqual(promoted["fact_id"], second_target["fact_id"])
        finally:
            feedback_pool.terminate()

        admin = await asyncpg.connect(DATABASE_URL)
        try:
            promoted_row = await admin.fetchrow(
                "select * from memory_facts where fact_id = $1", uuid.UUID(promoted["fact_id"])
            )
            self.assertIsNotNone(promoted_row)
            assert promoted_row is not None
            self.assertEqual(promoted_row["owner_principal"], "workflow-heartbeat-run")
            self.assertEqual(promoted_row["promoted_from_fact_id"], fact_id)
            self.assertEqual(
                await admin.fetchval(
                    "select count(*) from memory_fact_evidence where fact_id = $1",
                    promoted_row["fact_id"],
                ),
                1,
            )
            self.assertEqual(
                await admin.fetchval(
                    "select count(*) from memory_fact_events where idempotency_key like 'memory-promotion:%'"
                ),
                4,
            )
            projection = await admin.fetchrow(
                """
                select body, access_scope, metadata
                  from company_context_documents
                 where source_document_id = $1
                """,
                str(promoted_row["fact_id"]),
            )
            self.assertIsNotNone(projection)
            assert projection is not None
            self.assertEqual(projection["body"], "GTM owns the account.")
            self.assertEqual(
                projection["access_scope"], "organization:promotion-test:org-1"
            )
            second_promoted_row = await admin.fetchrow(
                "select fact_id from memory_facts where fact_id = $1",
                uuid.UUID(second_target["fact_id"]),
            )
            self.assertIsNotNone(second_promoted_row)
            await admin.execute(
                "delete from memory_fact_events where fact_id in ($1, $2, $3)",
                fact_id,
                promoted_row["fact_id"],
                second_target["fact_id"],
            )
            await admin.execute(
                "delete from memory_fact_evidence where fact_id in ($1, $2, $3)",
                fact_id,
                promoted_row["fact_id"],
                second_target["fact_id"],
            )
            await admin.execute(
                "delete from memory_facts where fact_id in ($1, $2, $3)",
                fact_id,
                promoted_row["fact_id"],
                second_target["fact_id"],
            )
            await admin.execute(
                "delete from heartbeat_profiles where profile_id in ($1, $2, $3, $4, $5)",
                source["profile_id"],
                target["profile_id"],
                same_scope["profile_id"],
                cross_namespace["profile_id"],
                target_two["profile_id"],
            )
        finally:
            await admin.close()
