create table if not exists linear_sync_runs (
    run_id text primary key,
    workflow_run_id text,
    mode text not null default 'incremental',
    status text not null,
    scopes_requested jsonb not null default '[]'::jsonb,
    scopes_synced jsonb not null default '[]'::jsonb,
    scopes_failed jsonb not null default '[]'::jsonb,
    projects_seen integer not null default 0,
    projects_upserted integer not null default 0,
    issues_seen integer not null default 0,
    issues_upserted integer not null default 0,
    comments_seen integer not null default 0,
    comments_upserted integer not null default 0,
    started_at timestamptz not null default now(),
    finished_at timestamptz,
    error_text text not null default '',
    metadata jsonb not null default '{}'::jsonb
);

create index if not exists idx_linear_sync_runs_started
    on linear_sync_runs (started_at desc);

create table if not exists linear_sync_projects (
    project_id text primary key,
    name text not null default '',
    description text not null default '',
    slug_id text not null default '',
    url text not null default '',
    state text not null default '',
    status_id text not null default '',
    status_name text not null default '',
    status_type text not null default '',
    lead_user_id text not null default '',
    lead_name text not null default '',
    team_ids jsonb not null default '[]'::jsonb,
    team_keys jsonb not null default '[]'::jsonb,
    content_text text not null default '',
    content_hash text not null default '',
    source_created_at timestamptz,
    source_updated_at timestamptz,
    source_archived_at timestamptz,
    source_completed_at timestamptz,
    source_canceled_at timestamptz,
    raw_payload jsonb not null default '{}'::jsonb,
    source_run_id text references linear_sync_runs(run_id) on delete set null,
    first_seen_at timestamptz not null default now(),
    last_seen_at timestamptz not null default now(),
    last_error text not null default '',
    updated_at timestamptz not null default now()
);

create index if not exists idx_linear_sync_projects_source_updated
    on linear_sync_projects (source_updated_at desc);

create index if not exists idx_linear_sync_projects_slug
    on linear_sync_projects (slug_id);

create index if not exists idx_linear_sync_projects_teams
    on linear_sync_projects using gin (team_ids);

create index if not exists idx_linear_sync_projects_text
    on linear_sync_projects
    using gin (to_tsvector('english', coalesce(content_text, '')));

create table if not exists linear_sync_issues (
    issue_id text primary key,
    identifier text not null default '',
    issue_number integer,
    title text not null default '',
    description text not null default '',
    url text not null default '',
    priority integer,
    priority_label text not null default '',
    estimate double precision,
    due_date date,
    team_id text not null default '',
    team_key text not null default '',
    team_name text not null default '',
    project_id text not null default '',
    project_name text not null default '',
    cycle_id text not null default '',
    cycle_name text not null default '',
    state_id text not null default '',
    state_name text not null default '',
    state_type text not null default '',
    assignee_user_id text not null default '',
    assignee_name text not null default '',
    creator_user_id text not null default '',
    creator_name text not null default '',
    parent_issue_id text not null default '',
    parent_identifier text not null default '',
    content_text text not null default '',
    content_hash text not null default '',
    source_created_at timestamptz,
    source_updated_at timestamptz,
    source_archived_at timestamptz,
    source_started_at timestamptz,
    source_completed_at timestamptz,
    source_canceled_at timestamptz,
    raw_payload jsonb not null default '{}'::jsonb,
    source_run_id text references linear_sync_runs(run_id) on delete set null,
    first_seen_at timestamptz not null default now(),
    last_seen_at timestamptz not null default now(),
    last_error text not null default '',
    updated_at timestamptz not null default now()
);

create index if not exists idx_linear_sync_issues_source_updated
    on linear_sync_issues (source_updated_at desc);

create index if not exists idx_linear_sync_issues_identifier
    on linear_sync_issues (identifier);

create index if not exists idx_linear_sync_issues_project
    on linear_sync_issues (project_id);

create index if not exists idx_linear_sync_issues_team
    on linear_sync_issues (team_id);

create index if not exists idx_linear_sync_issues_state
    on linear_sync_issues (state_id);

create index if not exists idx_linear_sync_issues_assignee
    on linear_sync_issues (assignee_user_id);

create index if not exists idx_linear_sync_issues_text
    on linear_sync_issues
    using gin (to_tsvector('english', coalesce(content_text, '')));

create table if not exists linear_sync_comments (
    comment_id text primary key,
    issue_id text not null default '',
    project_id text not null default '',
    parent_comment_id text not null default '',
    user_id text not null default '',
    user_name text not null default '',
    body text not null default '',
    url text not null default '',
    content_text text not null default '',
    content_hash text not null default '',
    source_created_at timestamptz,
    source_updated_at timestamptz,
    source_archived_at timestamptz,
    source_edited_at timestamptz,
    source_resolved_at timestamptz,
    raw_payload jsonb not null default '{}'::jsonb,
    source_run_id text references linear_sync_runs(run_id) on delete set null,
    first_seen_at timestamptz not null default now(),
    last_seen_at timestamptz not null default now(),
    last_error text not null default '',
    updated_at timestamptz not null default now()
);

create index if not exists idx_linear_sync_comments_source_updated
    on linear_sync_comments (source_updated_at desc);

create index if not exists idx_linear_sync_comments_issue
    on linear_sync_comments (issue_id);

create index if not exists idx_linear_sync_comments_project
    on linear_sync_comments (project_id);

create index if not exists idx_linear_sync_comments_user
    on linear_sync_comments (user_id);

create index if not exists idx_linear_sync_comments_text
    on linear_sync_comments
    using gin (to_tsvector('english', coalesce(content_text, '')));

create table if not exists linear_sync_checkpoints (
    scope_id text primary key,
    watermark_time timestamptz,
    last_run_id text references linear_sync_runs(run_id) on delete set null,
    last_success_at timestamptz,
    last_error text not null default '',
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now()
);
