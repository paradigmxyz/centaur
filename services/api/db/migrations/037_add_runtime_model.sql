-- migrate:up

ALTER TABLE sandbox_sessions
    ADD COLUMN IF NOT EXISTS model TEXT;

ALTER TABLE agent_runtime_assignments
    ADD COLUMN IF NOT EXISTS model TEXT;

-- migrate:down

ALTER TABLE agent_runtime_assignments
    DROP COLUMN IF EXISTS model;

ALTER TABLE sandbox_sessions
    DROP COLUMN IF EXISTS model;
