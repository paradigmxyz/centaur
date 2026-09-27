-- no-transaction

create index concurrently if not exists company_context_document_embeddings_embedding_hnsw_idx
    on company_context_document_embeddings
    using hnsw (embedding vector_cosine_ops)
    where embedding is not null and not embedding_failed;
