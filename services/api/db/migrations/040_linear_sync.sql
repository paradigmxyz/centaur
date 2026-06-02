-- migrate:up

CREATE TABLE IF NOT EXISTS linear_sync_runs (
    run_id             TEXT PRIMARY KEY,
    workflow_run_id    TEXT,
    mode               TEXT NOT NULL DEFAULT 'incremental',
    status             TEXT NOT NULL,
    scopes_requested   JSONB NOT NULL DEFAULT '[]'::jsonb,
    scopes_synced      JSONB NOT NULL DEFAULT '[]'::jsonb,
    scopes_failed      JSONB NOT NULL DEFAULT '[]'::jsonb,
    projects_seen      INTEGER NOT NULL DEFAULT 0,
    projects_upserted  INTEGER NOT NULL DEFAULT 0,
    issues_seen        INTEGER NOT NULL DEFAULT 0,
    issues_upserted    INTEGER NOT NULL DEFAULT 0,
    comments_seen      INTEGER NOT NULL DEFAULT 0,
    comments_upserted  INTEGER NOT NULL DEFAULT 0,
    started_at         TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    finished_at        TIMESTAMPTZ,
    error_text         TEXT NOT NULL DEFAULT '',
    metadata           JSONB NOT NULL DEFAULT '{}'::jsonb
);

CREATE INDEX IF NOT EXISTS idx_linear_sync_runs_started
    ON linear_sync_runs (started_at DESC);

