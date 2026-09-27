"""Extract durable memories from completed Slack turns and embed them."""

from __future__ import annotations

import hashlib
import json
import uuid
from dataclasses import dataclass
from typing import Any, Protocol

from api.metrics import increment_metric, set_gauge
from api.runtime_control import decode_jsonb
from api.workflow_engine import WorkflowContext
from openai import AsyncOpenAI, BadRequestError

WORKFLOW_NAME = "memory_generation"

DEFAULT_GENERATION_BATCH_SIZE = 250
DEFAULT_EMBEDDING_BATCH_SIZE = 25
DEFAULT_CONTEXT_MESSAGES = 40
DEFAULT_GENERATION_MAX_INPUT_CHARS = 32_000
DEFAULT_GENERATION_MODEL = "gpt-5.6-luna"
DEFAULT_EMBEDDING_MODEL = "text-embedding-3-small"
EMBEDDING_DIMENSIONS = 1_536
# Copied from paradigm/evals/evals/memory_save/prompts/v1.md.
SYSTEM_PROMPT = """Review one or more chronological conversation turns and generate memories that are likely to improve
future conversations.

Generate zero or more memories. Create a separate memory for each independently useful piece of
explicit, user-specific information. Preserve the order in which the information appears. Do not
combine unrelated information into one memory, and do not require the user to say "remember this."

Use this test: if the current task disappeared, is there a reasonable chance that retaining the
information could improve a plausible future conversation with the user in a new thread weeks or
months later? The information only needs to be useful in a related future conversation. It does not
need to matter across many topics or be certain to recur.

Valuable memories include stable preferences, standing instructions, decisions, durable identity and
relationship context, ongoing habits and goals, and circumstances likely to affect future advice. A
past event can be valuable when it reveals a durable preference, relationship, accomplishment, or
likely follow-up. Favor saving explicit, durable, user-centered information when it fits one of these
categories, even when the user mentions it as an aside, says it is recent, or asks about something
else in the same turn. Do not reject information merely because it would help only with a narrow
class of future conversations. Save a possession or product detail only when it is likely to matter
for future recommendations, compatibility, maintenance, or troubleshooting.

Do not generate memories for ordinary questions, speculative inferences, hypothetical scenarios,
quoted information about third parties, instructions limited to the current task, temporary states,
or incidental trivia unlikely to help in a future conversation. Do not save ordinary inventories,
one-off purchases, or isolated product details merely because they are explicit. Minor transaction
amounts, counts, dates, and durations are usually incidental unless they are operationally useful for
an ongoing goal or decision. A detail is not incidental merely because it is secondary to the user's
current request. A durable relationship fact centered on the user can be valuable, but a standalone
fact about another person is not.

Never save credentials, authentication data, financial account identifiers, or similarly dangerous
secrets. Honor any explicit request not to save something.

When later turns update the same information, generate only the latest state. Write each memory as a
concise, standalone statement. Preserve names, constraints, and scope, but remove conversational
filler. Do not infer details the user did not state."""

OUTPUT_INSTRUCTIONS = """Return an empty candidates list when no memory should be generated. Every candidate must contain
the memory content and cite one supplied source_execution_id."""

CANDIDATE_SCHEMA = {
    "type": "object",
    "properties": {
        "candidates": {
            "type": "array",
            "maxItems": 10,
            "items": {
                "type": "object",
                "properties": {
                    "content": {"type": "string"},
                    "source_execution_id": {"type": "string"},
                },
                "required": ["content", "source_execution_id"],
                "additionalProperties": False,
            },
        }
    },
    "required": ["candidates"],
    "additionalProperties": False,
}


class PermanentThreadError(Exception):
    """A thread-specific failure that retrying cannot repair."""


class MissingExecutionMaterialError(PermanentThreadError):
    pass


class GenerationInputTooLargeError(PermanentThreadError):
    pass


@dataclass(frozen=True)
class MemoryOwner:
    scope: str
    owner_id: str


class OpenAIClient(Protocol):
    responses: Any
    embeddings: Any


def _client() -> OpenAIClient:
    return AsyncOpenAI()


def _clean_string(value: Any) -> str:
    return str(value).strip() if value is not None else ""


def _metadata(value: Any) -> dict[str, Any]:
    decoded = decode_jsonb(value, {})
    return decoded if isinstance(decoded, dict) else {}


