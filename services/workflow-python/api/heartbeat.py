from __future__ import annotations

import datetime as dt
import hashlib
import hmac
import json
import secrets
import uuid
from typing import Any

_ID_NAMESPACE = uuid.UUID("ba55d079-050d-496d-a2a0-9c4f96e64c4f")
_SCOPES = {"organization", "team", "personal"}
_SENSITIVITIES = {"public", "internal", "confidential", "restricted"}
_ACTIONS = {"approve", "assign", "park", "snooze", "not_useful", "prepare_draft"}
_MEMORY_STATUSES = {"proposed", "confirmed", "disputed", "superseded", "forgotten", "expired"}
_ARTIFACT_KINDS = {
    "source_input",
    "source_error",
    "ranked_candidates",
    "synthesis_output",
    "delivery_preview",
    "synthesis_attempt",
}
_MAX_ARTIFACT_BYTES = 2 * 1024 * 1024


def _json(value: Any) -> str:
    return json.dumps(value, separators=(",", ":"), sort_keys=True, default=str)


def _json_value(value: Any) -> Any:
    if isinstance(value, str):
        try:
            return json.loads(value)
        except json.JSONDecodeError:
            return value
    return value


def _delivery_token(seed: str, kind: str, spec: dict[str, Any]) -> str:
    """Derive the one-time bearer token without persisting its plaintext."""
    material = f"{kind}:{_json(spec)}".encode()
    return hmac.new(seed.encode(), material, hashlib.sha256).hexdigest()


def _row(row: Any) -> dict[str, Any] | None:
    if row is None:
        return None
    result = dict(row)
    for key, value in list(result.items()):
        result[key] = _json_value(value)
    return result


def _uuid(kind: str, *parts: Any) -> uuid.UUID:
    joined = ":".join(str(part) for part in parts)
    return uuid.uuid5(_ID_NAMESPACE, f"{kind}:{joined}")


def _parse_uuid(value: str, field: str) -> uuid.UUID:
    try:
        return uuid.UUID(str(value))
    except (TypeError, ValueError) as exc:
        raise ValueError(f"{field} must be a UUID") from exc


def _parse_time(value: Any) -> dt.datetime | None:
    if value is None or isinstance(value, dt.datetime):
        return value
    if not isinstance(value, str):
        raise TypeError("timestamp must be RFC3339 text")
    parsed = dt.datetime.fromisoformat(value)
    return parsed if parsed.tzinfo else parsed.replace(tzinfo=dt.UTC)


