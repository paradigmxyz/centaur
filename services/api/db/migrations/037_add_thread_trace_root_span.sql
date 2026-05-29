-- migrate:up
ALTER TABLE thread_traces
    ADD COLUMN IF NOT EXISTS root_span_id TEXT;

-- migrate:down
ALTER TABLE thread_traces
    DROP COLUMN IF EXISTS root_span_id;
