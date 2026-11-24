-- Which nanocode session holds each Centaur thread. The brain assigns a
-- session id (a UUIDv7, as nanocode requires) when a thread first appears and
-- looks it up afterwards. The brain role only reads and adds rows.
create table if not exists nanocode.thread_sessions (
    thread_key text primary key,
    session_id text not null unique,
    created_at timestamptz not null default now()
);

revoke all privileges on nanocode.thread_sessions from centaur_brain;
grant select, insert on nanocode.thread_sessions to centaur_brain;