class HeartbeatState:
    """Typed durable state for trusted Centaur workflow modules.

    The caller never chooses an executor identity. It is pinned by workflow
    discovery and checked against the registered profile on every run.
    """

    def __init__(
        self,
        pool: Any,
        *,
        workflow_name: str,
        workflow_run_id: str,
        workflow_task_id: str,
        workflow_principal: str | None,
    ) -> None:
        self._pool = pool
        self.workflow_name = workflow_name
        self.workflow_run_id = workflow_run_id
        self.workflow_task_id = workflow_task_id
        self.workflow_principal = workflow_principal

    def _require_ready(self) -> None:
        if self._pool is None:
            raise RuntimeError("Heartbeat requires a workflow-host Postgres grant")
        if not self.workflow_principal:
            raise RuntimeError("Heartbeat requires WORKFLOW_PRINCIPAL")

    def _require_memory_workflow(self) -> None:
        # Until the Rust workflow identity is propagated through the typed
        # service boundary, constrain memory APIs to the two reviewed workflow
        # entry points. A caller cannot turn an arbitrary workflow-host module
        # into a memory reader merely by constructing this facade.
        if self.workflow_name not in {"heartbeat_run", "heartbeat_feedback"}:
            raise PermissionError("workflow is not authorized for memory APIs")

    async def _require_profile_executor(self, profile_id: uuid.UUID) -> dict[str, Any]:
        profile = await self._pool.fetchrow(
            """
            select * from heartbeat_profiles
            where profile_id = $1 and workflow_name = $2
              and executor_principal_foreign_id = $3
            """,
            profile_id,
            self.workflow_name,
            self.workflow_principal,
        )
        if profile is None:
            raise PermissionError(
                "workflow principal does not operate this heartbeat profile"
            )
        return _row(profile) or {}

    async def _require_run_executor(
        self, run_id: uuid.UUID, profile_id: uuid.UUID | None = None
    ) -> dict[str, Any]:
        run = await self._pool.fetchrow(
            """
            select r.* from heartbeat_runs r
            join heartbeat_profiles p on p.profile_id = r.profile_id
            where r.run_id = $1 and p.workflow_name = $2
              and r.executor_principal_foreign_id = $3
              and ($4::uuid is null or r.profile_id = $4)
            """,
            run_id,
            self.workflow_name,
            self.workflow_principal,
            profile_id,
        )
        if run is None:
            raise PermissionError(
                "workflow principal does not operate this heartbeat run"
            )
        return _row(run) or {}

    async def register_profile(self, definition: dict[str, Any]) -> dict[str, Any]:
        self._require_ready()
        namespace = str(definition.get("namespace") or "default").strip()
        name = str(definition.get("name") or "").strip()
        scope_kind = str(definition.get("scope_kind") or "").strip()
        scope_ref = str(definition.get("scope_ref") or "").strip()
        definition_hash = str(definition.get("definition_hash") or "").strip()
        definition_version = int(definition.get("definition_version") or 0)
        if not namespace or not name or not scope_ref or not definition_hash:
            raise ValueError(
                "profile namespace, name, scope_ref, and definition_hash are required"
            )
        if scope_kind not in _SCOPES:
            raise ValueError(f"unsupported profile scope_kind {scope_kind!r}")
        if definition_version <= 0:
            raise ValueError("profile definition_version must be positive")

        profile_id = _uuid("profile", namespace, name)
        row = await self._pool.fetchrow(
            """
            insert into heartbeat_profiles (
                profile_id, namespace, name, scope_kind, scope_ref, workflow_name,
                executor_principal_foreign_id, definition_hash, definition_version,
                destination, required_sources, optional_sources, delivery_policy, enabled
            ) values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10::jsonb, $11, $12,
                      $13::jsonb, $14)
            on conflict (namespace, name) do update set
                scope_kind = excluded.scope_kind,
                scope_ref = excluded.scope_ref,
                workflow_name = excluded.workflow_name,
                executor_principal_foreign_id = excluded.executor_principal_foreign_id,
                definition_hash = excluded.definition_hash,
                definition_version = excluded.definition_version,
                destination = excluded.destination,
                required_sources = excluded.required_sources,
                optional_sources = excluded.optional_sources,
                delivery_policy = excluded.delivery_policy,
                enabled = excluded.enabled,
                updated_at = now()
            where heartbeat_profiles.executor_principal_foreign_id = excluded.executor_principal_foreign_id
            returning *
            """,
            profile_id,
            namespace,
            name,
            scope_kind,
            scope_ref,
            self.workflow_name,
            self.workflow_principal,
            definition_hash,
            definition_version,
            _json(definition.get("destination") or {}),
            list(definition.get("required_sources") or []),
            list(definition.get("optional_sources") or []),
            _json(definition.get("delivery_policy") or {}),
            bool(definition.get("enabled", False)),
        )
        if row is None:
            raise PermissionError(
                "profile executor principal cannot be changed by this workflow"
            )

        reviewer_refs = sorted(
            {
                str(value).strip()
                for value in definition.get("reviewer_refs") or []
                if str(value).strip()
            }
        )
        await self._pool.execute(
            "select heartbeat_replace_profile_grants($1, $2, $3::text[])",
            profile_id,
            self.workflow_principal,
            reviewer_refs,
        )
        return _row(row) or {}

    async def begin_run(
        self,
        *,
        profile_id: str,
        trigger: str,
        definition_hash: str,
        prompt_version: str,
        scheduled_for: Any = None,
    ) -> dict[str, Any]:
        self._require_ready()
        profile_uuid = _parse_uuid(profile_id, "profile_id")
        workflow_run_uuid = _parse_uuid(self.workflow_run_id, "workflow_run_id")
        workflow_task_uuid = _parse_uuid(self.workflow_task_id, "workflow_task_id")
        if trigger not in {"schedule", "manual", "event", "replay"}:
            raise ValueError(f"unsupported heartbeat trigger {trigger!r}")
        profile = await self._pool.fetchrow(
            """
            select * from heartbeat_profiles
            where profile_id = $1 and workflow_name = $2
              and executor_principal_foreign_id = $3
            """,
            profile_uuid,
            self.workflow_name,
            self.workflow_principal,
        )
        if profile is None:
            raise PermissionError(
                "workflow principal does not operate this heartbeat profile"
            )
        if trigger in {"schedule", "event"} and not profile["enabled"]:
            raise PermissionError("scheduled or event heartbeat profile is disabled")
        if profile["definition_hash"] != definition_hash:
            raise ValueError("profile definition changed after registration")
        row = await self._pool.fetchrow(
            """
            insert into heartbeat_runs (
                run_id, profile_id, workflow_run_id, workflow_task_id, trigger,
                scheduled_for, profile_definition_hash, prompt_version,
                executor_principal_foreign_id, status
            ) values ($1, $2, $1, $3, $4, $5, $6, $7, $8, 'collecting')
            on conflict (profile_id, workflow_run_id) do update set
                workflow_task_id = excluded.workflow_task_id
            returning *
            """,
            workflow_run_uuid,
            profile_uuid,
            workflow_task_uuid,
            trigger,
            _parse_time(scheduled_for),
            definition_hash,
            prompt_version,
            self.workflow_principal,
        )
        return _row(row) or {}

    async def source_checkpoint(
        self, *, profile_id: str, source_key: str
    ) -> dict[str, Any]:
        self._require_ready()
        profile_uuid = _parse_uuid(profile_id, "profile_id")
        await self._require_profile_executor(profile_uuid)
        row = await self._pool.fetchrow(
            "select * from heartbeat_source_checkpoints where profile_id = $1 and source_key = $2",
            profile_uuid,
            source_key,
        )
        return _row(row) or {
            "profile_id": str(profile_uuid),
            "source_key": source_key,
            "version": 0,
        }

    async def commit_source_batch(
        self,
        *,
        profile_id: str,
        run_id: str,
        source_key: str,
        observations: list[dict[str, Any]],
        items: list[dict[str, Any]],
        expected_checkpoint_version: int = 0,
        next_cursor: Any = None,
        watermark: Any = None,
        complete: bool = True,
        freshness_deadline: Any = None,
        error: Any = None,
    ) -> dict[str, Any]:
        self._require_ready()
        profile_uuid = _parse_uuid(profile_id, "profile_id")
        run_uuid = _parse_uuid(run_id, "run_id")
        if str(run_uuid) != str(self.workflow_run_id):
            raise PermissionError("workflow may update only its current heartbeat run")
        await self._require_profile_executor(profile_uuid)
        await self._require_run_executor(run_uuid, profile_uuid)
        attempted_at = dt.datetime.now(dt.UTC)
        observation_ids: dict[tuple[str, str], uuid.UUID] = {}
        inserted_observations = 0
        changed_items = 0

        async with self._pool.acquire() as connection:
            async with connection.transaction():
                checkpoint = await connection.fetchrow(
                    """
                    select * from heartbeat_source_checkpoints
                    where profile_id = $1 and source_key = $2 for update
                    """,
                    profile_uuid,
                    source_key,
                )
                actual_version = int(checkpoint["version"]) if checkpoint else 0
                if actual_version != int(expected_checkpoint_version):
                    raise RuntimeError(
                        f"source checkpoint conflict for {source_key}: expected "
                        f"{expected_checkpoint_version}, got {actual_version}"
                    )

                if error is None:
                    for observation in observations:
                        object_id = str(
                            observation.get("source_object_id") or ""
                        ).strip()
                        revision = str(observation.get("source_revision") or "").strip()
                        content_hash = str(
                            observation.get("content_hash") or ""
                        ).strip()
                        title = str(observation.get("title") or "").strip()
                        sensitivity = str(observation.get("sensitivity") or "internal")
                        if (
                            not object_id
                            or not revision
                            or not content_hash
                            or not title
                        ):
                            raise ValueError(
                                "observation identity, hash, and title are required"
                            )
                        if sensitivity not in _SENSITIVITIES:
                            raise ValueError(
                                f"unsupported observation sensitivity {sensitivity!r}"
                            )
                        observation_id = _uuid(
                            "observation", profile_uuid, source_key, object_id, revision
                        )
                        observation_ids[(object_id, revision)] = observation_id
                        result = await connection.execute(
                            """
                            insert into heartbeat_observations (
                                observation_id, profile_id, run_id, source_key,
                                source_object_id, source_revision, source_updated_at,
                                content_hash, entity_keys, title, source_url,
                                normalized_payload, sensitivity
                            ) values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11,
                                      $12::jsonb, $13)
                            on conflict (profile_id, source_key, source_object_id, source_revision)
                            do nothing
                            """,
                            observation_id,
                            profile_uuid,
                            run_uuid,
                            source_key,
                            object_id,
                            revision,
                            _parse_time(observation.get("source_updated_at")),
                            content_hash,
                            list(observation.get("entity_keys") or []),
                            title,
                            observation.get("source_url"),
                            _json(observation.get("payload") or {}),
                            sensitivity,
                        )
                        inserted_observations += int(result.endswith(" 1"))
                        stored_hash = await connection.fetchval(
                            """
                            select content_hash from heartbeat_observations
                            where observation_id = $1
                            """,
                            observation_id,
                        )
                        if stored_hash != content_hash:
                            raise RuntimeError(
                                "heartbeat source revision changed immutable content"
                            )

                    for item in items:
                        story_key = str(item.get("story_key") or "").strip()
                        material_hash = str(item.get("material_hash") or "").strip()
                        title = str(item.get("title") or "").strip()
                        item_type = str(item.get("item_type") or "").strip()
                        if (
                            not story_key
                            or not material_hash
                            or not title
                            or not item_type
                        ):
                            raise ValueError(
                                "item story_key, material_hash, title, and item_type are required"
                            )
                        item_id = _uuid("item", profile_uuid, story_key)
                        existing = await connection.fetchrow(
                            "select * from heartbeat_items where profile_id = $1 and story_key = $2 for update",
                            profile_uuid,
                            story_key,
                        )
                        changed = (
                            existing is None
                            or existing["material_hash"] != material_hash
                        )
                        old_status = str(existing["status"]) if existing else None
                        if existing is None:
                            row = await connection.fetchrow(
                                """
                                insert into heartbeat_items (
                                    item_id, profile_id, story_key, item_type, entity_keys,
                                    title, status, priority_tier, due_at, owner_ref,
                                    proposed_action, material_hash
                                ) values ($1, $2, $3, $4, $5, $6, 'open', $7, $8, $9,
                                          $10::jsonb, $11)
                                returning *
                                """,
                                item_id,
                                profile_uuid,
                                story_key,
                                item_type,
                                list(item.get("entity_keys") or []),
                                title,
                                int(item.get("priority_tier", 3)),
                                _parse_time(item.get("due_at")),
                                item.get("owner_ref"),
                                _json(item.get("proposed_action") or {}),
                                material_hash,
                            )
                        elif changed:
                            # A material source revision invalidates a prior snooze as
                            # well as a terminal disposition. Snooze suppresses a
                            # delivery window; it must not suppress newly changed
                            # source evidence.
                            reopen = old_status in {"resolved", "dismissed", "stale", "snoozed"}
                            row = await connection.fetchrow(
                                """
                                update heartbeat_items set
                                    item_type = $3, entity_keys = $4, title = $5,
                                    status = case when $6 then 'open' else status end,
                                    disposition = case when $6 then null else disposition end,
                                    priority_tier = $7, due_at = $8, owner_ref = $9,
                                    proposed_action = $10::jsonb, material_hash = $11,
                                    last_changed_at = now(), snooze_until = case when $6 then null else snooze_until end,
                                    resolved_at = case when $6 then null else resolved_at end,
                                    version = version + 1
                                where profile_id = $1 and story_key = $2
                                returning *
                                """,
                                profile_uuid,
                                story_key,
                                item_type,
                                list(item.get("entity_keys") or []),
                                title,
                                reopen,
                                int(item.get("priority_tier", 3)),
                                _parse_time(item.get("due_at")),
                                item.get("owner_ref"),
                                _json(item.get("proposed_action") or {}),
                                material_hash,
                            )
                        else:
                            row = existing
                        assert row is not None
                        if changed:
                            changed_items += 1
                            event_type = (
                                "created" if existing is None else "material_change"
                            )
                            event_key = f"source:{run_uuid}:{source_key}:{story_key}:{material_hash}"
                            await connection.execute(
                                """
                                insert into heartbeat_item_events (
                                    event_id, item_id, run_id, event_type, from_status,
                                    to_status, item_version, actor_kind, actor_ref,
                                    reason, payload, idempotency_key
                                ) values ($1, $2, $3, $4, $5, $6, $7, 'source', $8,
                                          $9, $10::jsonb, $11)
                                on conflict (idempotency_key) do nothing
                                """,
                                _uuid("item-event", event_key),
                                row["item_id"],
                                run_uuid,
                                event_type,
                                old_status,
                                row["status"],
                                row["version"],
                                source_key,
                                item.get("change_reason"),
                                _json({"material_hash": material_hash}),
                                event_key,
                            )
                        for ref in item.get("observation_refs") or []:
                            ref_key = (
                                str(ref.get("source_object_id") or ""),
                                str(ref.get("source_revision") or ""),
                            )
                            observation_id = observation_ids.get(ref_key)
                            if observation_id is None:
                                continue
                            await connection.execute(
                                """
                                insert into heartbeat_item_observations (
                                    item_id, observation_id, relation, linked_by
                                ) values ($1, $2, $3, 'deterministic')
                                on conflict do nothing
                                """,
                                row["item_id"],
                                observation_id,
                                str(ref.get("relation") or "primary"),
                            )

                source_health = {
                    "status": "ok"
                    if error is None and complete
                    else "partial"
                    if error is None
                    else "failed",
                    "attempted_at": attempted_at.isoformat(),
                    "complete": bool(complete and error is None),
                    "error": error,
                }
                await connection.execute(
                    """
                    insert into heartbeat_source_checkpoints (
                        profile_id, source_key, cursor, watermark, last_attempted_at,
                        last_succeeded_at, last_complete_scan_at, freshness_deadline,
                        consecutive_failures, last_error, version
                    ) values ($1, $2, $3::jsonb, $4::timestamptz, $5::timestamptz,
                              case when $6::jsonb is null then $5::timestamptz else null end,
                              case when $7::boolean and $6::jsonb is null then $5::timestamptz else null end,
                              $8::timestamptz, case when $6::jsonb is null then 0 else 1 end,
                              $6::jsonb, 1)
                    on conflict (profile_id, source_key) do update set
                        cursor = case when excluded.last_error is null then excluded.cursor else heartbeat_source_checkpoints.cursor end,
                        watermark = case when excluded.last_error is null then excluded.watermark else heartbeat_source_checkpoints.watermark end,
                        last_attempted_at = excluded.last_attempted_at,
                        last_succeeded_at = case when excluded.last_error is null then excluded.last_succeeded_at else heartbeat_source_checkpoints.last_succeeded_at end,
                        last_complete_scan_at = case when excluded.last_complete_scan_at is not null then excluded.last_complete_scan_at else heartbeat_source_checkpoints.last_complete_scan_at end,
                        freshness_deadline = case when excluded.last_error is null then excluded.freshness_deadline else heartbeat_source_checkpoints.freshness_deadline end,
                        consecutive_failures = case when excluded.last_error is null then 0 else heartbeat_source_checkpoints.consecutive_failures + 1 end,
                        last_error = excluded.last_error,
                        version = heartbeat_source_checkpoints.version + 1
                    """,
                    profile_uuid,
                    source_key,
                    _json(next_cursor),
                    _parse_time(watermark),
                    attempted_at,
                    _json(error) if error is not None else None,
                    bool(complete),
                    _parse_time(freshness_deadline),
                )
                await connection.execute(
                    """
                    update heartbeat_runs
                    set source_health = source_health || jsonb_build_object($2::text, $3::jsonb)
                    where run_id = $1 and profile_id = $4
                    """,
                    run_uuid,
                    source_key,
                    _json(source_health),
                    profile_uuid,
                )
        return {
            "source_key": source_key,
            "inserted_observations": inserted_observations,
            "changed_items": changed_items,
            "health": source_health,
            "checkpoint_version": int(expected_checkpoint_version) + 1,
        }

    async def list_candidates(
        self, *, profile_id: str, limit: int = 25
    ) -> list[dict[str, Any]]:
        self._require_ready()
        profile_uuid = _parse_uuid(profile_id, "profile_id")
        await self._require_profile_executor(profile_uuid)
        limit = max(1, min(int(limit), 100))
        run_uuid = _parse_uuid(self.workflow_run_id, "workflow_run_id")
        await self._require_run_executor(run_uuid, profile_uuid)
        async with self._pool.acquire() as connection:
            async with connection.transaction():
                unsnoozed = await connection.fetch(
                    """
                    update heartbeat_items set status = 'open', snooze_until = null,
                        version = version + 1
                    where profile_id = $1 and status = 'snoozed'
                      and snooze_until <= now()
                    returning item_id, version
                    """,
                    profile_uuid,
                )
                for item in unsnoozed:
                    event_key = (
                        f"unsnoozed:{run_uuid}:{item['item_id']}:v{item['version']}"
                    )
                    await connection.execute(
                        """
                        insert into heartbeat_item_events (
                            event_id, item_id, run_id, event_type, from_status, to_status,
                            item_version, actor_kind, actor_ref, idempotency_key
                        ) values ($1, $2, $3, 'unsnoozed', 'snoozed', 'open', $4,
                                  'system', $5, $6)
                        on conflict (idempotency_key) do nothing
                        """,
                        _uuid("item-event", event_key),
                        item["item_id"],
                        run_uuid,
                        item["version"],
                        self.workflow_principal,
                        event_key,
                    )
        rows = await self._pool.fetch(
            """
            select i.*,
                   exists(
                       select 1 from heartbeat_item_events changed
                       where changed.item_id = i.item_id
                         and changed.run_id = $2
                         and changed.event_type in ('created', 'material_change')
                   ) changed_in_run,
                   coalesce(jsonb_agg(jsonb_build_object(
                       'observation_id', o.observation_id,
                       'source_key', o.source_key,
                       'source_object_id', o.source_object_id,
                       'source_revision', o.source_revision,
                       'source_updated_at', o.source_updated_at,
                       'title', o.title,
                       'source_url', o.source_url,
                       'payload', o.normalized_payload,
                       'relation', o.relation
                   ) order by o.captured_at desc) filter (where o.observation_id is not null), '[]'::jsonb) observations
            from heartbeat_items i
            left join lateral (
                select observation.*, item_observation.relation
                from heartbeat_item_observations item_observation
                join heartbeat_observations observation
                  on observation.observation_id = item_observation.observation_id
                where item_observation.item_id = i.item_id
                order by observation.captured_at desc, observation.observation_id
                limit 10
            ) o on true
            where i.profile_id = $1 and i.status = 'open'
            group by i.item_id
            order by i.priority_tier, i.due_at nulls last, i.last_changed_at desc, i.item_id
            limit $3
            """,
            profile_uuid,
            run_uuid,
            limit,
        )
        return [_row(row) or {} for row in rows]

    async def put_artifact(
        self,
        *,
        run_id: str,
        artifact_kind: str,
        artifact_key: str,
        content: Any,
    ) -> dict[str, Any]:
        self._require_ready()
        run_uuid = _parse_uuid(run_id, "run_id")
        if str(run_uuid) != str(self.workflow_run_id):
            raise PermissionError("workflow may capture only its current heartbeat run")
        await self._require_run_executor(run_uuid)
        artifact_kind = artifact_kind.strip()
        artifact_key = artifact_key.strip()
        if artifact_kind not in _ARTIFACT_KINDS:
            raise ValueError(f"unsupported heartbeat artifact kind {artifact_kind!r}")
        if not artifact_key or len(artifact_key) > 256:
            raise ValueError("heartbeat artifact_key must contain 1..=256 characters")
        encoded = _json(content)
        if len(encoded.encode()) > _MAX_ARTIFACT_BYTES:
            raise ValueError("heartbeat artifact exceeds the 2 MiB limit")
        content_hash = hashlib.sha256(encoded.encode()).hexdigest()
        row = await self._pool.fetchrow(
            """
            insert into heartbeat_run_artifacts (
                artifact_id, run_id, artifact_kind, artifact_key, content,
                content_hash
            ) values ($1, $2, $3, $4, $5::jsonb, $6)
            on conflict (run_id, artifact_kind, artifact_key) do update set
                content = excluded.content
            where heartbeat_run_artifacts.content_hash = excluded.content_hash
            returning *
            """,
            _uuid("run-artifact", run_uuid, artifact_kind, artifact_key),
            run_uuid,
            artifact_kind,
            artifact_key,
            encoded,
            content_hash,
        )
        if row is None:
            raise RuntimeError(
                "heartbeat replay artifact differs from the original run"
            )
        return _row(row) or {}

    async def list_artifacts(self, *, run_id: str) -> list[dict[str, Any]]:
        self._require_ready()
        run_uuid = _parse_uuid(run_id, "run_id")
        await self._require_run_executor(run_uuid)
        rows = await self._pool.fetch(
            """
            select * from heartbeat_run_artifacts
            where run_id = $1
            order by created_at, artifact_id
            """,
            run_uuid,
        )
        return [_row(row) or {} for row in rows]

    async def commit_synthesis(
        self,
        *,
        profile_id: str,
        run_id: str,
        items: list[dict[str, Any]],
        memory_proposals: list[dict[str, Any]] | None = None,
        candidate_count: int | None = None,
    ) -> dict[str, Any]:
        self._require_ready()
        profile_uuid = _parse_uuid(profile_id, "profile_id")
        run_uuid = _parse_uuid(run_id, "run_id")
        if str(run_uuid) != str(self.workflow_run_id):
            raise PermissionError("workflow may update only its current heartbeat run")
        await self._require_profile_executor(profile_uuid)
        await self._require_run_executor(run_uuid, profile_uuid)
        proposals = memory_proposals or []
        committed_memory_proposals: list[dict[str, Any]] = []
        if len(items) > 100 or len(proposals) > 100:
            raise ValueError(
                "heartbeat synthesis is limited to 100 items and proposals"
            )
        committed_candidate_count = (
            len(items) if candidate_count is None else int(candidate_count)
        )
        if committed_candidate_count < len(items) or committed_candidate_count > 100:
            raise ValueError(
                "heartbeat candidate_count must contain selected items and be at most 100"
            )
        async with self._pool.acquire() as connection:
            async with connection.transaction():
                for item in items:
                    item_uuid = _parse_uuid(str(item.get("item_id") or ""), "item_id")
                    expected_version = int(item.get("expected_version") or 0)
                    updated = await connection.fetchrow(
                        """
                        update heartbeat_items set
                            summary = $4,
                            proposed_action = $5::jsonb
                        where item_id = $1 and profile_id = $2 and version = $3 and status = 'open'
                        returning *
                        """,
                        item_uuid,
                        profile_uuid,
                        expected_version,
                        str(
                            item.get("summary") or item.get("what_changed") or ""
                        ).strip(),
                        _json(
                            {
                                "headline": item.get("headline"),
                                "why_now": item.get("why_now"),
                                "recommended_disposition": item.get(
                                    "recommended_disposition"
                                ),
                                "recommendation": item.get("recommendation"),
                                "evidence_observation_ids": item.get(
                                    "evidence_observation_ids"
                                )
                                or [],
                                "uncertainties": item.get("uncertainties") or [],
                            }
                        ),
                    )
                    if updated is None:
                        raise RuntimeError(
                            f"heartbeat item {item_uuid} changed before synthesis commit"
                        )
                    evidence_ids = [
                        _parse_uuid(str(value), "evidence_observation_id")
                        for value in item.get("evidence_observation_ids") or []
                    ]
                    if not evidence_ids:
                        raise ValueError("heartbeat synthesis items require evidence")
                    for evidence_id in evidence_ids:
                        linked = await connection.fetchval(
                            """
                            select exists(
                                select 1 from heartbeat_item_observations io
                                join heartbeat_observations o
                                  on o.observation_id = io.observation_id
                                where io.item_id = $1 and o.observation_id = $2
                                  and o.profile_id = $3
                            )
                            """,
                            item_uuid,
                            evidence_id,
                            profile_uuid,
                        )
                        if not linked:
                            raise ValueError(
                                "heartbeat synthesis evidence is outside the item"
                            )
                    event_key = f"synthesis:{run_uuid}:{item_uuid}:v{expected_version}"
                    await connection.execute(
                        """
                        insert into heartbeat_item_events (
                            event_id, item_id, run_id, event_type, from_status, to_status,
                            item_version, actor_kind, actor_ref, reason, payload, idempotency_key
                        ) values ($1, $2, $3, 'synthesized', 'open', 'open', $4,
                                  'model', $5, $6, $7::jsonb, $8)
                        on conflict (idempotency_key) do nothing
                        """,
                        _uuid("item-event", event_key),
                        item_uuid,
                        run_uuid,
                        expected_version,
                        self.workflow_principal,
                        item.get("why_now"),
                        _json(
                            {
                                "recommended_disposition": item.get(
                                    "recommended_disposition"
                                )
                            }
                        ),
                        event_key,
                    )

                profile = await connection.fetchrow(
                    "select namespace, scope_kind, scope_ref from heartbeat_profiles where profile_id = $1",
                    profile_uuid,
                )
                if profile is None:
                    raise RuntimeError("heartbeat profile disappeared")
                for proposal in proposals:
                    subject_key = str(proposal.get("subject_key") or "").strip()
                    predicate = str(proposal.get("predicate") or "").strip()
                    canonical_text = str(proposal.get("canonical_text") or "").strip()
                    sensitivity = str(proposal.get("sensitivity") or "internal")
                    value = proposal.get("value") or {}
                    if not subject_key or not predicate or not canonical_text:
                        raise ValueError(
                            "memory proposal subject, predicate, and canonical_text are required"
                        )
                    if sensitivity not in _SENSITIVITIES:
                        raise ValueError(
                            f"unsupported memory sensitivity {sensitivity!r}"
                        )
                    proposal_evidence = proposal.get("evidence_observation_ids") or []
                    if not proposal_evidence:
                        raise ValueError("memory proposals require heartbeat evidence")
                    value_hash = hashlib.sha256(_json(value).encode()).hexdigest()
                    fact_id = _uuid(
                        "memory-fact",
                        self.workflow_principal,
                        profile["namespace"],
                        profile["scope_kind"],
                        profile["scope_ref"],
                        subject_key,
                        predicate,
                        value_hash,
                    )
                    await connection.execute(
                        """
                        insert into memory_facts (
                            fact_id, owner_principal, namespace, scope_kind, scope_ref, subject_key,
                            predicate, value, canonical_text, status, sensitivity,
                            valid_until, observed_at, proposed_by_principal
                        ) values ($1, $2, $3, $4, $5, $6, $7, $8::jsonb, $9,
                                  'proposed', $10, $11, now(), $12)
                        on conflict (fact_id) do nothing
                        """,
                        fact_id,
                        self.workflow_principal,
                        profile["namespace"],
                        profile["scope_kind"],
                        profile["scope_ref"],
                        subject_key,
                        predicate,
                        _json(value),
                        canonical_text,
                        sensitivity,
                        _parse_time(proposal.get("valid_until")),
                        self.workflow_principal,
                    )
                    memory_event_key = f"heartbeat:{run_uuid}:{fact_id}:proposed"
                    await connection.execute(
                        """
                        insert into memory_fact_events (
                            event_id, fact_id, event_type, actor_ref, payload,
                            idempotency_key
                        ) values ($1, $2, 'proposed', $3, $4::jsonb, $5)
                        on conflict (idempotency_key) do nothing
                        """,
                        _uuid("memory-event", memory_event_key),
                        fact_id,
                        self.workflow_principal,
                        _json({"heartbeat_run_id": str(run_uuid)}),
                        memory_event_key,
                    )
                    await connection.execute(
                        """
                        insert into heartbeat_run_memory_facts (run_id, fact_id)
                        values ($1, $2) on conflict do nothing
                        """,
                        run_uuid,
                        fact_id,
                    )
                    for observation_id in proposal_evidence:
                        evidence_uuid = _parse_uuid(
                            str(observation_id), "evidence_observation_id"
                        )
                        exists = await connection.fetchval(
                            "select exists(select 1 from heartbeat_observations where observation_id = $1 and profile_id = $2)",
                            evidence_uuid,
                            profile_uuid,
                        )
                        if not exists:
                            raise ValueError("memory evidence is outside the profile")
                        await connection.execute(
                            """
                            insert into memory_fact_evidence (
                                evidence_id, fact_id, evidence_kind, evidence_ref
                            ) values ($1, $2, 'heartbeat_observation', $3)
                            on conflict do nothing
                            """,
                            _uuid("memory-evidence", fact_id, evidence_uuid),
                            fact_id,
                            str(evidence_uuid),
                        )
                    committed = await connection.fetchrow(
                        "select fact_id, revision, status, sensitivity from memory_facts where fact_id = $1",
                        fact_id,
                    )
                    committed_memory_proposals.append(_row(committed) or {})
                await connection.execute(
                    """
                    update heartbeat_runs set status = 'committing',
                        candidate_count = $2, memory_proposal_count = $3
                    where run_id = $1 and profile_id = $4
                    """,
                    run_uuid,
                    committed_candidate_count,
                    len(proposals),
                    profile_uuid,
                )
        return {
            "candidate_count": committed_candidate_count,
            "committed_items": len(items),
            "memory_proposals": committed_memory_proposals,
            "memory_proposal_count": len(proposals),
        }

    async def prepare_delivery(
        self,
        *,
        run_id: str,
        destination_kind: str,
        destination_ref: str,
        rendered_payload: dict[str, Any],
        item_actions: list[dict[str, Any]] | None = None,
        memory_actions: list[dict[str, Any]] | None = None,
        token_ttl_seconds: int = 604800,
    ) -> dict[str, Any]:
        self._require_ready()
        run_uuid = _parse_uuid(run_id, "run_id")
        if str(run_uuid) != str(self.workflow_run_id):
            raise PermissionError("workflow may deliver only its current heartbeat run")
        run = await self._require_run_executor(run_uuid)
        profile_uuid = _parse_uuid(str(run["profile_id"]), "profile_id")
        if not destination_kind.strip() or not destination_ref.strip():
            raise ValueError("heartbeat delivery destination is required")
        item_actions = item_actions or []
        if len(item_actions) > 100:
            raise ValueError("heartbeat delivery is limited to 100 actions")
        memory_actions = memory_actions or []
        if len(memory_actions) > 100:
            raise ValueError("heartbeat memory delivery is limited to 100 actions")
        delivery_id = _uuid("delivery", run_uuid, destination_kind, destination_ref)
        client_message_id = f"heartbeat:{run_uuid}:{destination_kind}:{destination_ref}"
        tokens: list[dict[str, Any]] = []
        async with self._pool.acquire() as connection:
            async with connection.transaction():
                existing_delivery = await connection.fetchrow(
                    """
                    select * from heartbeat_deliveries
                    where client_message_id = $1
                    for update
                    """,
                    client_message_id,
                )
                if existing_delivery is not None:
                    # The delivery key is the workflow/run/destination idempotency
                    if _json_value(existing_delivery["rendered_payload"]) != rendered_payload:
                        raise RuntimeError(
                            "heartbeat delivery replay differs from the original payload"
                        )
                    if not existing_delivery["token_seed"]:
                        raise RuntimeError("heartbeat delivery cannot recover its token set")
                    stored_items = await connection.fetch(
                        """
                        select token_hash, item_id, item_version, action, payload
                          from heartbeat_action_tokens
                         where delivery_id = $1
                        """,
                        existing_delivery["delivery_id"],
                    )
                    requested_items = [
                        {
                            "item_id": str(_parse_uuid(str(a.get("item_id") or ""), "item_id")),
                            "item_version": int(a.get("item_version") or 0),
                            "action": str(a.get("action") or ""),
                            "payload": a.get("payload") or {},
                        }
                        for a in item_actions
                    ]
                    persisted_items = [
                        {
                            "item_id": str(row["item_id"]),
                            "item_version": row["item_version"],
                            "action": row["action"],
                            "payload": _json_value(row["payload"]) or {},
                        }
                        for row in stored_items
                    ]
                    if sorted(requested_items, key=_json) != sorted(persisted_items, key=_json):
                        raise RuntimeError("heartbeat delivery replay differs in item actions")
                    stored_memory = await connection.fetch(
                        """
                        select token_hash, fact_id, expected_revision, action, payload
                          from heartbeat_memory_action_tokens
                         where delivery_id = $1
                        """,
                        existing_delivery["delivery_id"],
                    )
                    requested_memory = [
                        {
                            "memory_fact_id": str(_parse_uuid(str(a.get("memory_fact_id") or a.get("fact_id") or ""), "memory_fact_id")),
                            "expected_revision": int(a.get("expected_revision") or 0),
                            "action": str(a.get("action") or ""),
                            "payload": a.get("payload") or {},
                        }
                        for a in memory_actions
                    ]
                    persisted_memory = [
                        {
                            "memory_fact_id": str(row["fact_id"]),
                            "expected_revision": row["expected_revision"],
                            "action": row["action"],
                            "payload": _json_value(row["payload"]) or {},
                        }
                        for row in stored_memory
                    ]
                    if sorted(requested_memory, key=_json) != sorted(persisted_memory, key=_json):
                        raise RuntimeError("heartbeat delivery replay differs in memory actions")
                    for spec in requested_items:
                        token = _delivery_token(existing_delivery["token_seed"], "item", spec)
                        matching = next(
                            row for row in stored_items
                            if str(row["item_id"]) == spec["item_id"]
                            and row["item_version"] == spec["item_version"]
                            and row["action"] == spec["action"]
                            and (_json_value(row["payload"]) or {}) == spec["payload"]
                        )
                        if matching["token_hash"] != hashlib.sha256(token.encode()).hexdigest():
                            raise RuntimeError("heartbeat delivery token set is corrupt")
                        tokens.append({
                            "item_id": spec["item_id"],
                            "action": spec["action"],
                            "token": token,
                        })
                    for spec in requested_memory:
                        token = _delivery_token(existing_delivery["token_seed"], "memory", spec)
                        matching = next(
                            row for row in stored_memory
                            if str(row["fact_id"]) == spec["memory_fact_id"]
                            and row["expected_revision"] == spec["expected_revision"]
                            and row["action"] == spec["action"]
                            and (_json_value(row["payload"]) or {}) == spec["payload"]
                        )
                        if matching["token_hash"] != hashlib.sha256(token.encode()).hexdigest():
                            raise RuntimeError("heartbeat delivery token set is corrupt")
                        tokens.append({
                            "memory_fact_id": spec["memory_fact_id"],
                            "action": spec["action"],
                            "token": token,
                        })
                    return {
                        "delivery_id": str(existing_delivery["delivery_id"]),
                        "client_message_id": client_message_id,
                        "tokens": tokens,
                        "replayed": True,
                        "status": existing_delivery["status"],
                    }
                token_seed = secrets.token_urlsafe(32)
                await connection.execute(
                    """
                    insert into heartbeat_deliveries (
                        delivery_id, run_id, destination_kind, destination_ref,
                        status, client_message_id, rendered_payload, token_seed
                    ) values ($1, $2, $3, $4, 'pending', $5, $6::jsonb, $7)
                    on conflict (client_message_id) do nothing
                    """,
                    delivery_id,
                    run_uuid,
                    destination_kind,
                    destination_ref,
                    client_message_id,
                    _json(rendered_payload),
                    token_seed,
                )
                for item_action in item_actions:
                    action = str(item_action.get("action") or "")
                    if action not in _ACTIONS:
                        raise ValueError(f"unsupported heartbeat action {action!r}")
                    item_id = _parse_uuid(
                        str(item_action.get("item_id") or ""), "item_id"
                    )
                    item_version = int(item_action.get("item_version") or 0)
                    valid_item = await connection.fetchval(
                        """
                        select exists(
                            select 1 from heartbeat_items
                            where item_id = $1 and profile_id = $2
                              and version = $3 and status = 'open'
                        )
                        """,
                        item_id,
                        profile_uuid,
                        item_version,
                    )
                    if not valid_item:
                        raise RuntimeError(
                            "heartbeat delivery item is stale or outside the run profile"
                        )
                    item_spec = {
                        "item_id": str(item_id),
                        "item_version": item_version,
                        "action": action,
                        "payload": item_action.get("payload") or {},
                    }
                    token = _delivery_token(token_seed, "item", item_spec)
                    token_hash = hashlib.sha256(token.encode()).hexdigest()
                    await connection.execute(
                        """
                        insert into heartbeat_action_tokens (
                            token_hash, delivery_id, item_id, item_version, action,
                            payload, expires_at
                        ) values ($1, $2, $3, $4, $5, $6::jsonb,
                                  now() + make_interval(secs => $7))
                        """,
                        token_hash,
                        delivery_id,
                        item_id,
                        item_version,
                        action,
                        _json(item_action.get("payload") or {}),
                        max(60, min(int(token_ttl_seconds), 2592000)),
                    )
                    tokens.append(
                        {"item_id": str(item_id), "action": action, "token": token}
                    )
                for memory_action in memory_actions:
                    action = str(memory_action.get("action") or "")
                    if action not in {"confirm", "dispute", "forget", "correct"}:
                        raise ValueError(f"unsupported memory action {action!r}")
                    fact_id = _parse_uuid(
                        str(memory_action.get("memory_fact_id") or memory_action.get("fact_id") or ""),
                        "memory_fact_id",
                    )
                    expected_revision = int(memory_action.get("expected_revision") or 0)
                    fact = await connection.fetchrow(
                        """
                        select fact_id, revision, status, sensitivity from memory_facts
                        where fact_id = $1 and owner_principal = $2 and status = 'proposed'
                          and namespace = (select namespace from heartbeat_profiles where profile_id = $4)
                          and scope_kind = (select scope_kind from heartbeat_profiles where profile_id = $4)
                          and scope_ref = (select scope_ref from heartbeat_profiles where profile_id = $4)
                          and exists (
                              select 1 from heartbeat_run_memory_facts rmf
                              where rmf.run_id = $3 and rmf.fact_id = memory_facts.fact_id
                          )
                        """,
                        fact_id,
                        self.workflow_principal,
                        run_uuid,
                        profile_uuid,
                    )
                    if fact is None or fact["revision"] != expected_revision:
                        raise RuntimeError("heartbeat memory action fact is stale or outside the run profile")
                    if fact["sensitivity"] in {"confidential", "restricted"}:
                        raise PermissionError("confidential and restricted memory cannot be delivered")
                    memory_spec = {
                        "memory_fact_id": str(fact_id),
                        "expected_revision": expected_revision,
                        "action": action,
                        "payload": memory_action.get("payload") or {},
                    }
                    token = _delivery_token(token_seed, "memory", memory_spec)
                    token_hash = hashlib.sha256(token.encode()).hexdigest()
                    await connection.execute(
                        """
                        insert into heartbeat_memory_action_tokens (
                            token_hash, delivery_id, fact_id, expected_revision,
                            action, payload, expires_at
                        ) values ($1, $2, $3, $4, $5, $6::jsonb,
                                  now() + make_interval(secs => $7))
                        """,
                        token_hash, delivery_id, fact_id, expected_revision, action,
                        _json(memory_action.get("payload") or {}),
                        max(60, min(int(token_ttl_seconds), 2592000)),
                    )
                    tokens.append(
                        {"memory_fact_id": str(fact_id), "action": action, "token": token}
                    )
                await connection.execute(
                    "update heartbeat_runs set status = 'delivering' where run_id = $1",
                    run_uuid,
                )
        return {
            "delivery_id": str(delivery_id),
            "client_message_id": client_message_id,
            "tokens": tokens,
            "replayed": False,
        }

    async def _memory_authorized(
        self,
        connection: Any,
        *,
        fact_id: uuid.UUID,
        actor_ref: str,
        lock: bool = False,
    ) -> dict[str, Any]:
        """Load a fact only through a reviewer/admin grant for its scope.

        Memory facts have no profile foreign key by design. The grant join below
        binds their namespace/scope to a registered profile, preventing a caller
        from selecting a different scope merely by supplying a fact UUID.
        """
        suffix = " for update" if lock else ""
        fact = await connection.fetchrow(
            f"""
            select f.* from memory_facts f
            where f.fact_id = $1
              and exists (
                  select 1
                  from heartbeat_profiles p
                  join heartbeat_profile_grants g on g.profile_id = p.profile_id
                  where p.namespace = f.namespace
                    and p.scope_kind = f.scope_kind
                    and p.scope_ref = f.scope_ref
                    and p.executor_principal_foreign_id = f.owner_principal
                    and g.subject_kind = 'principal'
                    and g.subject_ref = $2
                    and g.permission in ('review', 'admin')
              ){suffix}
            """,
            fact_id,
            actor_ref,
        )
        if fact is None:
            raise PermissionError("actor is not authorized for this memory scope")
        return _row(fact) or {}

    async def _memory_result(
        self, connection: Any, fact_id: uuid.UUID
    ) -> dict[str, Any]:
        fact = await connection.fetchrow(
            "select * from memory_facts where fact_id = $1", fact_id
        )
        if fact is None:
            raise RuntimeError("memory fact not found")
        result = _row(fact) or {}
        result["evidence"] = [
            _row(row) or {}
            for row in await connection.fetch(
                """
                select evidence_id, evidence_kind, evidence_ref, source_url,
                       excerpt, content_hash, created_at
                from memory_fact_evidence where fact_id = $1
                order by created_at, evidence_id
                """,
                fact_id,
            )
        ]
        result["events"] = [
            _row(row) or {}
            for row in await connection.fetch(
                """
                select event_id, event_type, actor_ref, reason, payload,
                       idempotency_key, created_at
                from memory_fact_events where fact_id = $1
                order by created_at, event_id
                """,
                fact_id,
            )
        ]
        return result

    async def list_memory_facts(
        self,
        *,
        actor_ref: str,
        namespace: str = "default",
        scope_kind: str | None = None,
        scope_ref: str | None = None,
        subject_key: str | None = None,
        predicate: str | None = None,
        include_nonconfirmed: bool = False,
        limit: int = 50,
    ) -> list[dict[str, Any]]:
        """List reviewer-visible facts with evidence and provenance."""
        self._require_ready()
        self._require_memory_workflow()
        actor_ref = actor_ref.strip()
        if not actor_ref:
            raise ValueError("actor_ref is required")
        if scope_kind is not None and scope_kind not in _SCOPES:
            raise ValueError(f"unsupported memory scope_kind {scope_kind!r}")
        limit = max(1, min(int(limit), 100))
        args: list[Any] = [namespace, actor_ref]
        predicates = [
            "f.namespace = $1",
            "g.subject_kind = 'principal'",
            "g.subject_ref = $2",
            "g.permission in ('review', 'admin')",
        ]
        if not include_nonconfirmed:
            predicates.append("f.status = 'confirmed'")
        if scope_kind is not None:
            args.append(scope_kind)
            predicates.append(f"f.scope_kind = ${len(args)}")
        if scope_ref is not None:
            args.append(scope_ref)
            predicates.append(f"f.scope_ref = ${len(args)}")
        if subject_key is not None:
            args.append(subject_key)
            predicates.append(f"f.subject_key = ${len(args)}")
        if predicate is not None:
            args.append(predicate)
            predicates.append(f"f.predicate = ${len(args)}")
        args.append(limit)
        limit_arg = len(args)
        rows = await self._pool.fetch(
            f"""
            select distinct f.*
            from memory_facts f
            join heartbeat_profiles p
              on p.namespace = f.namespace and p.scope_kind = f.scope_kind
             and p.scope_ref = f.scope_ref
            join heartbeat_profile_grants g on g.profile_id = p.profile_id
            where {' and '.join(predicates)}
              and p.executor_principal_foreign_id = f.owner_principal
            order by f.updated_at desc, f.fact_id
            limit ${limit_arg}
            """,
            *args,
        )
        results: list[dict[str, Any]] = []
        async with self._pool.acquire() as connection:
            async with connection.transaction():
                for row in rows:
                    results.append(await self._memory_result(connection, row["fact_id"]))
        return results

    async def get_memory_fact(
        self, *, fact_id: str, actor_ref: str, include_history: bool = False
    ) -> dict[str, Any]:
        self._require_ready()
        self._require_memory_workflow()
        fact_uuid = _parse_uuid(fact_id, "fact_id")
        async with self._pool.acquire() as connection:
            async with connection.transaction():
                await self._memory_authorized(
                    connection, fact_id=fact_uuid, actor_ref=actor_ref
                )
                result = await self._memory_result(connection, fact_uuid)
                if include_history:
                    result["history"] = [
                        _row(row) or {}
                        for row in await connection.fetch(
                            """
                            with recursive lineage as (
                                select fact_id, supersedes_fact_id, revision
                                from memory_facts where fact_id = $1
                                union all
                                select f.fact_id, f.supersedes_fact_id, f.revision
                                from memory_facts f join lineage l
                                  on f.fact_id = l.supersedes_fact_id
                            )
                            select f.* from memory_facts f
                            join lineage l on l.fact_id = f.fact_id
                            order by f.revision
                            """,
                            fact_uuid,
                        )
                    ]
                return result

    async def retrieve_memory_fact(
        self, *, fact_id: str, actor_ref: str, include_history: bool = False
    ) -> dict[str, Any]:
        """Named retrieval alias for workflow callers and future API adapters."""
        return await self.get_memory_fact(
            fact_id=fact_id, actor_ref=actor_ref, include_history=include_history
        )

    async def retrieve_memory_facts(self, **kwargs: Any) -> list[dict[str, Any]]:
        """Plural retrieval alias retaining the list filter contract."""
        return await self.list_memory_facts(**kwargs)

    async def retrieve_confirmed_memory(
        self,
        *,
        profile_id: str,
        entity_keys: list[str] | tuple[str, ...] | None = None,
        max_sensitivity: str = "internal",
        allowed_sensitivities: list[str] | tuple[str, ...] | None = None,
        limit: int = 25,
    ) -> list[dict[str, Any]]:
        """Retrieve confirmed memory for the workflow's registered profile.

        This is the model-facing read path: authorization comes from the pinned
        workflow executor and profile scope, never from a human actor supplied by
        workflow input. Reviewer-facing list/get APIs remain separate.
        """
        self._require_ready()
        profile_uuid = _parse_uuid(profile_id, "profile_id")
        profile = await self._require_profile_executor(profile_uuid)
        if max_sensitivity not in _SENSITIVITIES:
            raise ValueError(f"unsupported max_sensitivity {max_sensitivity!r}")
        sensitivity_rank = {
            "public": 0,
            "internal": 1,
            "confidential": 2,
            "restricted": 3,
        }
        max_rank = sensitivity_rank[max_sensitivity]
        if allowed_sensitivities is None:
            allowed = [
                sensitivity
                for sensitivity, rank in sensitivity_rank.items()
                if rank <= max_rank
            ]
        else:
            allowed = list(dict.fromkeys(allowed_sensitivities))
            if not allowed or any(value not in _SENSITIVITIES for value in allowed):
                raise ValueError("allowed_sensitivities contains an unsupported value")
            if any(sensitivity_rank[value] > max_rank for value in allowed):
                raise ValueError("allowed_sensitivities exceeds max_sensitivity")
        keys = [str(key).strip() for key in (entity_keys or []) if str(key).strip()]
        limit = max(1, min(int(limit), 100))
        rows = await self._pool.fetch(
            """
            select f.* from memory_facts f
            where f.owner_principal = $1
              and f.namespace = $2 and f.scope_kind = $3 and f.scope_ref = $4
              and f.status = 'confirmed'
              and f.sensitivity = any($5::text[])
              and (cardinality($6::text[]) = 0 or f.subject_key = any($6::text[]))
              and (f.valid_from is null or f.valid_from <= now())
              and (f.valid_until is null or f.valid_until > now())
            order by f.updated_at desc, f.fact_id
            limit $7
            """,
            self.workflow_principal,
            profile["namespace"],
            profile["scope_kind"],
            profile["scope_ref"],
            allowed,
            keys,
            limit,
        )
        results: list[dict[str, Any]] = []
        async with self._pool.acquire() as connection:
            async with connection.transaction():
                for row in rows:
                    results.append(await self._memory_result(connection, row["fact_id"]))
        return results

    async def _memory_event_exists(
        self, connection: Any, idempotency_key: str
    ) -> uuid.UUID | None:
        return await connection.fetchval(
            "select fact_id from memory_fact_events where idempotency_key = $1",
            idempotency_key,
        )

    async def _insert_memory_evidence(
        self,
        connection: Any,
        *,
        fact_id: uuid.UUID,
        evidence: list[dict[str, Any]],
    ) -> None:
        for entry in evidence:
            kind = str(entry.get("evidence_kind") or "source_ref")
            reference = str(entry.get("evidence_ref") or "").strip()
            if kind not in {"heartbeat_observation", "source_ref", "user_statement", "decision_record"}:
                raise ValueError(f"unsupported memory evidence_kind {kind!r}")
            if not reference:
                raise ValueError("memory evidence_ref is required")
            evidence_id = _uuid("memory-evidence", fact_id, kind, reference)
            await connection.execute(
                """
                insert into memory_fact_evidence (
                    evidence_id, fact_id, evidence_kind, evidence_ref,
                    source_url, excerpt, content_hash
                ) values ($1, $2, $3, $4, $5, $6, $7)
                on conflict (fact_id, evidence_kind, evidence_ref) do nothing
                """,
                evidence_id,
                fact_id,
                kind,
                reference,
                entry.get("source_url"),
                entry.get("excerpt"),
                entry.get("content_hash"),
            )

    async def confirm_memory_fact(
        self,
        *,
        fact_id: str,
        actor_ref: str,
        expected_revision: int,
        reason: str | None = None,
        idempotency_key: str | None = None,
    ) -> dict[str, Any]:
        return await self._transition_memory_fact(
            fact_id=fact_id,
            actor_ref=actor_ref,
            expected_revision=expected_revision,
            target_status="confirmed",
            event_type="confirmed",
            reason=reason,
            idempotency_key=idempotency_key,
        )

    async def dispute_memory_fact(
        self,
        *,
        fact_id: str,
        actor_ref: str,
        expected_revision: int,
        reason: str,
        evidence: list[dict[str, Any]] | None = None,
        idempotency_key: str | None = None,
    ) -> dict[str, Any]:
        if not reason.strip():
            raise ValueError("dispute reason is required")
        return await self._transition_memory_fact(
            fact_id=fact_id,
            actor_ref=actor_ref,
            expected_revision=expected_revision,
            target_status="disputed",
            event_type="disputed",
            reason=reason,
            evidence=evidence,
            idempotency_key=idempotency_key,
        )

    async def forget_memory_fact(
        self,
        *,
        fact_id: str,
        actor_ref: str,
        expected_revision: int,
        reason: str | None = None,
        idempotency_key: str | None = None,
    ) -> dict[str, Any]:
        return await self._transition_memory_fact(
            fact_id=fact_id,
            actor_ref=actor_ref,
            expected_revision=expected_revision,
            target_status="forgotten",
            event_type="forgotten",
            reason=reason,
            idempotency_key=idempotency_key,
        )

    async def _transition_memory_fact(
        self,
        *,
        fact_id: str,
        actor_ref: str,
        expected_revision: int,
        target_status: str,
        event_type: str,
        reason: str | None = None,
        evidence: list[dict[str, Any]] | None = None,
        idempotency_key: str | None = None,
    ) -> dict[str, Any]:
        self._require_ready()
        self._require_memory_workflow()
        if target_status not in {"confirmed", "disputed", "forgotten"}:
            raise ValueError("unsupported memory transition")
        fact_uuid = _parse_uuid(fact_id, "fact_id")
        actor_ref = actor_ref.strip()
        if not actor_ref:
            raise ValueError("actor_ref is required")
        event_key = idempotency_key or f"memory:{fact_uuid}:{event_type}:v{expected_revision}"
        async with self._pool.acquire() as connection:
            async with connection.transaction():
                existing_event = await self._memory_event_exists(connection, event_key)
                if existing_event == fact_uuid:
                    await self._memory_authorized(
                        connection, fact_id=fact_uuid, actor_ref=actor_ref
                    )
                    return await self._memory_result(connection, fact_uuid)
                fact = await self._memory_authorized(
                    connection, fact_id=fact_uuid, actor_ref=actor_ref, lock=True
                )
                actual_revision = int(fact["revision"])
                if actual_revision != int(expected_revision):
                    raise RuntimeError(
                        f"memory fact revision conflict: expected {expected_revision}, got {actual_revision}"
                    )
                if target_status == "confirmed" and fact["status"] not in {"proposed", "disputed"}:
                    raise RuntimeError("only proposed or disputed memory facts can be confirmed")
                if target_status == "disputed" and fact["status"] in {"forgotten", "superseded"}:
                    raise RuntimeError("forgotten or superseded memory facts cannot be disputed")
                new_revision = actual_revision + 1
                if target_status == "forgotten":
                    # Keep a non-content tombstone for audit while immediately
                    # excluding and purging the sensitive value/evidence. Walk
                    # both directions because a correction points backwards via
                    # supersedes_fact_id while a user may forget either side of
                    # the correction chain.
                    lineage_ids = await connection.fetch(
                        """
                        with recursive lineage(fact_id, supersedes_fact_id) as (
                            select fact_id, supersedes_fact_id
                              from memory_facts
                             where fact_id = $1
                            union
                            select f.fact_id, f.supersedes_fact_id
                              from memory_facts f
                              join lineage l
                                on f.supersedes_fact_id = l.fact_id
                                or l.supersedes_fact_id = f.fact_id
                             where f.owner_principal = (select owner_principal from memory_facts where fact_id = $1)
                               and f.namespace = (select namespace from memory_facts where fact_id = $1)
                               and f.scope_kind = (select scope_kind from memory_facts where fact_id = $1)
                               and f.scope_ref = (select scope_ref from memory_facts where fact_id = $1)
                        )
                        select fact_id from lineage
                        """,
                        fact_uuid,
                    )
                    lineage_ids = [row["fact_id"] for row in lineage_ids]
                    await connection.execute(
                        "delete from memory_fact_evidence where fact_id = any($1::uuid[])",
                        lineage_ids,
                    )
                    await connection.execute(
                        """
                        update memory_facts set status = 'forgotten', value = '{}'::jsonb,
                               canonical_text = '[forgotten]', revision = revision + 1,
                               confirmed_by_principal = $2, updated_at = now()
                        where fact_id = any($1::uuid[])
                        """,
                        lineage_ids,
                        actor_ref,
                    )
                    for lineage_id in lineage_ids:
                        lineage_event_key = (
                            event_key
                            if lineage_id == fact_uuid
                            else f"{event_key}:lineage:{lineage_id}"
                        )
                        await connection.execute(
                            """
                            insert into memory_fact_events (
                                event_id, fact_id, event_type, actor_ref, reason,
                                payload, idempotency_key
                            ) values ($1, $2, 'forgotten', $3, $4, $5::jsonb, $6)
                            on conflict (idempotency_key) do nothing
                            """,
                            _uuid("memory-event", lineage_event_key),
                            lineage_id,
                            actor_ref,
                            reason,
                            _json({"forgotten_from_fact_id": str(fact_uuid)}),
                            lineage_event_key,
                        )
                    # The selected fact's event was inserted above; skip the
                    # generic event insertion below for this transition.
                    return await self._memory_result(connection, fact_uuid)
                else:
                    await connection.execute(
                        """
                        update memory_facts set status = $2, revision = $3,
                               confirmed_by_principal = case when $2 = 'confirmed' then $4 else confirmed_by_principal end,
                               updated_at = now()
                        where fact_id = $1
                        """,
                        fact_uuid,
                        target_status,
                        new_revision,
                        actor_ref,
                    )
                    if evidence:
                        await self._insert_memory_evidence(
                            connection, fact_id=fact_uuid, evidence=evidence
                        )
                await connection.execute(
                    """
                    insert into memory_fact_events (
                        event_id, fact_id, event_type, actor_ref, reason,
                        payload, idempotency_key
                    ) values ($1, $2, $3, $4, $5, $6::jsonb, $7)
                    """,
                    _uuid("memory-event", event_key),
                    fact_uuid,
                    event_type,
                    actor_ref,
                    reason,
                    _json({"expected_revision": expected_revision, "new_revision": new_revision}),
                    event_key,
                )
                return await self._memory_result(connection, fact_uuid)

    async def correct_memory_fact(
        self,
        *,
        fact_id: str,
        actor_ref: str,
        expected_revision: int,
        canonical_text: str,
        value: Any,
        subject_key: str | None = None,
        predicate: str | None = None,
        sensitivity: str | None = None,
        evidence: list[dict[str, Any]] | None = None,
        reason: str | None = None,
        idempotency_key: str | None = None,
    ) -> dict[str, Any]:
        """Supersede a fact with a new auditable revision."""
        self._require_ready()
        self._require_memory_workflow()
        fact_uuid = _parse_uuid(fact_id, "fact_id")
        actor_ref = actor_ref.strip()
        canonical_text = canonical_text.strip()
        if not actor_ref or not canonical_text:
            raise ValueError("actor_ref and canonical_text are required")
        if sensitivity is not None and sensitivity not in _SENSITIVITIES:
            raise ValueError(f"unsupported memory sensitivity {sensitivity!r}")
        value_hash = hashlib.sha256(_json(value).encode()).hexdigest()
        event_key = idempotency_key or f"memory:{fact_uuid}:correct:{value_hash}"
        replacement_id = _uuid("memory-correction", fact_uuid, event_key)
        async with self._pool.acquire() as connection:
            async with connection.transaction():
                prior_event = await self._memory_event_exists(connection, event_key)
                if prior_event is not None:
                    replacement = await connection.fetchval(
                        "select fact_id from memory_fact_events where idempotency_key = $1",
                        f"{event_key}:replacement",
                    )
                    return await self._memory_result(
                        connection, replacement or prior_event
                    )
                fact = await self._memory_authorized(
                    connection, fact_id=fact_uuid, actor_ref=actor_ref, lock=True
                )
                if int(fact["revision"]) != int(expected_revision):
                    raise RuntimeError(
                        f"memory fact revision conflict: expected {expected_revision}, got {fact['revision']}"
                    )
                await connection.execute(
                    """
                    update memory_facts set status = 'superseded', revision = revision + 1,
                           updated_at = now() where fact_id = $1
                    """,
                    fact_uuid,
                )
                await connection.execute(
                    """
                    insert into memory_facts (
                        fact_id, owner_principal, namespace, scope_kind, scope_ref, subject_key,
                        predicate, value, canonical_text, status, sensitivity,
                        confidence, valid_from, valid_until, observed_at, revision,
                        supersedes_fact_id, proposed_by_principal, confirmed_by_principal
                    ) values ($1, $2, $3, $4, $5, $6, $7, $8::jsonb, $9, $10, $11,
                              $12, $13, $14, now(), $15, $16, $17, $18)
                    """,
                    replacement_id,
                    fact["owner_principal"],
                    fact["namespace"],
                    fact["scope_kind"],
                    fact["scope_ref"],
                    subject_key or fact["subject_key"],
                    predicate or fact["predicate"],
                    _json(value),
                    canonical_text,
                    "confirmed" if fact["status"] == "confirmed" else "proposed",
                    sensitivity or fact["sensitivity"],
                    fact["confidence"],
                    fact["valid_from"],
                    fact["valid_until"],
                    int(expected_revision) + 1,
                    fact_uuid,
                    actor_ref,
                    actor_ref if fact["status"] == "confirmed" else None,
                )
                if evidence:
                    await self._insert_memory_evidence(
                        connection, fact_id=replacement_id, evidence=evidence
                    )
                else:
                    raise ValueError("memory corrections require evidence")
                payload = {
                    "supersedes_fact_id": str(fact_uuid),
                    "expected_revision": expected_revision,
                    "new_revision": int(expected_revision) + 1,
                }
                for target in (fact_uuid, replacement_id):
                    await connection.execute(
                        """
                        insert into memory_fact_events (
                            event_id, fact_id, event_type, actor_ref, reason,
                            payload, idempotency_key
                        ) values ($1, $2, $3, $4, $5, $6::jsonb, $7)
                        """,
                        _uuid("memory-event", event_key, target),
                        target,
                        "superseded" if target == fact_uuid else "confirmed" if fact["status"] == "confirmed" else "proposed",
                        actor_ref,
                        reason,
                        _json(payload),
                        event_key if target == fact_uuid else f"{event_key}:replacement",
                    )
                return await self._memory_result(connection, replacement_id)

    async def supersede_memory_fact(self, **kwargs: Any) -> dict[str, Any]:
        """Explicit lifecycle name for a correction that supersedes a fact."""
        return await self.correct_memory_fact(**kwargs)

    async def mark_delivery_sent(
        self,
        *,
        delivery_id: str,
        provider_message_id: str,
        surfaced_item_ids: list[str],
    ) -> dict[str, Any]:
        self._require_ready()
        delivery_uuid = _parse_uuid(delivery_id, "delivery_id")
        delivery_run_id = await self._pool.fetchval(
            "select run_id from heartbeat_deliveries where delivery_id = $1",
            delivery_uuid,
        )
        if delivery_run_id is None:
            raise RuntimeError("heartbeat delivery not found")
        run = await self._require_run_executor(delivery_run_id)
        profile_uuid = _parse_uuid(str(run["profile_id"]), "profile_id")
        async with self._pool.acquire() as connection:
            async with connection.transaction():
                delivery = await connection.fetchrow(
                    """
                    update heartbeat_deliveries set status = 'sent', provider_message_id = $2,
                        sent_at = coalesce(sent_at, now())
                    where delivery_id = $1 returning *
                    """,
                    delivery_uuid,
                    provider_message_id,
                )
                if delivery is None:
                    raise RuntimeError("heartbeat delivery not found")
                for raw_item_id in surfaced_item_ids:
                    item_id = _parse_uuid(raw_item_id, "item_id")
                    item = await connection.fetchrow(
                        """
                        update heartbeat_items set last_surfaced_at = now()
                        where item_id = $1 and profile_id = $2 returning *
                        """,
                        item_id,
                        profile_uuid,
                    )
                    if item is None:
                        raise RuntimeError("heartbeat surfaced item not found")
                    event_key = f"delivery:{delivery_uuid}:{item_id}:v{item['version']}"
                    await connection.execute(
                        """
                        insert into heartbeat_item_events (
                            event_id, item_id, run_id, event_type, from_status, to_status,
                            item_version, actor_kind, actor_ref, idempotency_key
                        ) select $1, $2, run_id, 'surfaced', $3, $3, $4,
                                 'system', $5, $6
                          from heartbeat_deliveries where delivery_id = $7
                        on conflict (idempotency_key) do nothing
                        """,
                        _uuid("item-event", event_key),
                        item_id,
                        item["status"],
                        item["version"],
                        self.workflow_principal,
                        event_key,
                        delivery_uuid,
                    )
                await connection.execute(
                    """
                    update heartbeat_runs set surfaced_count = $2
                    where run_id = $1
                    """,
                    delivery["run_id"],
                    len(surfaced_item_ids),
                )
        return {"delivery_id": str(delivery_uuid), "status": "sent"}

    async def apply_action(
        self,
        *,
        token: str,
        actor_ref: str,
        provider_event_key: str,
    ) -> dict[str, Any]:
        self._require_ready()
        if not token or not actor_ref or not provider_event_key:
            raise ValueError("token, actor_ref, and provider_event_key are required")
        token_hash = hashlib.sha256(token.encode()).hexdigest()
        try:
            async with self._pool.acquire() as connection:
                result = await connection.fetchval(
                    "select heartbeat_consume_action($1, $2, $3)",
                    token_hash,
                    actor_ref,
                    provider_event_key,
                )
        except Exception as exc:
            message = str(exc)
            if "invalid, expired, or already used" in message or "already used" in message or "reviewer" in message:
                raise PermissionError(message) from exc
            if "changed after this action" in message:
                raise RuntimeError(message) from exc
            raise
        return _json_value(result) or {}

    async def apply_memory_action(
        self,
        *,
        token: str,
        actor_ref: str,
        provider_event_key: str,
        corrected_canonical_text: str | None = None,
        corrected_value: Any = None,
        reason: str | None = None,
    ) -> dict[str, Any]:
        self._require_ready()
        self._require_memory_workflow()
        if not token or not actor_ref or not provider_event_key:
            raise ValueError("token, actor_ref, and provider_event_key are required")
        try:
            async with self._pool.acquire() as connection:
                result = await connection.fetchval(
                    "select heartbeat_consume_memory_action($1,$2,$3,$4,$5::jsonb,$6)",
                    hashlib.sha256(token.encode()).hexdigest(),
                    actor_ref,
                    provider_event_key,
                    corrected_canonical_text,
                    _json(corrected_value) if corrected_value is not None else None,
                    reason,
                )
        except Exception as exc:
            message = str(exc)
            if "invalid" in message or "expired" in message or "reviewer" in message or "already used" in message:
                raise PermissionError(message) from exc
            raise RuntimeError(message) from exc
        return _json_value(result) or {}

    async def request_memory_correction(
        self,
        *,
        token: str,
        actor_ref: str,
        provider_event_key: str,
        fact_id: str | None = None,
        corrected_canonical_text: str | None = None,
        corrected_value: Any = None,
        reason: str | None = None,
    ) -> dict[str, Any]:
        # fact_id is accepted for Phai compatibility, but the opaque token is
        # the sole authority for the target fact.
        del fact_id
        return await self.apply_memory_action(
            token=token,
            actor_ref=actor_ref,
            provider_event_key=provider_event_key,
            corrected_canonical_text=corrected_canonical_text,
            corrected_value=corrected_value,
            reason=reason,
        )

    async def request_assignment(
        self, *, token: str, actor_ref: str, provider_event_key: str
    ) -> dict[str, Any]:
        return await self.apply_action(
            token=token, actor_ref=actor_ref, provider_event_key=provider_event_key
        )

    async def get_item(
        self,
        *,
        draft_grant: str | None = None,
        grant: str | None = None,
        item_id: str | None = None,
        item_version: int | None = None,
        expected_version: int | None = None,
    ) -> dict[str, Any]:
        self._require_ready()
        if self.workflow_name != "heartbeat_prepare_action":
            raise PermissionError("workflow is not authorized for draft reads")
        raw_grant = draft_grant or grant
        if not raw_grant:
            raise ValueError("draft_grant is required")
        try:
            async with self._pool.acquire() as connection:
                result = await connection.fetchval(
                    "select heartbeat_get_item($1)",
                hashlib.sha256(raw_grant.encode()).hexdigest(),
                )
        except Exception as exc:
            message = str(exc)
            if "invalid" in message or "authorized" in message:
                raise PermissionError(message) from exc
            raise RuntimeError(message) from exc
        item = _json_value(result) or {}
        if item_id is not None and str(item.get("item_id")) != str(item_id):
            raise PermissionError("draft grant item mismatch")
        requested_version = item_version if item_version is not None else expected_version
        if requested_version is not None and int(item.get("version")) != int(requested_version):
            raise RuntimeError("draft grant item version mismatch")
        return item

    async def put_draft_artifact(
        self,
        *,
        draft_grant: str | None = None,
        grant: str | None = None,
        item_id: str | None = None,
        item_version: int | None = None,
        draft: dict[str, Any] | None = None,
        content: dict[str, Any] | None = None,
    ) -> dict[str, Any]:
        self._require_ready()
        if self.workflow_name != "heartbeat_prepare_action":
            raise PermissionError("workflow is not authorized for draft writes")
        raw_grant = draft_grant or grant
        if not raw_grant or not item_id or item_version is None:
            raise ValueError("draft_grant, item_id, and item_version are required")
        payload = draft if draft is not None else content
        if not isinstance(payload, dict):
            raise ValueError("draft must be a JSON object")
        try:
            async with self._pool.acquire() as connection:
                result = await connection.fetchval(
                    "select heartbeat_put_draft_artifact($1,$2::uuid,$3,$4::jsonb)",
                hashlib.sha256(raw_grant.encode()).hexdigest(),
                    _parse_uuid(item_id, "item_id"),
                    int(item_version),
                    _json(payload),
                )
        except Exception as exc:
            message = str(exc)
            if "invalid" in message or "authorized" in message or "already used" in message:
                raise PermissionError(message) from exc
            raise RuntimeError(message) from exc
        return _json_value(result) or {}

    async def complete_run(
        self,
        *,
        run_id: str,
        status: str,
        outcome: str,
        error: Any = None,
    ) -> dict[str, Any]:
        self._require_ready()
        run_uuid = _parse_uuid(run_id, "run_id")
        if str(run_uuid) != str(self.workflow_run_id):
            raise PermissionError(
                "workflow may complete only its current heartbeat run"
            )
        await self._require_run_executor(run_uuid)
        if status not in {"completed", "partial", "failed", "cancelled"}:
            raise ValueError("heartbeat completion status is invalid")
        if outcome not in {"attention", "clean", "degraded", "none"}:
            raise ValueError("heartbeat outcome is invalid")
        row = await self._pool.fetchrow(
            """
            update heartbeat_runs set status = $2, outcome = $3, error = $4::jsonb,
                completed_at = now()
            where run_id = $1 returning *
            """,
            run_uuid,
            status,
            outcome,
            _json(error) if error is not None else None,
        )
        if row is None:
            raise RuntimeError("heartbeat run not found")
        return _row(row) or {}

    async def fail_current_run(self, error: BaseException) -> bool:
        """Best-effort terminal audit update used by the workflow host."""
        if self._pool is None or not self.workflow_principal:
            return False
        try:
            run_uuid = _parse_uuid(self.workflow_run_id, "workflow_run_id")
        except ValueError:
            return False
        result = await self._pool.execute(
            """
            update heartbeat_runs set status = 'failed', outcome = 'degraded',
                error = $2::jsonb, completed_at = now()
            where run_id = $1 and executor_principal_foreign_id = $3
              and status not in ('completed', 'partial', 'failed', 'cancelled')
            """,
            run_uuid,
            _json({"type": type(error).__name__, "message": str(error)[:1000]}),
            self.workflow_principal,
        )
        return result.endswith(" 1")
