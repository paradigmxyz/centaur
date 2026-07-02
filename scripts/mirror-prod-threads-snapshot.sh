#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage:
  scripts/mirror-prod-threads-snapshot.sh [all|snapshot|import]

Creates a bounded, local-only snapshot of production thread/session data for
Centaur Console UX work. The source connection is forced into a read-only
transaction mode with PGOPTIONS. The import target is expected to be a local
ai_v2 database and is truncated by default.

Modes:
  all       Export from source and import into local target. Default.
  snapshot  Export CSV files only.
  import    Import CSV files from SNAPSHOT_DIR only.

Required for snapshot/all:
  CENTAUR_PROD_DATABASE_URL
      Read-only Postgres DSN for the production ai_v2 database.

Optional production secret lookup:
  CENTAUR_PROD_KUBE_CONTEXT
  CENTAUR_PROD_NAMESPACE=centaur
  CENTAUR_PROD_DATABASE_URL_SECRET_NAME
  CENTAUR_PROD_DATABASE_URL_SECRET_KEY=DATABASE_URL

Optional import target:
  CENTAUR_LOCAL_DB_CONTAINER=codex-centaur-console-db
  CENTAUR_LOCAL_CENTAUR_DATABASE_URL=postgresql://postgres:postgres@localhost:5432/ai_v2

Snapshot sizing:
  THREAD_LIMIT=250
  MESSAGE_LIMIT_PER_THREAD=120
  EXECUTION_LIMIT_PER_THREAD=20
  EVENT_LIMIT_PER_THREAD=40
  THINKING_EVENT_LIMIT_PER_THREAD=200

Safety:
  TRUNCATE_LOCAL_SESSION_TABLES=1
  ALLOW_NONLOCAL_TARGET=0

After import, run Console with:
  CENTAUR_CONSOLE_THREADS_READ_ONLY=1
USAGE
}

die() {
  echo "error: $*" >&2
  exit 1
}

info() {
  echo "==> $*"
}

require_command() {
  command -v "$1" >/dev/null 2>&1 || die "missing required command: $1"
}

validate_integer() {
  local name="$1"
  local value="$2"
  [[ "$value" =~ ^[0-9]+$ ]] || die "$name must be a non-negative integer"
}

sql_quote_path() {
  local value="$1"
  printf "%s" "${value//\'/\'\'}"
}

resolve_source_database_url() {
  if [[ -n "${CENTAUR_PROD_DATABASE_URL:-}" ]]; then
    printf "%s" "$CENTAUR_PROD_DATABASE_URL"
    return
  fi

  if [[ -n "${CENTAUR_PROD_KUBE_CONTEXT:-}" ]]; then
    require_command kubectl
    local secret_name="${CENTAUR_PROD_DATABASE_URL_SECRET_NAME:-}"
    local secret_key="${CENTAUR_PROD_DATABASE_URL_SECRET_KEY:-DATABASE_URL}"
    local namespace="${CENTAUR_PROD_NAMESPACE:-centaur}"
    [[ -n "$secret_name" ]] || die \
      "set CENTAUR_PROD_DATABASE_URL to a read-only DSN, or set CENTAUR_PROD_DATABASE_URL_SECRET_NAME for kube lookup"

    kubectl --context "$CENTAUR_PROD_KUBE_CONTEXT" \
      -n "$namespace" \
      get secret "$secret_name" \
      -o "jsonpath={.data.${secret_key}}" |
      python3 -c 'import base64, sys; sys.stdout.write(base64.b64decode(sys.stdin.read()).decode())'
    return
  fi

  die "set CENTAUR_PROD_DATABASE_URL to a read-only production ai_v2 DSN"
}

