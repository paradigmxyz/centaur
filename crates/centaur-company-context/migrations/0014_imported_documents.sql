-- Documents that operators import directly into the database instead of
-- having them synchronized from a source. Importers write
-- company_context_system.imported_documents; the service embeds new and
-- changed rows and projects them into company_context_data for retrieval.
-- Imported documents are public: every principal can read them.

create table company_context_system.imported_documents (
    source text not null,
    external_id text not null,
    title text not null default '',
    body text not null,
    occurred_at timestamptz,
    -- Importer-defined fields, such as a link or author; not searched.
    metadata jsonb not null default '{}'::jsonb,
    -- Set by trigger; the projection republishes rows whose hash changed.
    content_hash text not null default '',
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    primary key (source, external_id),
    check (source ~ '^[a-z0-9][a-z0-9-]{0,63}$'),
    check (external_id <> '' and octet_length(external_id) <= 512),
    check (char_length(title) <= 1000),
    check (btrim(body) <> '' and octet_length(body) <= 1000000),
    check (jsonb_typeof(metadata) = 'object' and octet_length(metadata::text) <= 65536)
);

create function company_context_system.imported_documents_hash()
returns trigger
language plpgsql
as $$
begin
    new.content_hash := encode(sha256(convert_to(jsonb_build_array(
        new.title,
        new.body,
        extract(epoch from new.occurred_at),
        new.metadata
    )::text, 'UTF8')), 'hex');
    if tg_op = 'UPDATE' then
        new.created_at := old.created_at;
        new.updated_at := now();
    end if;
    return new;
end
$$;

create trigger imported_documents_hash
    before insert or update on company_context_system.imported_documents
    for each row execute function company_context_system.imported_documents_hash();

-- Rows the embeddings API permanently rejected, so the projection does not
-- retry them until they change.
create table company_context_system.imported_document_rejections (
    source text not null,
    external_id text not null,
    content_hash text not null,
    error text not null default '',
    created_at timestamptz not null default now(),
    primary key (source, external_id),
    foreign key (source, external_id)
        references company_context_system.imported_documents(source, external_id)
        on delete cascade
);

create table company_context_data.imported_document_chunks (
    document_id text primary key,
    source text not null,
    external_id text not null,
    chunk_id text not null,
    title text not null default '',
    body text not null,
    occurred_at timestamptz,
    metadata jsonb not null default '{}'::jsonb,
    -- The imported row's content hash the chunk was projected from.
    item_hash text not null,
    content_hash text not null,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    unique (source, external_id, chunk_id),
    -- Deleting an imported row removes its chunks immediately.
    foreign key (source, external_id)
        references company_context_system.imported_documents(source, external_id)
        on delete cascade,
    check (document_id <> ''),
    check (chunk_id <> ''),
    check (body <> ''),
    check (item_hash <> ''),
    check (content_hash <> '')
);

create index imported_document_chunks_item_idx
    on company_context_data.imported_document_chunks (source, external_id, item_hash);

create index imported_document_chunks_occurred_idx
    on company_context_data.imported_document_chunks (occurred_at desc);

create index imported_document_chunks_bm25_idx
    on company_context_data.imported_document_chunks
    using bm25 (
        document_id,
        source,
        external_id,
        chunk_id,
        title,
        body,
        occurred_at
    )
    with (
        key_field = 'document_id',
        text_fields = '{
            "document_id": {"tokenizer": {"type": "keyword"}},
            "source": {"tokenizer": {"type": "keyword"}},
            "external_id": {"tokenizer": {"type": "keyword"}},
            "chunk_id": {"tokenizer": {"type": "keyword"}}
        }'
    );

create table company_context_data.imported_document_chunk_embeddings (
    document_id text primary key references company_context_data.imported_document_chunks(document_id) on delete cascade,
    model text not null,
    dimensions integer not null,
    content_hash text not null,
    embedding vector(1536) not null,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    check (model <> ''),
    check (dimensions = 1536),
    check (content_hash <> '')
);

create index imported_document_chunk_embeddings_hnsw_idx
    on company_context_data.imported_document_chunk_embeddings
    using hnsw (embedding vector_cosine_ops);