def _owner_for_thread(thread: Any) -> MemoryOwner | None:
    metadata = _metadata(thread["session_metadata"])
    if metadata.get("platform") != "slack" or metadata.get("source") != "slackbotv2":
        return None
    if not _clean_string(thread["iron_control_principal"]):
        return None

    channel_id = _clean_string(metadata.get("slack_channel_id"))
    if not channel_id or channel_id not in _clean_string(thread["thread_key"]).split(
        ":"
    ):
        return None
    conversation_type = _clean_string(thread.get("conversation_type", ""))
    if conversation_type == "mpim":
        return None

    if channel_id.startswith("D"):
        user_id = _clean_string(metadata.get("slack_user_id"))
        return MemoryOwner("user", user_id) if user_id else None
    if channel_id.startswith("G"):
        # A G-prefixed conversation may be either an MPIM or a private channel.
        # Only synced private channels have unambiguous channel ownership.
        if conversation_type != "private_channel":
            return None
        return MemoryOwner("channel", channel_id)
    if channel_id.startswith("C"):
        return MemoryOwner("channel", channel_id)
    return None


def _thread_step_name(thread_key: str) -> str:
    digest = hashlib.sha256(thread_key.encode("utf-8")).hexdigest()[:20]
    return f"generate_thread_{digest}"


async def _load_executions(pool: Any, batch_size: int) -> list[dict[str, Any]]:
    rows = await pool.fetch(
        "WITH eligible AS MATERIALIZED ("
        "  SELECT e.execution_id, e.thread_key, e.completed_at "
        "  FROM session_executions e "
        "  JOIN sessions s ON s.thread_key = e.thread_key "
        "  CROSS JOIN memory_generation_cursor cursor "
        "  WHERE e.status = 'completed' "
        "    AND e.completed_at <= NOW() - INTERVAL '2 minutes' "
        "    AND (e.completed_at, e.execution_id) > "
        "        (cursor.completed_at, cursor.execution_id) "
        "    AND e.thread_key LIKE 'slack:%' "
        "    AND s.metadata->>'platform' = 'slack' "
        "    AND s.metadata->>'source' = 'slackbotv2' "
        "    AND EXISTS ("
        "      SELECT 1 FROM session_events terminal "
        "      WHERE terminal.execution_id = e.execution_id "
        "        AND terminal.event_type = 'session.execution_completed' "
        "        AND NULLIF(BTRIM(terminal.payload->>'result_text'), '') IS NOT NULL"
        "    )"
        "), thread_starts AS ("
        "  SELECT DISTINCT ON (thread_key) thread_key, completed_at, execution_id "
        "  FROM eligible "
        "  ORDER BY thread_key, completed_at, execution_id"
        "), cutoff AS ("
        "  SELECT completed_at, execution_id FROM thread_starts "
        "  ORDER BY completed_at, execution_id OFFSET $1 LIMIT 1"
        ") SELECT execution_id, thread_key FROM eligible "
        "WHERE NOT EXISTS (SELECT 1 FROM cutoff) "
        "   OR (completed_at, execution_id) < "
        "      (SELECT completed_at, execution_id FROM cutoff) "
        "ORDER BY completed_at, execution_id",
        batch_size,
    )
    return [dict(row) for row in rows]


async def _advance_cursor(pool: Any, execution_id: str) -> None:
    await pool.execute(
        "UPDATE memory_generation_cursor cursor "
        "SET completed_at = e.completed_at, execution_id = e.execution_id "
        "FROM session_executions e "
        "WHERE cursor.singleton AND e.execution_id = $1 "
        "  AND (e.completed_at, e.execution_id) > (cursor.completed_at, cursor.execution_id)",
        execution_id,
    )


def _text_parts(parts_value: Any) -> str:
    parts = decode_jsonb(parts_value, [])
    if not isinstance(parts, list):
        return ""
    return "\n".join(
        text
        for part in parts
        if isinstance(part, dict)
        and part.get("type") == "text"
        and (text := _clean_string(part.get("text")))
    )


