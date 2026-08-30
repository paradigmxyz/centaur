from __future__ import annotations

import datetime as dt
import hashlib
import json
import secrets
import uuid
from typing import Any

_ID_NAMESPACE = uuid.UUID("ba55d079-050d-496d-a2a0-9c4f96e64c4f")
_SCOPES = {"organization", "team", "personal"}
_SENSITIVITIES = {"public", "internal", "confidential", "restricted"}
_ACTIONS = {"approve", "assign", "park", "snooze", "not_useful", "prepare_draft"}
_ARTIFACT_KINDS = {
    "source_input",
    "source_error",
    "ranked_candidates",
    "synthesis_output",
    "delivery_preview",
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
            raise RuntimeError("Heartbeat requires DATABASE_URL in the workflow host")
        if not self.workflow_principal:
            raise RuntimeError("Heartbeat requires WORKFLOW_PRINCIPAL")

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
        async with self._pool.acquire() as connection:
            async with connection.transaction():
                await connection.execute(
                    "delete from heartbeat_profile_grants where profile_id = $1 and permission = 'review'",
                    profile_id,
                )
                for reviewer in reviewer_refs:
                    await connection.execute(
                        """
                        insert into heartbeat_profile_grants (
                            profile_id, subject_kind, subject_ref, permission, granted_by_principal
                        ) values ($1, 'principal', $2, 'review', $3)
                        on conflict do nothing
                        """,
                        profile_id,
                        reviewer,
                        self.workflow_principal,
                    )
                await connection.execute(
                    """
                    insert into heartbeat_profile_grants (
                        profile_id, subject_kind, subject_ref, permission, granted_by_principal
                    ) values ($1, 'principal', $2, 'operate', $2)
                    on conflict do nothing
                    """,
                    profile_id,
                    self.workflow_principal,
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
                            reopen = old_status in {"resolved", "dismissed", "stale"}
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
                            fact_id, namespace, scope_kind, scope_ref, subject_key,
                            predicate, value, canonical_text, status, sensitivity,
                            valid_until, observed_at, proposed_by_principal
                        ) values ($1, $2, $3, $4, $5, $6, $7::jsonb, $8,
                                  'proposed', $9, $10, now(), $11)
                        on conflict (fact_id) do nothing
                        """,
                        fact_id,
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
            "memory_proposals": len(proposals),
        }

    async def prepare_delivery(
        self,
        *,
        run_id: str,
        destination_kind: str,
        destination_ref: str,
        rendered_payload: dict[str, Any],
        item_actions: list[dict[str, Any]],
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
        if len(item_actions) > 100:
            raise ValueError("heartbeat delivery is limited to 100 actions")
        delivery_id = _uuid("delivery", run_uuid, destination_kind, destination_ref)
        client_message_id = f"heartbeat:{run_uuid}:{destination_kind}:{destination_ref}"
        tokens: list[dict[str, Any]] = []
        async with self._pool.acquire() as connection:
            async with connection.transaction():
                await connection.execute(
                    """
                    insert into heartbeat_deliveries (
                        delivery_id, run_id, destination_kind, destination_ref,
                        status, client_message_id, rendered_payload
                    ) values ($1, $2, $3, $4, 'pending', $5, $6::jsonb)
                    on conflict (client_message_id) do nothing
                    """,
                    delivery_id,
                    run_uuid,
                    destination_kind,
                    destination_ref,
                    client_message_id,
                    _json(rendered_payload),
                )
                for item_action in item_actions:
                    action = str(item_action.get("action") or "")
                    if action not in _ACTIONS:
                        raise ValueError(f"unsupported heartbeat action {action!r}")
                    token = secrets.token_urlsafe(24)
                    token_hash = hashlib.sha256(token.encode()).hexdigest()
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
                await connection.execute(
                    "update heartbeat_runs set status = 'delivering' where run_id = $1",
                    run_uuid,
                )
        return {
            "delivery_id": str(delivery_id),
            "client_message_id": client_message_id,
            "tokens": tokens,
        }

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
        async with self._pool.acquire() as connection:
            async with connection.transaction():
                action = await connection.fetchrow(
                    """
                    select t.*, d.run_id, r.profile_id
                    from heartbeat_action_tokens t
                    join heartbeat_deliveries d on d.delivery_id = t.delivery_id
                    join heartbeat_runs r on r.run_id = d.run_id
                    where t.token_hash = $1 for update
                    """,
                    token_hash,
                )
                if (
                    action is None
                    or action["consumed_at"] is not None
                    or action["expires_at"] <= dt.datetime.now(dt.UTC)
                ):
                    raise PermissionError(
                        "heartbeat action token is invalid, expired, or already used"
                    )
                allowed = await connection.fetchval(
                    """
                    select exists(
                        select 1 from heartbeat_profile_grants
                        where profile_id = $1 and permission in ('review', 'admin')
                          and subject_kind = 'principal' and subject_ref = $2
                    )
                    """,
                    action["profile_id"],
                    actor_ref,
                )
                if not allowed:
                    raise PermissionError("actor is not a heartbeat reviewer")
                item = await connection.fetchrow(
                    "select * from heartbeat_items where item_id = $1 for update",
                    action["item_id"],
                )
                if item is None or int(item["version"]) != int(action["item_version"]):
                    raise RuntimeError(
                        "heartbeat item changed after this action was rendered"
                    )
                action_name = str(action["action"])
                to_status = str(item["status"])
                disposition = item["disposition"]
                snooze_until = item["snooze_until"]
                if action_name in {"approve", "assign", "park"}:
                    to_status, disposition = "resolved", action_name
                elif action_name == "snooze":
                    payload = _json_value(action["payload"]) or {}
                    snooze_until = _parse_time(payload.get("until")) or (
                        dt.datetime.now(dt.UTC) + dt.timedelta(days=1)
                    )
                    to_status, disposition = "snoozed", "snooze"
                elif action_name == "not_useful":
                    to_status, disposition = "dismissed", "not_useful"
                elif action_name != "prepare_draft":
                    raise ValueError(f"unsupported heartbeat action {action_name!r}")
                new_version = int(item["version"]) + 1
                await connection.execute(
                    """
                    update heartbeat_items set status = $2, disposition = $3,
                        snooze_until = $4,
                        resolved_at = case when $2 in ('resolved', 'dismissed') then now() else null end,
                        version = $5
                    where item_id = $1
                    """,
                    item["item_id"],
                    to_status,
                    disposition,
                    snooze_until,
                    new_version,
                )
                await connection.execute(
                    """
                    update heartbeat_action_tokens set consumed_at = now(), consumed_by_principal = $2
                    where token_hash = $1
                    """,
                    token_hash,
                    actor_ref,
                )
                event_key = f"slack:{provider_event_key}"
                await connection.execute(
                    """
                    insert into heartbeat_item_events (
                        event_id, item_id, run_id, event_type, from_status, to_status,
                        item_version, actor_kind, actor_ref, payload, idempotency_key
                    ) values ($1, $2, $3, $4, $5, $6, $7, 'human', $8,
                              $9::jsonb, $10)
                    on conflict (idempotency_key) do nothing
                    """,
                    _uuid("item-event", event_key),
                    item["item_id"],
                    action["run_id"],
                    action_name,
                    item["status"],
                    to_status,
                    new_version,
                    actor_ref,
                    _json({"delivery_id": str(action["delivery_id"])}),
                    event_key,
                )
        return {
            "item_id": str(item["item_id"]),
            "action": action_name,
            "status": to_status,
            "version": new_version,
        }

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
