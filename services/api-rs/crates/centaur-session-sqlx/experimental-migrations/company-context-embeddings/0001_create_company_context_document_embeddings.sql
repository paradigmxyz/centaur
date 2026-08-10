-- This migration is intentionally outside the main SQLx migration set.
-- Apply it manually only in environments participating in the experiment.

create extension if not exists vector;

create table if not exists company_context_document_embeddings (
    embedding_id bigint generated always as identity primary key,
    company_context_document_id text unique
        references company_context_documents(document_id) on delete cascade,
    google_docs_context_document_id text unique
        references google_docs_context_documents(document_id) on delete cascade,
    granola_context_document_id text unique
        references granola_context_documents(document_id) on delete cascade,
    model text not null,
    content_hash text not null,
    embedding vector(1536),
    embedding_failed boolean not null default false,
    failure_reason text,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    check (
        num_nonnulls(
            company_context_document_id,
            google_docs_context_document_id,
            granola_context_document_id
        ) = 1
    ),
    check (model <> ''),
    check (
        (
            not embedding_failed
            and embedding is not null
            and failure_reason is null
        )
        or (
            embedding_failed
            and embedding is null
            and failure_reason is not null
            and failure_reason <> ''
        )
    )
);

-- Keep the embedding writer available while this is applied to an existing
-- experiment table. PostgreSQL requires concurrent index builds to run outside
-- an explicit transaction.
create index concurrently if not exists company_context_document_embeddings_embedding_hnsw_idx
    on company_context_document_embeddings
    using hnsw (embedding vector_cosine_ops)
    where embedding is not null and not embedding_failed;

do $$
begin
    if not exists (
        select 1
        from pg_roles
        where rolname = 'centaur_company_context_embedding_writer'
    ) then
        create role centaur_company_context_embedding_writer nologin;
    end if;
end
$$;

grant usage on schema public
    to centaur_company_context_embedding_writer;
grant centaur_company_context_embedding_writer to current_user;

grant select on
    company_context_documents,
    google_docs_context_documents,
    granola_context_documents
    to centaur_company_context_embedding_writer;
grant select on company_context_document_embeddings
    to centaur_company_context_reader,
       centaur_company_context_embedding_writer;
grant insert, update on company_context_document_embeddings
    to centaur_company_context_embedding_writer;
grant usage, select on sequence
    company_context_document_embeddings_embedding_id_seq
    to centaur_company_context_embedding_writer;

alter table company_context_document_embeddings enable row level security;

drop policy if exists centaur_cc_embedding_writer_documents_select
    on company_context_documents;
create policy centaur_cc_embedding_writer_documents_select
    on company_context_documents
    for select
    to centaur_company_context_embedding_writer
    using (true);

drop policy if exists centaur_cc_embedding_writer_google_docs_select
    on google_docs_context_documents;
create policy centaur_cc_embedding_writer_google_docs_select
    on google_docs_context_documents
    for select
    to centaur_company_context_embedding_writer
    using (true);

drop policy if exists centaur_cc_embedding_writer_granola_select
    on granola_context_documents;
create policy centaur_cc_embedding_writer_granola_select
    on granola_context_documents
    for select
    to centaur_company_context_embedding_writer
    using (true);

create or replace function centaur_company_context_embedding_document_visible(
    p_company_context_document_id text,
    p_google_docs_context_document_id text,
    p_granola_context_document_id text
)
returns boolean
language sql
stable
set search_path = public
as $$
    select
        (
            p_company_context_document_id is not null
            and exists (
                select 1
                from company_context_documents documents
                where documents.document_id = p_company_context_document_id
            )
        )
        or (
            p_google_docs_context_document_id is not null
            and exists (
                select 1
                from google_docs_context_documents documents
                where documents.document_id = p_google_docs_context_document_id
            )
        )
        or (
            p_granola_context_document_id is not null
            and exists (
                select 1
                from granola_context_documents documents
                where documents.document_id = p_granola_context_document_id
            )
        )
$$;

revoke all on function centaur_company_context_embedding_document_visible(
    text,
    text,
    text
)
    from public;
grant execute on function centaur_company_context_embedding_document_visible(
    text,
    text,
    text
)
    to centaur_company_context_reader,
       centaur_company_context_embedding_writer;

drop policy if exists centaur_cc_embeddings_select
    on company_context_document_embeddings;
create policy centaur_cc_embeddings_select
    on company_context_document_embeddings
    for select
    to centaur_company_context_reader,
       centaur_company_context_embedding_writer
    using (
        centaur_company_context_embedding_document_visible(
            company_context_document_id,
            google_docs_context_document_id,
            granola_context_document_id
        )
    );

drop policy if exists centaur_cc_embeddings_writer_insert
    on company_context_document_embeddings;
create policy centaur_cc_embeddings_writer_insert
    on company_context_document_embeddings
    for insert
    to centaur_company_context_embedding_writer
    with check (
        centaur_company_context_embedding_document_visible(
            company_context_document_id,
            google_docs_context_document_id,
            granola_context_document_id
        )
    );

drop policy if exists centaur_cc_embeddings_writer_update
    on company_context_document_embeddings;
create policy centaur_cc_embeddings_writer_update
    on company_context_document_embeddings
    for update
    to centaur_company_context_embedding_writer
    using (
        centaur_company_context_embedding_document_visible(
            company_context_document_id,
            google_docs_context_document_id,
            granola_context_document_id
        )
    )
    with check (
        centaur_company_context_embedding_document_visible(
            company_context_document_id,
            google_docs_context_document_id,
            granola_context_document_id
        )
    );
