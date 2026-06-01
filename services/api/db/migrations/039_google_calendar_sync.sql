-- migrate:up

CREATE TABLE IF NOT EXISTS google_calendar_sync_runs (
    run_id              TEXT PRIMARY KEY,
    workflow_run_id     TEXT,
    mode                TEXT NOT NULL DEFAULT 'incremental',
    status              TEXT NOT NULL,
    calendars_requested JSONB NOT NULL DEFAULT '[]'::jsonb,
    calendars_synced    JSONB NOT NULL DEFAULT '[]'::jsonb,
    calendars_failed    JSONB NOT NULL DEFAULT '[]'::jsonb,
    calendars_seen      INTEGER NOT NULL DEFAULT 0,
    calendars_upserted  INTEGER NOT NULL DEFAULT 0,
    events_seen         INTEGER NOT NULL DEFAULT 0,
    events_upserted     INTEGER NOT NULL DEFAULT 0,
    events_cancelled    INTEGER NOT NULL DEFAULT 0,
    started_at          TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    finished_at         TIMESTAMPTZ,
    error_text          TEXT NOT NULL DEFAULT '',
    metadata            JSONB NOT NULL DEFAULT '{}'::jsonb
);

CREATE INDEX IF NOT EXISTS idx_google_calendar_sync_runs_started
    ON google_calendar_sync_runs (started_at DESC);

CREATE TABLE IF NOT EXISTS google_calendar_sync_calendars (
    calendar_id      TEXT PRIMARY KEY,
    summary          TEXT NOT NULL DEFAULT '',
    description      TEXT NOT NULL DEFAULT '',
    location         TEXT NOT NULL DEFAULT '',
    time_zone        TEXT NOT NULL DEFAULT '',
    access_role      TEXT NOT NULL DEFAULT '',
    is_primary       BOOLEAN NOT NULL DEFAULT FALSE,
    is_selected      BOOLEAN NOT NULL DEFAULT FALSE,
    is_hidden        BOOLEAN NOT NULL DEFAULT FALSE,
    background_color TEXT NOT NULL DEFAULT '',
    foreground_color TEXT NOT NULL DEFAULT '',
    raw_payload      JSONB NOT NULL DEFAULT '{}'::jsonb,
    source_run_id    TEXT REFERENCES google_calendar_sync_runs(run_id) ON DELETE SET NULL,
    first_seen_at    TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    last_seen_at     TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    last_error       TEXT NOT NULL DEFAULT '',
    updated_at       TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_google_calendar_sync_calendars_summary
    ON google_calendar_sync_calendars (summary);

CREATE TABLE IF NOT EXISTS google_calendar_sync_events (
    calendar_id           TEXT NOT NULL REFERENCES google_calendar_sync_calendars(calendar_id) ON DELETE CASCADE,
    event_id              TEXT NOT NULL,
    i_cal_uid             TEXT NOT NULL DEFAULT '',
    status                TEXT NOT NULL DEFAULT '',
    summary               TEXT NOT NULL DEFAULT '',
    description           TEXT NOT NULL DEFAULT '',
    location              TEXT NOT NULL DEFAULT '',
    html_link             TEXT NOT NULL DEFAULT '',
    creator               JSONB NOT NULL DEFAULT '{}'::jsonb,
    organizer             JSONB NOT NULL DEFAULT '{}'::jsonb,
    attendees             JSONB NOT NULL DEFAULT '[]'::jsonb,
    start_payload         JSONB NOT NULL DEFAULT '{}'::jsonb,
    end_payload           JSONB NOT NULL DEFAULT '{}'::jsonb,
    start_at              TIMESTAMPTZ,
    end_at                TIMESTAMPTZ,
    is_all_day            BOOLEAN NOT NULL DEFAULT FALSE,
    recurring_event_id    TEXT NOT NULL DEFAULT '',
    original_start        JSONB NOT NULL DEFAULT '{}'::jsonb,
    transparency          TEXT NOT NULL DEFAULT '',
    visibility            TEXT NOT NULL DEFAULT '',
    event_type            TEXT NOT NULL DEFAULT '',
    sequence              INTEGER NOT NULL DEFAULT 0,
    source_created_at     TIMESTAMPTZ,
    source_updated_at     TIMESTAMPTZ,
    content_text          TEXT NOT NULL DEFAULT '',
    content_hash          TEXT NOT NULL DEFAULT '',
    raw_payload           JSONB NOT NULL DEFAULT '{}'::jsonb,
    source_run_id         TEXT REFERENCES google_calendar_sync_runs(run_id) ON DELETE SET NULL,
    first_seen_at         TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    last_seen_at          TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    last_error            TEXT NOT NULL DEFAULT '',
    updated_at            TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (calendar_id, event_id)
);

CREATE INDEX IF NOT EXISTS idx_google_calendar_sync_events_start
    ON google_calendar_sync_events (start_at DESC);

CREATE INDEX IF NOT EXISTS idx_google_calendar_sync_events_updated
    ON google_calendar_sync_events (source_updated_at DESC);

CREATE INDEX IF NOT EXISTS idx_google_calendar_sync_events_text
    ON google_calendar_sync_events
    USING GIN (to_tsvector('english', coalesce(content_text, '')));

CREATE INDEX IF NOT EXISTS idx_google_calendar_sync_events_attendees
    ON google_calendar_sync_events USING GIN (attendees);

CREATE TABLE IF NOT EXISTS google_calendar_sync_checkpoints (
    calendar_id      TEXT PRIMARY KEY REFERENCES google_calendar_sync_calendars(calendar_id) ON DELETE CASCADE,
    sync_token       TEXT NOT NULL DEFAULT '',
    watermark_time   TIMESTAMPTZ,
    last_run_id      TEXT REFERENCES google_calendar_sync_runs(run_id) ON DELETE SET NULL,
    last_success_at  TIMESTAMPTZ,
    last_error       TEXT NOT NULL DEFAULT '',
    created_at       TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at       TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- migrate:down

DROP TABLE IF EXISTS google_calendar_sync_checkpoints;
DROP INDEX IF EXISTS idx_google_calendar_sync_events_attendees;
DROP INDEX IF EXISTS idx_google_calendar_sync_events_text;
DROP INDEX IF EXISTS idx_google_calendar_sync_events_updated;
DROP INDEX IF EXISTS idx_google_calendar_sync_events_start;
DROP TABLE IF EXISTS google_calendar_sync_events;
DROP INDEX IF EXISTS idx_google_calendar_sync_calendars_summary;
DROP TABLE IF EXISTS google_calendar_sync_calendars;
DROP INDEX IF EXISTS idx_google_calendar_sync_runs_started;
DROP TABLE IF EXISTS google_calendar_sync_runs;