assert_import_target_is_local() {
  local source_url="${1:-}"
  local target_url="$2"

  require_command python3
  python3 - "$source_url" "$target_url" "${ALLOW_NONLOCAL_TARGET:-0}" <<'PY'
import sys
from urllib.parse import urlparse

source = urlparse(sys.argv[1]) if sys.argv[1] else None
target = urlparse(sys.argv[2])
allow_nonlocal = sys.argv[3].lower() in {"1", "true", "yes"}

local_hosts = {
    "",
    "localhost",
    "127.0.0.1",
    "::1",
    "codex-centaur-console-db",
    "host.docker.internal",
}
target_host = target.hostname or ""
target_db = (target.path or "").lstrip("/")

if not allow_nonlocal and target_host not in local_hosts:
    raise SystemExit(
        f"target host {target_host!r} is not local; set ALLOW_NONLOCAL_TARGET=1 to override"
    )

if source:
    source_db = (source.path or "").lstrip("/")
    same_host = (source.hostname or "") == target_host
    same_port = (source.port or 5432) == (target.port or 5432)
    same_db = source_db == target_db
    if same_host and same_port and same_db:
        raise SystemExit("source and target appear to point at the same database")
PY
}

copy_to_csv() {
  local database_url="$1"
  local output_file="$2"
  local sql="$3"

  PGOPTIONS="-c default_transaction_read_only=on -c statement_timeout=600000" \
    psql --no-psqlrc -X "$database_url" \
      -v ON_ERROR_STOP=1 \
      -c "copy ($sql) to stdout with (format csv, header true, force_quote *);" \
      > "$output_file"
}