CREATE TABLE IF NOT EXISTS linear_sync_projects (
    project_id          TEXT PRIMARY KEY,
    name                TEXT NOT NULL DEFAULT '',
    description         TEXT NOT NULL DEFAULT '',
    slug_id             TEXT NOT NULL DEFAULT '',
    url                 TEXT NOT NULL DEFAULT '',
    state               TEXT NOT NULL DEFAULT '',
    status_id           TEXT NOT NULL DEFAULT '',
    status_name         TEXT NOT NULL DEFAULT '',
    status_type         TEXT NOT NULL DEFAULT '',
    lead_user_id        TEXT NOT NULL DEFAULT '',
    lead_name           TEXT NOT NULL DEFAULT '',
    team_ids            JSONB NOT NULL DEFAULT '[]'::jsonb,
    team_keys           JSONB NOT NULL DEFAULT '[]'::jsonb,
    content_text        TEXT NOT NULL DEFAULT '',
    content_hash        TEXT NOT NULL DEFAULT '',
    source_created_at   TIMESTAMPTZ,
    source_updated_at   TIMESTAMPTZ,
    source_archived_at  TIMESTAMPTZ,
    source_completed_at TIMESTAMPTZ,
    source_canceled_at  TIMESTAMPTZ,
    raw_payload         JSONB NOT NULL DEFAULT '{}'::jsonb,
    source_run_id       TEXT REFERENCES linear_sync_runs(run_id) ON DELETE SET NULL,
    first_seen_at       TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    last_seen_at        TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    last_error          TEXT NOT NULL DEFAULT '',
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_linear_sync_projects_source_updated
    ON linear_sync_projects (source_updated_at DESC);

CREATE INDEX IF NOT EXISTS idx_linear_sync_projects_slug
    ON linear_sync_projects (slug_id);

CREATE INDEX IF NOT EXISTS idx_linear_sync_projects_teams
    ON linear_sync_projects USING GIN (team_ids);

CREATE INDEX IF NOT EXISTS idx_linear_sync_projects_text
    ON linear_sync_projects
    USING GIN (to_tsvector('english', coalesce(content_text, '')));

CREATE TABLE IF NOT EXISTS linear_sync_issues (
    issue_id            TEXT PRIMARY KEY,
    identifier          TEXT NOT NULL DEFAULT '',
    issue_number        INTEGER,
    title               TEXT NOT NULL DEFAULT '',
    description         TEXT NOT NULL DEFAULT '',
    url                 TEXT NOT NULL DEFAULT '',
    priority            INTEGER,
    priority_label      TEXT NOT NULL DEFAULT '',
    estimate            DOUBLE PRECISION,
    due_date            DATE,
    team_id             TEXT NOT NULL DEFAULT '',
    team_key            TEXT NOT NULL DEFAULT '',
    team_name           TEXT NOT NULL DEFAULT '',
    project_id          TEXT NOT NULL DEFAULT '',
    project_name        TEXT NOT NULL DEFAULT '',
    cycle_id            TEXT NOT NULL DEFAULT '',
    cycle_name          TEXT NOT NULL DEFAULT '',
    state_id            TEXT NOT NULL DEFAULT '',
    state_name          TEXT NOT NULL DEFAULT '',
    state_type          TEXT NOT NULL DEFAULT '',
    assignee_user_id    TEXT NOT NULL DEFAULT '',
    assignee_name       TEXT NOT NULL DEFAULT '',
    creator_user_id     TEXT NOT NULL DEFAULT '',
    creator_name        TEXT NOT NULL DEFAULT '',
    parent_issue_id     TEXT NOT NULL DEFAULT '',
    parent_identifier   TEXT NOT NULL DEFAULT '',
    content_text        TEXT NOT NULL DEFAULT '',
    content_hash        TEXT NOT NULL DEFAULT '',
    source_created_at   TIMESTAMPTZ,
    source_updated_at   TIMESTAMPTZ,
    source_archived_at  TIMESTAMPTZ,
    source_started_at   TIMESTAMPTZ,
    source_completed_at TIMESTAMPTZ,
    source_canceled_at  TIMESTAMPTZ,
    raw_payload         JSONB NOT NULL DEFAULT '{}'::jsonb,
    source_run_id       TEXT REFERENCES linear_sync_runs(run_id) ON DELETE SET NULL,
    first_seen_at       TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    last_seen_at        TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    last_error          TEXT NOT NULL DEFAULT '',
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_linear_sync_issues_source_updated
    ON linear_sync_issues (source_updated_at DESC);

CREATE INDEX IF NOT EXISTS idx_linear_sync_issues_identifier
    ON linear_sync_issues (identifier);

CREATE INDEX IF NOT EXISTS idx_linear_sync_issues_project
    ON linear_sync_issues (project_id);

CREATE INDEX IF NOT EXISTS idx_linear_sync_issues_team
    ON linear_sync_issues (team_id);

CREATE INDEX IF NOT EXISTS idx_linear_sync_issues_state
    ON linear_sync_issues (state_id);

CREATE INDEX IF NOT EXISTS idx_linear_sync_issues_assignee
    ON linear_sync_issues (assignee_user_id);

CREATE INDEX IF NOT EXISTS idx_linear_sync_issues_text
    ON linear_sync_issues
    USING GIN (to_tsvector('english', coalesce(content_text, '')));

CREATE TABLE IF NOT EXISTS linear_sync_comments (
    comment_id          TEXT PRIMARY KEY,
    issue_id            TEXT NOT NULL DEFAULT '',
    project_id          TEXT NOT NULL DEFAULT '',
    parent_comment_id   TEXT NOT NULL DEFAULT '',
    user_id             TEXT NOT NULL DEFAULT '',
    user_name           TEXT NOT NULL DEFAULT '',
    body                TEXT NOT NULL DEFAULT '',
    url                 TEXT NOT NULL DEFAULT '',
    content_text        TEXT NOT NULL DEFAULT '',
    content_hash        TEXT NOT NULL DEFAULT '',
    source_created_at   TIMESTAMPTZ,
    source_updated_at   TIMESTAMPTZ,
    source_archived_at  TIMESTAMPTZ,
    source_edited_at    TIMESTAMPTZ,
    source_resolved_at  TIMESTAMPTZ,
    raw_payload         JSONB NOT NULL DEFAULT '{}'::jsonb,
    source_run_id       TEXT REFERENCES linear_sync_runs(run_id) ON DELETE SET NULL,
    first_seen_at       TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    last_seen_at        TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    last_error          TEXT NOT NULL DEFAULT '',
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_linear_sync_comments_source_updated
    ON linear_sync_comments (source_updated_at DESC);

CREATE INDEX IF NOT EXISTS idx_linear_sync_comments_issue
    ON linear_sync_comments (issue_id);

CREATE INDEX IF NOT EXISTS idx_linear_sync_comments_project
    ON linear_sync_comments (project_id);

CREATE INDEX IF NOT EXISTS idx_linear_sync_comments_user
    ON linear_sync_comments (user_id);

CREATE INDEX IF NOT EXISTS idx_linear_sync_comments_text
    ON linear_sync_comments
    USING GIN (to_tsvector('english', coalesce(content_text, '')));

CREATE TABLE IF NOT EXISTS linear_sync_checkpoints (
    scope_id         TEXT PRIMARY KEY,
    watermark_time   TIMESTAMPTZ,
    last_run_id      TEXT REFERENCES linear_sync_runs(run_id) ON DELETE SET NULL,
    last_success_at  TIMESTAMPTZ,
    last_error       TEXT NOT NULL DEFAULT '',
    created_at       TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at       TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- migrate:down

DROP TABLE IF EXISTS linear_sync_checkpoints;
DROP INDEX IF EXISTS idx_linear_sync_comments_text;
DROP INDEX IF EXISTS idx_linear_sync_comments_user;
DROP INDEX IF EXISTS idx_linear_sync_comments_project;
DROP INDEX IF EXISTS idx_linear_sync_comments_issue;
DROP INDEX IF EXISTS idx_linear_sync_comments_source_updated;
DROP TABLE IF EXISTS linear_sync_comments;
DROP INDEX IF EXISTS idx_linear_sync_issues_text;
DROP INDEX IF EXISTS idx_linear_sync_issues_assignee;
DROP INDEX IF EXISTS idx_linear_sync_issues_state;
DROP INDEX IF EXISTS idx_linear_sync_issues_team;
DROP INDEX IF EXISTS idx_linear_sync_issues_project;
DROP INDEX IF EXISTS idx_linear_sync_issues_identifier;
DROP INDEX IF EXISTS idx_linear_sync_issues_source_updated;
DROP TABLE IF EXISTS linear_sync_issues;
DROP INDEX IF EXISTS idx_linear_sync_projects_text;
DROP INDEX IF EXISTS idx_linear_sync_projects_teams;
DROP INDEX IF EXISTS idx_linear_sync_projects_slug;
DROP INDEX IF EXISTS idx_linear_sync_projects_source_updated;
DROP TABLE IF EXISTS linear_sync_projects;
DROP INDEX IF EXISTS idx_linear_sync_runs_started;
DROP TABLE IF EXISTS linear_sync_runs;
