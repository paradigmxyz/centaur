-- migrate:up

CREATE TABLE IF NOT EXISTS google_drive_sync_runs (
    run_id             TEXT PRIMARY KEY,
    workflow_run_id    TEXT,
    mode               TEXT NOT NULL DEFAULT 'incremental',
    status             TEXT NOT NULL,
    scopes_requested   JSONB NOT NULL DEFAULT '[]'::jsonb,
    scopes_synced      JSONB NOT NULL DEFAULT '[]'::jsonb,
    scopes_failed      JSONB NOT NULL DEFAULT '[]'::jsonb,
    files_seen         INTEGER NOT NULL DEFAULT 0,
    files_upserted     INTEGER NOT NULL DEFAULT 0,
    docs_fetched       INTEGER NOT NULL DEFAULT 0,
    docs_upserted      INTEGER NOT NULL DEFAULT 0,
    started_at         TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    finished_at        TIMESTAMPTZ,
    error_text         TEXT NOT NULL DEFAULT '',
    metadata           JSONB NOT NULL DEFAULT '{}'::jsonb
);

CREATE INDEX IF NOT EXISTS idx_google_drive_sync_runs_started
    ON google_drive_sync_runs (started_at DESC);

CREATE TABLE IF NOT EXISTS google_drive_sync_files (
    file_id                  TEXT PRIMARY KEY,
    name                     TEXT NOT NULL DEFAULT '',
    mime_type                TEXT NOT NULL DEFAULT '',
    web_view_link            TEXT NOT NULL DEFAULT '',
    drive_id                 TEXT NOT NULL DEFAULT '',
    parent_ids               JSONB NOT NULL DEFAULT '[]'::jsonb,
    owners                   JSONB NOT NULL DEFAULT '[]'::jsonb,
    last_modifying_user      JSONB NOT NULL DEFAULT '{}'::jsonb,
    trashed                  BOOLEAN NOT NULL DEFAULT FALSE,
    source_created_at        TIMESTAMPTZ,
    source_modified_at       TIMESTAMPTZ,
    text_content             TEXT NOT NULL DEFAULT '',
    text_hash                TEXT NOT NULL DEFAULT '',
    raw_payload              JSONB NOT NULL DEFAULT '{}'::jsonb,
    source_run_id            TEXT REFERENCES google_drive_sync_runs(run_id) ON DELETE SET NULL,
    first_seen_at            TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    last_seen_at             TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    last_content_synced_at   TIMESTAMPTZ,
    last_error               TEXT NOT NULL DEFAULT '',
    updated_at               TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_google_drive_sync_files_modified
    ON google_drive_sync_files (source_modified_at DESC);

CREATE INDEX IF NOT EXISTS idx_google_drive_sync_files_text
    ON google_drive_sync_files
    USING GIN (to_tsvector('english', coalesce(text_content, '')));

CREATE INDEX IF NOT EXISTS idx_google_drive_sync_files_parents
    ON google_drive_sync_files USING GIN (parent_ids);

CREATE TABLE IF NOT EXISTS google_drive_sync_checkpoints (
    scope_id          TEXT PRIMARY KEY,
    watermark_time    TIMESTAMPTZ,
    last_run_id       TEXT REFERENCES google_drive_sync_runs(run_id) ON DELETE SET NULL,
    last_success_at   TIMESTAMPTZ,
    last_error        TEXT NOT NULL DEFAULT '',
    created_at        TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at        TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- migrate:down

DROP TABLE IF EXISTS google_drive_sync_checkpoints;
DROP INDEX IF EXISTS idx_google_drive_sync_files_parents;
DROP INDEX IF EXISTS idx_google_drive_sync_files_text;
DROP INDEX IF EXISTS idx_google_drive_sync_files_modified;
DROP TABLE IF EXISTS google_drive_sync_files;
DROP INDEX IF EXISTS idx_google_drive_sync_runs_started;
DROP TABLE IF EXISTS google_drive_sync_runs;