create_snapshot() {
  local source_url="$1"
  local snapshot_dir="$2"

  require_command psql
  mkdir -p "$snapshot_dir"

  local recent_sessions_sql="
    select thread_key
    from sessions
    order by coalesce(updated_at, created_at) desc, thread_key
    limit ${THREAD_LIMIT}
  "

  info "exporting ${THREAD_LIMIT} recent sessions"
  copy_to_csv "$source_url" "$snapshot_dir/sessions.csv" "
    with recent_sessions as (${recent_sessions_sql})
    select
      s.thread_key,
      s.sandbox_id,
      s.harness_type,
      s.harness_thread_id,
      s.iron_control_principal,
      s.persona_id,
      s.status,
      s.metadata,
      s.created_at,
      s.updated_at
    from sessions s
    join recent_sessions r using (thread_key)
    order by coalesce(s.updated_at, s.created_at) desc, s.thread_key
  "

  info "exporting up to ${MESSAGE_LIMIT_PER_THREAD} messages per thread"
  copy_to_csv "$source_url" "$snapshot_dir/session_messages.csv" "
    with recent_sessions as (${recent_sessions_sql}),
    ranked as (
      select
        m.message_id,
        m.thread_key,
        m.client_message_id,
        m.role,
        m.parts,
        m.metadata,
        m.created_at,
        row_number() over (
          partition by m.thread_key
          order by m.created_at desc, m.message_id desc
        ) as rn
      from session_messages m
      join recent_sessions r using (thread_key)
    )
    select message_id, thread_key, client_message_id, role, parts, metadata, created_at
    from ranked
    where rn <= ${MESSAGE_LIMIT_PER_THREAD}
    order by thread_key, created_at, message_id
  "

  info "exporting up to ${EXECUTION_LIMIT_PER_THREAD} executions per thread"
  copy_to_csv "$source_url" "$snapshot_dir/session_executions.csv" "
    with recent_sessions as (${recent_sessions_sql}),
    ranked as (
      select
        e.execution_id,
        e.thread_key,
        e.idempotency_key,
        e.status,
        e.metadata,
        e.error,
        e.created_at,
        e.updated_at,
        e.started_at,
        e.completed_at,
        row_number() over (
          partition by e.thread_key
          order by e.created_at desc, e.execution_id desc
        ) as rn
      from session_executions e
      join recent_sessions r using (thread_key)
    )
    select
      execution_id,
      thread_key,
      idempotency_key,
      status,
      metadata,
      error,
      created_at,
      updated_at,
      started_at,
      completed_at
    from ranked
    where rn <= ${EXECUTION_LIMIT_PER_THREAD}
    order by thread_key, created_at, execution_id
  "

  info "exporting up to ${EVENT_LIMIT_PER_THREAD} terminal events and ${THINKING_EVENT_LIMIT_PER_THREAD} reasoning lines per thread"
  copy_to_csv "$source_url" "$snapshot_dir/session_events.csv" "
    with recent_sessions as (${recent_sessions_sql}),
    ranked as (
      select
        ev.thread_key,
        ev.execution_id,
        ev.event_type,
        ev.payload,
        ev.created_at,
        row_number() over (
          partition by ev.thread_key
          order by ev.event_id desc
        ) as rn
      from session_events ev
      join recent_sessions r using (thread_key)
      where ev.event_type in (
        'session.execution_completed',
        'session.execution_failed',
        'session.execution_cancelled'
      )
    ),
    -- Reasoning traces live in the session.output.line firehose as
    -- item/completed notifications for reasoning items. The LIKE filter keeps
    -- the export from paging every stdout line; Console re-filters exactly.
    ranked_thinking as (
      select
        ev.thread_key,
        ev.execution_id,
        ev.event_type,
        ev.payload,
        ev.created_at,
        row_number() over (
          partition by ev.thread_key
          order by ev.event_id desc
        ) as rn
      from session_events ev
      join recent_sessions r using (thread_key)
      where ev.event_type = 'session.output.line'
        and ev.payload::text like '%reasoning%'
    )
    select thread_key, execution_id, event_type, payload, created_at
    from (
      select thread_key, execution_id, event_type, payload, created_at
      from ranked
      where rn <= ${EVENT_LIMIT_PER_THREAD}
      union all
      select thread_key, execution_id, event_type, payload, created_at
      from ranked_thinking
      where rn <= ${THINKING_EVENT_LIMIT_PER_THREAD}
    ) combined
    order by thread_key, created_at
  "

  info "exporting Slack users referenced by mirrored threads"
  copy_to_csv "$source_url" "$snapshot_dir/slack_sync_users.csv" "
    with recent_sessions as (${recent_sessions_sql}),
    message_mentions as (
      select coalesce(mention.match[1], mention.match[2]) as user_id
      from session_messages m
      join recent_sessions r using (thread_key)
      cross join lateral regexp_matches(
        m.parts::text,
        '<@([UW][A-Z0-9]+)(?:\\|[^>]+)?>|@([UW][A-Z0-9]+)',
        'g'
      ) as mention(match)
    ),
    event_mentions as (
      select coalesce(mention.match[1], mention.match[2]) as user_id
      from session_events ev
      join recent_sessions r using (thread_key)
      cross join lateral regexp_matches(
        ev.payload::text,
        '<@([UW][A-Z0-9]+)(?:\\|[^>]+)?>|@([UW][A-Z0-9]+)',
        'g'
      ) as mention(match)
    ),
    metadata_users as (
      select s.metadata ->> key.name as user_id
      from sessions s
      join recent_sessions r using (thread_key)
      cross join (values ('slack_user_id'), ('user_id'), ('actor_user_id')) as key(name)
      union all
      select m.metadata ->> key.name as user_id
      from session_messages m
      join recent_sessions r using (thread_key)
      cross join (values ('slack_user_id'), ('user_id'), ('actor_user_id')) as key(name)
    ),
    referenced_users as (
      select distinct nullif(user_id, '') as user_id
      from (
        select user_id from message_mentions
        union all
        select user_id from event_mentions
        union all
        select user_id from metadata_users
      ) ids
      where nullif(user_id, '') is not null
    )
    select
      u.user_id,
      u.user_name,
      u.real_name,
      u.display_name,
      u.is_bot,
      u.is_deleted,
      u.team_id,
      u.raw_payload,
      u.first_seen_at,
      u.last_seen_at,
      u.updated_at
    from slack_sync_users u
    join referenced_users r using (user_id)
    order by u.user_id
  "

  {
    echo "created_at=$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    echo "thread_limit=${THREAD_LIMIT}"
    echo "message_limit_per_thread=${MESSAGE_LIMIT_PER_THREAD}"
    echo "execution_limit_per_thread=${EXECUTION_LIMIT_PER_THREAD}"
    echo "event_limit_per_thread=${EVENT_LIMIT_PER_THREAD}"
    echo "thinking_event_limit_per_thread=${THINKING_EVENT_LIMIT_PER_THREAD}"
    echo "slack_sync_users=referenced"
  } > "$snapshot_dir/manifest.env"

  info "snapshot written to $snapshot_dir"
}