async def _load_thread_material(
    connection: Any, executions: list[Any]
) -> dict[str, Any]:
    execution_ids = [_clean_string(item["execution_id"]) for item in executions]
    rows = await connection.fetch(
        "SELECT e.execution_id, e.thread_key, e.completed_at, e.metadata AS execution_metadata, "
        "  terminal.payload->>'result_text' AS result_text, "
        "  s.metadata AS session_metadata, s.iron_control_principal, "
        "  COALESCE(("
        "    SELECT conversation_type FROM slack_private_sync_conversations conversation "
        "    WHERE conversation.conversation_id = s.metadata->>'slack_channel_id' "
        "      AND conversation.home_team_id = COALESCE("
        "        NULLIF(s.metadata->>'slack_home_team_id', ''), "
        "        NULLIF(s.metadata->>'slack_team_id', '')"
        "      ) LIMIT 1"
        "  ), '') AS conversation_type "
        "FROM session_executions e "
        "JOIN sessions s ON s.thread_key = e.thread_key "
        "JOIN LATERAL ("
        "  SELECT payload FROM session_events "
        "  WHERE execution_id = e.execution_id "
        "    AND event_type = 'session.execution_completed' "
        "  ORDER BY event_id DESC LIMIT 1"
        ") terminal ON TRUE "
        "WHERE e.execution_id = ANY($1::text[]) "
        "ORDER BY e.completed_at, e.execution_id",
        execution_ids,
    )
    if not rows:
        raise MissingExecutionMaterialError("memory generation executions disappeared")

    thread_key = _clean_string(rows[0]["thread_key"])
    messages = await connection.fetch(
        "SELECT message_id, client_message_id, parts "
        "FROM session_messages "
        "WHERE thread_key = $1 AND role = 'user' AND created_at <= $2 "
        "ORDER BY created_at DESC, message_id DESC LIMIT $3",
        thread_key,
        max(row["completed_at"] for row in rows),
        DEFAULT_CONTEXT_MESSAGES,
    )
    return {
        "thread": {
            "thread_key": thread_key,
            "session_metadata": rows[0]["session_metadata"],
            "iron_control_principal": rows[0]["iron_control_principal"],
            "conversation_type": rows[0]["conversation_type"],
        },
        "executions": [
            {
                "source_execution_id": _clean_string(row["execution_id"]),
                "creator_user_id": _clean_string(
                    _metadata(row["execution_metadata"]).get("slack_user_id")
                ),
                "assistant_final": _clean_string(row["result_text"]),
            }
            for row in rows
        ],
        "preceding_user_messages": [
            {
                "message_id": _clean_string(message["client_message_id"])
                or _clean_string(message["message_id"]),
                "text": _text_parts(message["parts"]),
            }
            for message in reversed(messages)
            if _text_parts(message["parts"])
        ],
    }


def _generation_input(
    material: dict[str, Any],
    max_chars: int = DEFAULT_GENERATION_MAX_INPUT_CHARS,
) -> str:
    payload = {
        "executions": [dict(item) for item in material["executions"]],
        "preceding_user_messages": [
            dict(item) for item in material["preceding_user_messages"]
        ],
    }

    def encode() -> str:
        return json.dumps(payload, ensure_ascii=False, separators=(",", ":"))

    encoded = encode()
    text_fields = [
        (execution, "assistant_final") for execution in payload["executions"]
    ] + [(message, "text") for message in payload["preceding_user_messages"]]
    for item, field in text_fields:
        while len(encoded) > max_chars and (value := _clean_string(item.get(field))):
            trim_chars = min(len(value), max(1, len(encoded) - max_chars))
            item[field] = value[:-trim_chars]
            encoded = encode()
    if len(encoded) > max_chars:
        raise GenerationInputTooLargeError(
            "memory generation metadata exceeds input limit"
        )
    return encoded


async def _generate_candidates(
    client: OpenAIClient,
    *,
    model: str,
    material: dict[str, Any],
) -> list[Any]:
    response = await client.responses.create(
        model=model,
        input=[
            {"role": "system", "content": SYSTEM_PROMPT},
            {"role": "system", "content": OUTPUT_INSTRUCTIONS},
            {
                "role": "user",
                "content": _generation_input(material),
            },
        ],
        text={
            "format": {
                "type": "json_schema",
                "name": "memory_candidates",
                "strict": True,
                "schema": CANDIDATE_SCHEMA,
            }
        },
    )
    output_text = _clean_string(getattr(response, "output_text", ""))
    if not output_text:
        raise RuntimeError("OpenAI returned no structured memory candidates")
    parsed = json.loads(output_text)
    candidates = parsed.get("candidates") if isinstance(parsed, dict) else None
    if not isinstance(candidates, list):
        raise TypeError("structured memory response is missing candidates")
    return candidates


def _content_hash(content: str) -> str:
    return hashlib.sha256(content.encode("utf-8")).hexdigest()


