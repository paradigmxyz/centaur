alter table sessions
    add column if not exists sandbox_tool_allowlist jsonb;
