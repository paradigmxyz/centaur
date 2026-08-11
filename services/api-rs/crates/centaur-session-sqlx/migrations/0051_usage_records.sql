-- Per-turn usage observations. First populated by workflow-tier agent turns
-- (`ctx.agent_turn` in the python workflow host), which previously wrote no
-- usage record at all -- unlike the session-tier harness protocol, whose
-- `turn.completed` events already carry a `usage` object.
create table if not exists usage_records (
    id bigserial primary key,
    execution_id text not null,
    thread_key text not null,
    harness text not null,
    model text,
    input_tokens bigint not null,
    output_tokens bigint not null,
    recorded_at timestamptz not null default now()
);

create index if not exists idx_usage_records_execution_id
    on usage_records (execution_id);
