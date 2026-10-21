-- Imported documents become custom documents: rows written directly to the
-- database rather than synchronized by an indexer, such as imports, notes,
-- and memory. Published document IDs move from the imported: prefix to
-- custom:.

alter table company_context_system.imported_documents
    rename to custom_documents;
alter table company_context_system.custom_documents
    rename constraint imported_documents_pkey to custom_documents_pkey;
alter table company_context_system.custom_documents
    rename constraint imported_documents_source_check to custom_documents_source_check;
alter table company_context_system.custom_documents
    rename constraint imported_documents_external_id_check to custom_documents_external_id_check;
alter table company_context_system.custom_documents
    rename constraint imported_documents_title_check to custom_documents_title_check;
alter table company_context_system.custom_documents
    rename constraint imported_documents_body_check to custom_documents_body_check;
alter table company_context_system.custom_documents
    rename constraint imported_documents_metadata_check to custom_documents_metadata_check;
alter function company_context_system.imported_documents_hash()
    rename to custom_documents_hash;
alter trigger imported_documents_hash on company_context_system.custom_documents
    rename to custom_documents_hash;

alter table company_context_system.imported_document_rejections
    rename to custom_document_rejections;
alter table company_context_system.custom_document_rejections
    rename constraint imported_document_rejections_pkey
    to custom_document_rejections_pkey;
alter table company_context_system.custom_document_rejections
    rename constraint imported_document_rejections_source_external_id_fkey
    to custom_document_rejections_source_external_id_fkey;

alter table company_context_data.imported_document_chunks
    rename to custom_document_chunks;
alter table company_context_data.custom_document_chunks
    rename constraint imported_document_chunks_pkey to custom_document_chunks_pkey;
alter table company_context_data.custom_document_chunks
    rename constraint imported_document_chunks_source_external_id_chunk_id_key
    to custom_document_chunks_source_external_id_chunk_id_key;
alter table company_context_data.custom_document_chunks
    rename constraint imported_document_chunks_source_external_id_fkey
    to custom_document_chunks_source_external_id_fkey;
alter table company_context_data.custom_document_chunks
    rename constraint imported_document_chunks_document_id_check
    to custom_document_chunks_document_id_check;
alter table company_context_data.custom_document_chunks
    rename constraint imported_document_chunks_chunk_id_check
    to custom_document_chunks_chunk_id_check;
alter table company_context_data.custom_document_chunks
    rename constraint imported_document_chunks_body_check
    to custom_document_chunks_body_check;
alter table company_context_data.custom_document_chunks
    rename constraint imported_document_chunks_item_hash_check
    to custom_document_chunks_item_hash_check;
alter table company_context_data.custom_document_chunks
    rename constraint imported_document_chunks_content_hash_check
    to custom_document_chunks_content_hash_check;
alter index company_context_data.imported_document_chunks_item_idx
    rename to custom_document_chunks_item_idx;
alter index company_context_data.imported_document_chunks_occurred_idx
    rename to custom_document_chunks_occurred_idx;
alter index company_context_data.imported_document_chunks_bm25_idx
    rename to custom_document_chunks_bm25_idx;

alter table company_context_data.imported_document_chunk_embeddings
    rename to custom_document_chunk_embeddings;
alter table company_context_data.custom_document_chunk_embeddings
    rename constraint imported_document_chunk_embeddings_pkey
    to custom_document_chunk_embeddings_pkey;
alter table company_context_data.custom_document_chunk_embeddings
    rename constraint imported_document_chunk_embeddings_model_check
    to custom_document_chunk_embeddings_model_check;
alter table company_context_data.custom_document_chunk_embeddings
    rename constraint imported_document_chunk_embeddings_dimensions_check
    to custom_document_chunk_embeddings_dimensions_check;
alter table company_context_data.custom_document_chunk_embeddings
    rename constraint imported_document_chunk_embeddings_content_hash_check
    to custom_document_chunk_embeddings_content_hash_check;
alter index company_context_data.imported_document_chunk_embeddings_hnsw_idx
    rename to custom_document_chunk_embeddings_hnsw_idx;

-- Rewrite projected document IDs in place so existing embeddings are kept.
alter table company_context_data.custom_document_chunk_embeddings
    drop constraint imported_document_chunk_embeddings_document_id_fkey;
update company_context_data.custom_document_chunks
set document_id = 'custom:' || substr(document_id, length('imported:') + 1)
where document_id like 'imported:%';
update company_context_data.custom_document_chunk_embeddings
set document_id = 'custom:' || substr(document_id, length('imported:') + 1)
where document_id like 'imported:%';
alter table company_context_data.custom_document_chunk_embeddings
    add constraint custom_document_chunk_embeddings_document_id_fkey
    foreign key (document_id)
    references company_context_data.custom_document_chunks(document_id)
    on delete cascade;