def _validate_candidate(
    candidate: Any,
    *,
    executions_by_id: dict[str, dict[str, str]],
    seen_hashes: set[str],
) -> tuple[dict[str, str] | None, str]:
    if not isinstance(candidate, dict):
        return None, "invalid_shape"

    content = " ".join(_clean_string(candidate.get("content")).split())
    if not content or len(content) > 1_500:
        return None, "invalid_length"
    source_execution_id = _clean_string(candidate.get("source_execution_id"))
    source = executions_by_id.get(source_execution_id)
    if not source or not _clean_string(source.get("creator_user_id")):
        return None, "invalid_source"

    digest = _content_hash(content)
    if digest in seen_hashes:
        return None, "duplicate"
    seen_hashes.add(digest)
    return {
        "content": content,
        "content_hash": digest,
        "source_execution_id": source_execution_id,
        "creator_user_id": source["creator_user_id"],
    }, "accepted"


async def _store_candidates(
    connection: Any,
    *,
    owner: MemoryOwner,
    material: dict[str, Any],
    candidates: list[Any],
) -> dict[str, int]:
    executions_by_id = {
        item["source_execution_id"]: item for item in material["executions"]
    }
    seen_hashes: set[str] = set()
    rejected = 0
    created = 0
    for raw_candidate in candidates:
        candidate, reason = _validate_candidate(
            raw_candidate,
            executions_by_id=executions_by_id,
            seen_hashes=seen_hashes,
        )
        if candidate is None:
            rejected += 1
            increment_metric(
                "memory_generation_candidates_rejected_total", 1, reason=reason
            )
            continue

        inserted = await connection.fetchval(
            "INSERT INTO memories ("
            "  id, content, content_hash, scope, owner_id, creator_user_id, "
            "  origin_thread_key, source_execution_id"
            ") VALUES ("
            "  $1::uuid, $2, $3, $4, $5, $6, $7, $8"
            ") ON CONFLICT DO NOTHING RETURNING id::text",
            str(uuid.uuid4()),
            candidate["content"],
            candidate["content_hash"],
            owner.scope,
            owner.owner_id,
            candidate["creator_user_id"],
            material["thread"]["thread_key"],
            candidate["source_execution_id"],
        )
        if not inserted:
            rejected += 1
            continue
        created += 1
    return {"created": created, "rejected": rejected}


async def _process_thread(
    pool: Any,
    *,
    executions: list[Any],
    generation_model: str,
    client: OpenAIClient,
) -> dict[str, int]:
    async with pool.acquire() as connection:
        material = await _load_thread_material(connection, executions)
        owner = _owner_for_thread(material["thread"])
        if owner is None:
            return {
                "created": 0,
                "rejected": 0,
                "skipped": len(executions),
            }
    candidates = await _generate_candidates(
        client, model=generation_model, material=material
    )
    async with pool.acquire() as connection, connection.transaction():
        result = await _store_candidates(
            connection,
            owner=owner,
            material=material,
            candidates=candidates,
        )
    return {**result, "skipped": 0}


async def _process_thread_safely(
    pool: Any,
    *,
    executions: list[Any],
    generation_model: str,
    client: OpenAIClient,
    ctx: WorkflowContext,
    step_name: str,
) -> dict[str, int]:
    try:
        result = await _process_thread(
            pool,
            executions=executions,
            generation_model=generation_model,
            client=client,
        )
    except (BadRequestError, PermanentThreadError) as error:
        failed = len(executions)
        increment_metric(
            "memory_generation_threads_failed_total",
            1,
            error_type=type(error).__name__,
        )
        ctx.log(
            "memory_generation_thread_failed",
            step=step_name,
            executions=failed,
            error_type=type(error).__name__,
        )
        return {"created": 0, "rejected": 0, "skipped": 0, "failed": failed}
    return {**result, "failed": 0}


async def _generate_embeddings(
    client: OpenAIClient, *, model: str, inputs: list[str]
) -> list[list[float]]:
    response = await client.embeddings.create(
        model=model,
        input=inputs,
        dimensions=EMBEDDING_DIMENSIONS,
        encoding_format="float",
    )
    by_index = {item.index: item.embedding for item in response.data}
    if set(by_index) != set(range(len(inputs))):
        raise RuntimeError("OpenAI returned an incomplete embedding batch")
    return [by_index[index] for index in range(len(inputs))]