write_import_sql() {
  local import_root="$1"
  local output_file="$2"
  local sessions_csv messages_csv executions_csv events_csv slack_users_csv
  sessions_csv="$(sql_quote_path "$import_root/sessions.csv")"
  messages_csv="$(sql_quote_path "$import_root/session_messages.csv")"
  executions_csv="$(sql_quote_path "$import_root/session_executions.csv")"
  events_csv="$(sql_quote_path "$import_root/session_events.csv")"
  slack_users_csv="$(sql_quote_path "$import_root/slack_sync_users.csv")"

  cat > "$output_file" <<SQL
\\set ON_ERROR_STOP on

begin;

create temp table import_sessions (
  thread_key text,
  sandbox_id text,
  harness_type text,
  harness_thread_id text,
  iron_control_principal text,
  persona_id text,
  status text,
  metadata jsonb,
  created_at timestamptz,
  updated_at timestamptz
);

create temp table import_session_messages (
  message_id text,
  thread_key text,
  client_message_id text,
  role text,
  parts jsonb,
  metadata jsonb,
  created_at timestamptz
);

create temp table import_session_executions (
  execution_id text,
  thread_key text,
  idempotency_key text,
  status text,
  metadata jsonb,
  error text,
  created_at timestamptz,
  updated_at timestamptz,
  started_at timestamptz,
  completed_at timestamptz
);

create temp table import_session_events (
  thread_key text,
  execution_id text,
  event_type text,
  payload jsonb,
  created_at timestamptz
);

create temp table import_slack_sync_users (
  user_id text,
  user_name text,
  real_name text,
  display_name text,
  is_bot boolean,
  is_deleted boolean,
  team_id text,
  raw_payload jsonb,
  first_seen_at timestamptz,
  last_seen_at timestamptz,
  updated_at timestamptz
);

\\copy import_sessions from '${sessions_csv}' with (format csv, header true)
\\copy import_session_messages from '${messages_csv}' with (format csv, header true)
\\copy import_session_executions from '${executions_csv}' with (format csv, header true)
\\copy import_session_events from '${events_csv}' with (format csv, header true)
\\copy import_slack_sync_users from '${slack_users_csv}' with (format csv, header true)

SQL

  if [[ "${TRUNCATE_LOCAL_SESSION_TABLES}" == "1" ]]; then
    cat >> "$output_file" <<'SQL'
truncate table session_events, session_messages, session_executions, sessions cascade;

SQL
  fi

  cat >> "$output_file" <<'SQL'
create table if not exists slack_sync_users (
    user_id text primary key,
    user_name text not null default '',
    real_name text not null default '',
    display_name text not null default '',
    is_bot boolean not null default false,
    is_deleted boolean not null default false,
    team_id text not null default '',
    raw_payload jsonb not null default '{}'::jsonb,
    first_seen_at timestamptz not null default now(),
    last_seen_at timestamptz not null default now(),
    updated_at timestamptz not null default now()
);

create index if not exists idx_slack_sync_users_real_name
    on slack_sync_users (real_name);

insert into sessions (
  thread_key,
  sandbox_id,
  harness_type,
  harness_thread_id,
  iron_control_principal,
  persona_id,
  status,
  metadata,
  created_at,
  updated_at
)
select
  thread_key,
  sandbox_id,
  harness_type,
  harness_thread_id,
  iron_control_principal,
  persona_id,
  status,
  metadata,
  created_at,
  updated_at
from import_sessions
on conflict (thread_key) do update set
  sandbox_id = excluded.sandbox_id,
  harness_type = excluded.harness_type,
  harness_thread_id = excluded.harness_thread_id,
  iron_control_principal = excluded.iron_control_principal,
  persona_id = excluded.persona_id,
  status = excluded.status,
  metadata = excluded.metadata,
  created_at = excluded.created_at,
  updated_at = excluded.updated_at;

insert into session_messages (
  message_id,
  thread_key,
  client_message_id,
  role,
  parts,
  metadata,
  created_at
)
select
  message_id,
  thread_key,
  client_message_id,
  role,
  parts,
  metadata,
  created_at
from import_session_messages
on conflict (message_id) do update set
  thread_key = excluded.thread_key,
  client_message_id = excluded.client_message_id,
  role = excluded.role,
  parts = excluded.parts,
  metadata = excluded.metadata,
  created_at = excluded.created_at;

insert into session_executions (
  execution_id,
  thread_key,
  idempotency_key,
  status,
  metadata,
  error,
  created_at,
  updated_at,
  started_at,
  completed_at
)
select
  execution_id,
  thread_key,
  idempotency_key,
  status,
  metadata,
  error,
  created_at,
  updated_at,
  started_at,
  completed_at
from import_session_executions
on conflict (execution_id) do update set
  thread_key = excluded.thread_key,
  idempotency_key = excluded.idempotency_key,
  status = excluded.status,
  metadata = excluded.metadata,
  error = excluded.error,
  created_at = excluded.created_at,
  updated_at = excluded.updated_at,
  started_at = excluded.started_at,
  completed_at = excluded.completed_at;

insert into session_events (
  thread_key,
  execution_id,
  event_type,
  payload,
  created_at
)
select
  ev.thread_key,
  ev.execution_id,
  ev.event_type,
  ev.payload,
  ev.created_at
from import_session_events ev
where ev.execution_id is null
   or exists (
     select 1
     from session_executions imported_execution
     where imported_execution.execution_id = ev.execution_id
   );

insert into slack_sync_users (
  user_id,
  user_name,
  real_name,
  display_name,
  is_bot,
  is_deleted,
  team_id,
  raw_payload,
  first_seen_at,
  last_seen_at,
  updated_at
)
select
  user_id,
  coalesce(user_name, ''),
  coalesce(real_name, ''),
  coalesce(display_name, ''),
  coalesce(is_bot, false),
  coalesce(is_deleted, false),
  coalesce(team_id, ''),
  coalesce(raw_payload, '{}'::jsonb),
  coalesce(first_seen_at, now()),
  coalesce(last_seen_at, now()),
  coalesce(updated_at, now())
from import_slack_sync_users
where nullif(user_id, '') is not null
on conflict (user_id) do update set
  user_name = excluded.user_name,
  real_name = excluded.real_name,
  display_name = excluded.display_name,
  is_bot = excluded.is_bot,
  is_deleted = excluded.is_deleted,
  team_id = excluded.team_id,
  raw_payload = excluded.raw_payload,
  first_seen_at = excluded.first_seen_at,
  last_seen_at = excluded.last_seen_at,
  updated_at = excluded.updated_at;

analyze sessions;
analyze session_messages;
analyze session_executions;
analyze session_events;
analyze slack_sync_users;

commit;
SQL
}

