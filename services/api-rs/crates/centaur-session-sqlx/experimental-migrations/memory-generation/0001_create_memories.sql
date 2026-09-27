-- This migration is intentionally outside the main SQLx migration set.
-- Apply it manually only in environments participating in the experiment.

create extension if not exists pg_search;
create extension if not exists vector;

create table if not exists memories (
    id uuid primary key,
    content text not null check (length(content) between 1 and 1500),
    content_hash text not null check (content_hash <> ''),
    scope text not null check (scope in ('user', 'channel')),
    owner_id text not null check (owner_id <> ''),
    creator_user_id text not null check (creator_user_id <> ''),
    origin_thread_key text not null check (origin_thread_key <> ''),
    source_execution_id text not null check (source_execution_id <> ''),
    embedding vector(1536),
    embedding_model text,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    deleted_at timestamptz,
    check ((embedding is null) = (embedding_model is null))
);

create unique index if not exists idx_memories_active_owner_content
    on memories (scope, owner_id, content_hash)
    where deleted_at is null;

create index if not exists idx_memories_active_owner
    on memories (scope, owner_id, updated_at desc)
    where deleted_at is null;

create index if not exists idx_memories_source_execution
    on memories (source_execution_id);

create index if not exists idx_memories_active_creator
    on memories (creator_user_id, updated_at desc)
    where deleted_at is null;

create index if not exists idx_memories_missing_embedding
    on memories (created_at, id)
    where deleted_at is null and embedding is null;

drop index if exists idx_memories_bm25;

create index idx_memories_bm25
    on memories
    using bm25 (id, content, scope, owner_id, updated_at)
    with (key_field = 'id');

-- PostgreSQL requires concurrent index builds to run outside a transaction.
create index concurrently if not exists idx_memories_embedding_hnsw
    on memories
    using hnsw (embedding vector_cosine_ops)
    where deleted_at is null and embedding is not null;

-- The workflow is serialized, so one high-water mark is enough.
create table if not exists memory_generation_cursor (
    singleton boolean primary key default true check (singleton),
    completed_at timestamptz not null default 'epoch',
    execution_id text not null default ''
);

insert into memory_generation_cursor (singleton)
values (true)
on conflict (singleton) do nothing;
