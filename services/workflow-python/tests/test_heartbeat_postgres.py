from __future__ import annotations

import hashlib
import os
import sys
import unittest
import uuid
from pathlib import Path

import asyncpg

WORKFLOW_PYTHON = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(WORKFLOW_PYTHON))

from api.heartbeat import HeartbeatState

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
        ):
            await self.pool.execute((MIGRATIONS / migration).read_text())

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
        async with self.pool.acquire() as connection:
            async with connection.transaction():
                await connection.execute("set local role centaur_heartbeat_feedback")
                self.assertEqual(await connection.fetchval("select count(*) from heartbeat_items"), 0)

        async with self.pool.acquire() as connection:
            async with connection.transaction():
                await connection.execute("set local role centaur_heartbeat_feedback")
                with self.assertRaises(asyncpg.InsufficientPrivilegeError):
                    await connection.fetchval("select count(*) from memory_facts")

        async with self.pool.acquire() as connection:
            async with connection.transaction():
                await connection.execute("set local role centaur_heartbeat_run")
                self.assertEqual(await connection.fetchval("select count(*) from memory_facts"), 0)

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
        self.assertEqual(len(await first.list_artifacts(run_id=str(run["run_id"]))), 1)
        with self.assertRaisesRegex(RuntimeError, "differs from the original"):
            await first.put_artifact(
                run_id=str(run["run_id"]),
                artifact_kind="source_input",
                artifact_key="linear",
                content={"observations": [], "items": []},
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
        await first.mark_delivery_sent(
            delivery_id=delivery["delivery_id"],
            provider_message_id="1710000000.000100",
            surfaced_item_ids=[synthesized["item_id"]],
        )

        async def assume_feedback_role(connection: asyncpg.Connection) -> None:
            await connection.execute("set role centaur_heartbeat_feedback")

        feedback_pool = await asyncpg.create_pool(
            DATABASE_URL,
            min_size=1,
            max_size=1,
            init=assume_feedback_role,
        )
        try:
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
            with self.assertRaises(PermissionError):
                await feedback.apply_action(
                    token=raw_token,
                    actor_ref="U-REVIEWER",
                    provider_event_key="slack-action-1",
                )
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
            observations=[observation],
            items=[item],
            expected_checkpoint_version=1,
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