import_snapshot() {
  local snapshot_dir="$1"
  local target_url="$2"

  [[ -f "$snapshot_dir/sessions.csv" ]] || die "missing $snapshot_dir/sessions.csv"
  [[ -f "$snapshot_dir/session_messages.csv" ]] || die "missing $snapshot_dir/session_messages.csv"
  [[ -f "$snapshot_dir/session_executions.csv" ]] || die "missing $snapshot_dir/session_executions.csv"
  [[ -f "$snapshot_dir/session_events.csv" ]] || die "missing $snapshot_dir/session_events.csv"
  [[ -f "$snapshot_dir/slack_sync_users.csv" ]] || die "missing $snapshot_dir/slack_sync_users.csv"

  local local_container="${CENTAUR_LOCAL_DB_CONTAINER:-codex-centaur-console-db}"
  local use_container="${USE_LOCAL_DB_CONTAINER:-auto}"
  local import_root="$snapshot_dir"
  local import_sql="$snapshot_dir/import.sql"

  if [[ "$use_container" == "auto" ]] && command -v docker >/dev/null 2>&1 \
    && docker inspect "$local_container" >/dev/null 2>&1; then
    use_container="1"
  fi

  if [[ "$use_container" == "1" || "$use_container" == "true" ]]; then
    require_command docker
    import_root="/tmp/centaur-thread-snapshot"
    write_import_sql "$import_root" "$import_sql"

    info "copying snapshot into local database container $local_container"
    docker exec "$local_container" sh -c "rm -rf '$import_root' && mkdir -p '$import_root'"
    docker cp "$snapshot_dir/." "$local_container:$import_root/"

    info "importing snapshot into local target via $local_container"
    docker exec -i "$local_container" psql --no-psqlrc -X "$target_url" < "$import_sql"
  else
    require_command psql
    write_import_sql "$import_root" "$import_sql"

    info "importing snapshot into local target"
    psql --no-psqlrc -X "$target_url" < "$import_sql"
  fi
}

