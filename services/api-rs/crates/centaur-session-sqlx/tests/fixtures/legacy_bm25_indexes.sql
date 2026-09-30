-- BM25 indexes that core migrations created before the text-search backends
-- split. Used to reproduce a database migrated by the pre-split releases.
create extension if not exists pg_search;

create index idx_company_context_documents_bm25
    on company_context_documents
    using bm25 (
        document_id,
        title,
        body,
        source,
        source_type,
        access_scope,
        occurred_at,
        source_updated_at,
        metadata
    )
    with (
        key_field = 'document_id',
        text_fields = '{
            "document_id": {
                "tokenizer": {"type": "keyword"}
            }
        }'
    );

create index idx_google_docs_context_documents_bm25
    on google_docs_context_documents
    using bm25 (
        document_id,
        title,
        body,
        file_id,
        chunk_id,
        url,
        provider_author_id,
        provider_author_name,
        mime_type,
        drive_id,
        source_created_at,
        source_modified_at,
        metadata
    )
    with (
        key_field = 'document_id',
        text_fields = '{
            "document_id": {
                "tokenizer": {"type": "keyword"}
            },
            "file_id": {
                "tokenizer": {"type": "keyword"}
            },
            "chunk_id": {
                "tokenizer": {"type": "keyword"}
            },
            "provider_author_id": {
                "tokenizer": {"type": "keyword"}
            },
            "drive_id": {
                "tokenizer": {"type": "keyword"}
            }
        }'
    );

create index idx_granola_context_documents_bm25
    on granola_context_documents
    using bm25 (
        document_id,
        note_id,
        title,
        body,
        url,
        owner_id,
        owner_email,
        owner_name,
        occurred_at,
        source_updated_at,
        metadata
    )
    with (
        key_field = 'document_id',
        text_fields = '{
            "document_id": {
                "tokenizer": {"type": "keyword"}
            },
            "note_id": {
                "tokenizer": {"type": "keyword"}
            },
            "owner_id": {
                "tokenizer": {"type": "keyword"}
            },
            "owner_email": {
                "tokenizer": {"type": "keyword"}
            }
        }'
    );

create index idx_slack_private_context_documents_bm25
    on slack_private_context_documents
    using bm25 (
        document_id,
        title,
        body,
        home_team_id,
        conversation_id,
        conversation_type,
        user_id,
        bot_id,
        message_type,
        message_subtype,
        occurred_at,
        source_updated_at,
        metadata
    )
    with (
        key_field = 'document_id',
        text_fields = '{
            "document_id": {
                "tokenizer": {"type": "keyword"}
            },
            "home_team_id": {
                "tokenizer": {"type": "keyword"}
            },
            "conversation_id": {
                "tokenizer": {"type": "keyword"}
            },
            "user_id": {
                "tokenizer": {"type": "keyword"}
            },
            "bot_id": {
                "tokenizer": {"type": "keyword"}
            }
        }'
    );

create index idx_slack_private_conversation_context_documents_bm25
    on slack_private_conversation_context_documents
    using bm25 (
        document_id,
        title,
        body,
        home_team_id,
        conversation_id,
        conversation_type,
        last_seen_at,
        source_updated_at,
        metadata
    )
    with (
        key_field = 'document_id',
        text_fields = '{
            "document_id": {
                "tokenizer": {"type": "keyword"}
            },
            "home_team_id": {
                "tokenizer": {"type": "keyword"}
            },
            "conversation_id": {
                "tokenizer": {"type": "keyword"}
            }
        }'
    );