async def _embed_pending(
    pool: Any,
    *,
    batch_size: int,
    model: str,
    client: OpenAIClient,
    ctx: WorkflowContext,
) -> dict[str, int]:
    rows = await pool.fetch(
        "SELECT id::text AS id, content FROM memories "
        "WHERE deleted_at IS NULL AND embedding IS NULL "
        "ORDER BY created_at, id LIMIT $1",
        batch_size,
    )
    if not rows:
        return {"embedded": 0, "embedding_failed": 0}

    try:
        embeddings = await _generate_embeddings(
            client, model=model, inputs=[_clean_string(row["content"]) for row in rows]
        )
    except Exception as error:  # noqa: BLE001
        increment_metric(
            "memory_embedding_failures_total",
            len(rows),
            error_type=type(error).__name__,
        )
        ctx.log(
            "memory_embedding_batch_failed",
            memories=len(rows),
            error_type=type(error).__name__,
        )
        return {"embedded": 0, "embedding_failed": len(rows)}
    embedded = 0
    for row, embedding in zip(rows, embeddings, strict=True):
        result = await pool.execute(
            "UPDATE memories SET embedding = $2::vector, embedding_model = $3, "
            "  updated_at = NOW() "
            "WHERE id = $1::uuid AND deleted_at IS NULL AND embedding IS NULL",
            _clean_string(row["id"]),
            json.dumps(embedding, separators=(",", ":")),
            model,
        )
        embedded += int(result == "UPDATE 1")
    increment_metric("memory_embeddings_generated_total", embedded)
    return {"embedded": embedded, "embedding_failed": 0}


async def _emit_pending_embedding_age(pool: Any) -> None:
    age = await pool.fetchval(
        "SELECT COALESCE(EXTRACT(EPOCH FROM (NOW() - MIN(created_at))), 0)::double precision "
        "FROM memories WHERE deleted_at IS NULL AND embedding IS NULL"
    )
    set_gauge("memory_pending_embedding_age_seconds", max(float(age or 0), 0.0))


async def handler(_inp: Any, ctx: WorkflowContext) -> dict[str, Any]:
    if ctx._pool is None:
        raise RuntimeError("memory generation requires DATABASE_URL")

    executions = await ctx.step(
        "load_completed_slack_executions",
        lambda: _load_executions(ctx._pool, DEFAULT_GENERATION_BATCH_SIZE),
    )
    by_thread: dict[str, list[Any]] = {}
    for execution in executions:
        by_thread.setdefault(_clean_string(execution["thread_key"]), []).append(
            execution
        )

    client = _client()
    totals = {"created": 0, "rejected": 0, "skipped": 0, "failed": 0}
    for thread_key, thread_executions in by_thread.items():
        step_name = _thread_step_name(thread_key)
        result = await ctx.step(
            step_name,
            lambda rows=thread_executions, name=step_name: _process_thread_safely(
                ctx._pool,
                executions=rows,
                generation_model=DEFAULT_GENERATION_MODEL,
                client=client,
                ctx=ctx,
                step_name=name,
            ),
        )
        for key in totals:
            totals[key] += int(result[key])

    if executions:
        await ctx.step(
            "advance_memory_generation_cursor",
            lambda: _advance_cursor(
                ctx._pool, _clean_string(executions[-1]["execution_id"])
            ),
        )
    embedding_result = await ctx.step(
        "embed_null_memories",
        lambda: _embed_pending(
            ctx._pool,
            batch_size=DEFAULT_EMBEDDING_BATCH_SIZE,
            model=DEFAULT_EMBEDDING_MODEL,
            client=client,
            ctx=ctx,
        ),
    )
    await _emit_pending_embedding_age(ctx._pool)
    next_run = None
    if (
        len(by_thread) == DEFAULT_GENERATION_BATCH_SIZE
        or embedding_result["embedded"] == DEFAULT_EMBEDDING_BATCH_SIZE
    ):
        next_run = await ctx.start_workflow(
            WORKFLOW_NAME,
            {"source": "memory_generation_continuation"},
            idempotency_key=f"{WORKFLOW_NAME}:{ctx.run_id}:next",
        )
    result = {
        "status": "completed",
        "processed": len(executions),
        "threads": len(by_thread),
        **totals,
        **embedding_result,
        "requeued": next_run is not None,
        "generation_model": DEFAULT_GENERATION_MODEL,
        "embedding_model": DEFAULT_EMBEDDING_MODEL,
    }
    if next_run is not None:
        result["next_run"] = next_run
    ctx.log("memory_generation_completed", **result)
    return result