main() {
  local mode="${1:-all}"
  if [[ "$mode" == "-h" || "$mode" == "--help" ]]; then
    usage
    exit 0
  fi
  [[ "$mode" =~ ^(all|snapshot|import)$ ]] || die "unknown mode: $mode"

  THREAD_LIMIT="${THREAD_LIMIT:-250}"
  MESSAGE_LIMIT_PER_THREAD="${MESSAGE_LIMIT_PER_THREAD:-120}"
  EXECUTION_LIMIT_PER_THREAD="${EXECUTION_LIMIT_PER_THREAD:-20}"
  EVENT_LIMIT_PER_THREAD="${EVENT_LIMIT_PER_THREAD:-40}"
  THINKING_EVENT_LIMIT_PER_THREAD="${THINKING_EVENT_LIMIT_PER_THREAD:-200}"
  TRUNCATE_LOCAL_SESSION_TABLES="${TRUNCATE_LOCAL_SESSION_TABLES:-1}"

  validate_integer THREAD_LIMIT "$THREAD_LIMIT"
  validate_integer MESSAGE_LIMIT_PER_THREAD "$MESSAGE_LIMIT_PER_THREAD"
  validate_integer EXECUTION_LIMIT_PER_THREAD "$EXECUTION_LIMIT_PER_THREAD"
  validate_integer EVENT_LIMIT_PER_THREAD "$EVENT_LIMIT_PER_THREAD"

  local snapshot_dir="${SNAPSHOT_DIR:-}"
  if [[ -z "$snapshot_dir" ]]; then
    snapshot_dir="$(mktemp -d "${TMPDIR:-/tmp}/centaur-thread-snapshot.XXXXXX")"
  fi

  local source_url=""
  if [[ "$mode" == "all" || "$mode" == "snapshot" ]]; then
    source_url="$(resolve_source_database_url)"
  fi

  local target_url="${CENTAUR_LOCAL_CENTAUR_DATABASE_URL:-${CENTAUR_CONSOLE_CENTAUR_DATABASE_URL:-${TARGET_DATABASE_URL:-postgresql://postgres:postgres@localhost:5432/ai_v2}}}"
  if [[ "$mode" == "all" || "$mode" == "import" ]]; then
    assert_import_target_is_local "$source_url" "$target_url"
  fi

  if [[ "$mode" == "all" || "$mode" == "snapshot" ]]; then
    create_snapshot "$source_url" "$snapshot_dir"
  fi

  if [[ "$mode" == "all" || "$mode" == "import" ]]; then
    import_snapshot "$snapshot_dir" "$target_url"
    info "import complete"
    info "restart Console with CENTAUR_CONSOLE_THREADS_READ_ONLY=1 before browsing mirrored data"
  fi
}

main "$@"
