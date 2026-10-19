//! Ingestion API: producers publish public documents to a source, which every
//! principal can then retrieve through the query endpoints.
//!
//! Every write is authenticated with the configured ingestion API key, sent in
//! the `X-Api-Key` header.

use std::sync::Arc;

use anyhow::{Context, Result};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, State, rejection::JsonRejection},
    http::HeaderMap,
    routing::put,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use tracing::info;

use crate::{
    config::{PUBLISHED_DOCUMENT_ID_PREFIX, is_source_name},
    embeddings::EmbeddingsClient,
    extraction::{chunk_text, hex_sha256},
    query::{ApiError, internal, invalid},
};

const MAX_EXTERNAL_ID_BYTES: usize = 512;
const MAX_TITLE_CHARS: usize = 1_000;
const MAX_METADATA_BYTES: usize = 65_536;
const MAX_BODY_BYTES: usize = 1_000_000;
/// Leaves room for JSON escaping of a body at the limit.
const MAX_REQUEST_BYTES: usize = 4 * MAX_BODY_BYTES;

#[derive(Clone)]
pub struct PublishState {
    pub pool: PgPool,
    pub embeddings: EmbeddingsClient,
    /// Key producers must send; `None` disables publishing.
    pub api_key: Option<Arc<str>>,
    pub chunk_chars: usize,
}

pub fn router(state: PublishState) -> Router {
    Router::new()
        .route(
            "/sources/{source}/documents/{external_id}",
            put(handle_publish).delete(handle_delete),
        )
        .layer(DefaultBodyLimit::max(MAX_REQUEST_BYTES))
        .with_state(state)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublishRequest {
    #[serde(default)]
    pub title: String,
    pub body: String,
    /// Producer-defined fields, such as a link or author.
    #[serde(default)]
    pub metadata: Map<String, Value>,
    pub occurred_at: Option<DateTime<Utc>>,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PublishStatus {
    Published,
    Unchanged,
}

#[derive(Debug, Serialize)]
pub struct PublishResponse {
    pub status: PublishStatus,
    pub chunks: usize,
}

const API_KEY_HEADER: &str = "x-api-key";

/// Rejects requests without the ingestion API key.
fn authorize(api_key: Option<&str>, headers: &HeaderMap) -> Result<(), ApiError> {
    let Some(api_key) = api_key else {
        return Err(ApiError::Forbidden("publishing is not configured"));
    };
    let provided = headers
        .get(API_KEY_HEADER)
        .ok_or(ApiError::Unauthorized("missing API key"))?;
    // Comparing digests keeps the comparison time independent of the key.
    if Sha256::digest(provided.as_bytes()) != Sha256::digest(api_key.as_bytes()) {
        return Err(ApiError::Unauthorized("invalid API key"));
    }
    Ok(())
}

async fn handle_publish(
    State(state): State<PublishState>,
    headers: HeaderMap,
    Path((source, external_id)): Path<(String, String)>,
    request: Result<Json<PublishRequest>, JsonRejection>,
) -> Result<Json<PublishResponse>, ApiError> {
    // Authorize before looking at the body.
    authorize(state.api_key.as_deref(), &headers)?;
    let Json(request) = request.map_err(|rejection| ApiError::BadRequest(rejection.body_text()))?;
    let response = publish(
        &state.pool,
        &state.embeddings,
        state.chunk_chars,
        &source,
        &external_id,
        &request,
    )
    .await
    .map_err(internal("company_context_publish_failed"))?;
    Ok(Json(response))
}

async fn handle_delete(
    State(state): State<PublishState>,
    headers: HeaderMap,
    Path((source, external_id)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, ApiError> {
    authorize(state.api_key.as_deref(), &headers)?;
    let deleted = delete(&state.pool, &source, &external_id)
        .await
        .map_err(internal("company_context_unpublish_failed"))?;
    Ok(Json(json!({ "deleted": deleted })))
}

fn validate_external_id(external_id: &str) -> Result<()> {
    if external_id.is_empty()
        || external_id.len() > MAX_EXTERNAL_ID_BYTES
        || external_id.chars().any(char::is_control)
    {
        return Err(invalid(format!(
            "document ID must be 1-{MAX_EXTERNAL_ID_BYTES} bytes without control characters"
        )));
    }
    Ok(())
}

fn validate_source(source: &str) -> Result<()> {
    if !is_source_name(source) {
        return Err(invalid("invalid source name"));
    }
    Ok(())
}

/// A document's fields after validation.
struct Fields<'a> {
    title: &'a str,
    body: &'a str,
    metadata: &'a Map<String, Value>,
    occurred_at: Option<DateTime<Utc>>,
}

impl<'a> Fields<'a> {
    fn new(request: &'a PublishRequest) -> Result<Self> {
        let title = request.title.trim();
        if title.chars().count() > MAX_TITLE_CHARS {
            return Err(invalid(format!(
                "title must be at most {MAX_TITLE_CHARS} characters"
            )));
        }
        let body = request.body.trim();
        if body.is_empty() {
            return Err(invalid("body must not be empty"));
        }
        if body.len() > MAX_BODY_BYTES {
            return Err(invalid(format!(
                "body must be at most {MAX_BODY_BYTES} bytes"
            )));
        }
        if serde_json::to_vec(&request.metadata)?.len() > MAX_METADATA_BYTES {
            return Err(invalid(format!(
                "metadata must be at most {MAX_METADATA_BYTES} bytes"
            )));
        }
        Ok(Self {
            title,
            body,
            metadata: &request.metadata,
            occurred_at: request.occurred_at,
        })
    }

    fn content_hash(&self) -> String {
        hex_sha256(
            json!([self.title, self.body, self.metadata, self.occurred_at])
                .to_string()
                .as_bytes(),
        )
    }
}

/// A chunk ready to publish: document ID, chunk ID, body, hash, and embedding.
type Chunk = (String, String, String, String, Vec<f32>);

async fn embed_chunks(
    embeddings: &EmbeddingsClient,
    chunk_chars: usize,
    source: &str,
    external_id: &str,
    fields: &Fields<'_>,
) -> Result<Vec<Chunk>> {
    let chunks = chunk_text(fields.body, chunk_chars);
    let inputs: Vec<String> = chunks
        .iter()
        .map(|chunk| {
            if fields.title.is_empty() {
                chunk.body.clone()
            } else {
                format!("{}\n\n{}", fields.title, chunk.body)
            }
        })
        .collect();
    let vectors = embeddings.embed(&inputs).await?;
    Ok(chunks
        .into_iter()
        .zip(inputs)
        .zip(vectors)
        .map(|((chunk, input), vector)| {
            (
                format!(
                    "{PUBLISHED_DOCUMENT_ID_PREFIX}{source}:{external_id}:{}",
                    chunk.chunk_id
                ),
                chunk.chunk_id,
                chunk.body,
                hex_sha256(input.as_bytes()),
                vector,
            )
        })
        .collect())
}

/// Publishes a document, replacing any previous version. Republishing
/// unchanged fields does not embed the document again.
pub async fn publish(
    pool: &PgPool,
    embeddings: &EmbeddingsClient,
    chunk_chars: usize,
    source: &str,
    external_id: &str,
    request: &PublishRequest,
) -> Result<PublishResponse> {
    validate_source(source)?;
    validate_external_id(external_id)?;
    let fields = Fields::new(request)?;
    let content_hash = fields.content_hash();
    let current: Option<String> = sqlx::query_scalar(
        r#"
        SELECT content_hash FROM company_context_system.published_items
        WHERE source = $1 AND external_id = $2
        "#,
    )
    .bind(source)
    .bind(external_id)
    .fetch_optional(pool)
    .await?;
    if current.as_deref() == Some(content_hash.as_str()) {
        return Ok(PublishResponse {
            status: PublishStatus::Unchanged,
            chunks: 0,
        });
    }

    // Embed outside the transaction. Concurrent publications of a document
    // serialize on its item row; the last to commit wins.
    let chunks = embed_chunks(embeddings, chunk_chars, source, external_id, &fields).await?;
    let mut tx = pool.begin().await?;
    sqlx::query(
        r#"
        INSERT INTO company_context_system.published_items
            (source, external_id, content_hash)
        VALUES ($1, $2, $3)
        ON CONFLICT (source, external_id) DO UPDATE
        SET content_hash = EXCLUDED.content_hash,
            updated_at = NOW()
        "#,
    )
    .bind(source)
    .bind(external_id)
    .bind(&content_hash)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "DELETE FROM company_context_data.published_documents WHERE source = $1 AND external_id = $2",
    )
    .bind(source)
    .bind(external_id)
    .execute(&mut *tx)
    .await?;
    for (document_id, chunk_id, body, chunk_hash, vector) in &chunks {
        sqlx::query(
            r#"
            INSERT INTO company_context_data.published_documents (
                document_id, source, external_id, chunk_id, title, body,
                metadata, occurred_at, content_hash
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
            "#,
        )
        .bind(document_id)
        .bind(source)
        .bind(external_id)
        .bind(chunk_id)
        .bind(fields.title)
        .bind(body)
        .bind(sqlx::types::Json(fields.metadata))
        .bind(fields.occurred_at)
        .bind(chunk_hash)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            r#"
            INSERT INTO company_context_data.published_document_embeddings (
                document_id, model, dimensions, content_hash, embedding
            )
            VALUES ($1, $2, $3, $4, $5::vector)
            "#,
        )
        .bind(document_id)
        .bind(embeddings.model())
        .bind(embeddings.dimensions() as i32)
        .bind(chunk_hash)
        .bind(serde_json::to_string(vector)?)
        .execute(&mut *tx)
        .await
        .context("insert published document embedding")?;
    }
    tx.commit().await?;
    info!(
        event = "company_context_document_published",
        source,
        chunks = chunks.len()
    );
    Ok(PublishResponse {
        status: PublishStatus::Published,
        chunks: chunks.len(),
    })
}

/// Removes a published document. Returns whether it existed.
pub async fn delete(pool: &PgPool, source: &str, external_id: &str) -> Result<bool> {
    validate_source(source)?;
    validate_external_id(external_id)?;
    let deleted = sqlx::query(
        "DELETE FROM company_context_system.published_items WHERE source = $1 AND external_id = $2",
    )
    .bind(source)
    .bind(external_id)
    .execute(pool)
    .await?
    .rows_affected();
    Ok(deleted > 0)
}

#[cfg(test)]
mod tests {
    use std::env;

    use axum::{http::HeaderValue, routing::post};
    use clap::Parser;
    use serde_json::Value;

    use super::*;
    use crate::{
        config::Config,
        credentials::PrincipalIdentity,
        query::{self, QueryRequest},
        test_support::TestDatabase,
    };

    fn config(extra: &[&str]) -> Config {
        let mut args = vec![
            "centaur-company-context",
            "--database-url",
            "postgresql://context",
            "--console-database-url",
            "postgresql://console",
            "--active-record-primary-key",
            "primary",
            "--active-record-key-derivation-salt",
            "salt",
            "--openai-api-key",
            "test-key",
            "--slack-bot-token",
            "xoxb-test",
            "--jwt-signing-secret",
            "secret",
        ];
        args.extend(extra);
        Config::try_parse_from(args).unwrap()
    }

    /// Serves embeddings for every input until the returned handle is aborted.
    async fn fake_embeddings() -> (EmbeddingsClient, tokio::task::JoinHandle<()>) {
        async fn embed(Json(body): Json<Value>) -> Json<Value> {
            let count = body["input"].as_array().unwrap().len();
            Json(json!({
                "data": (0..count)
                    .map(|index| json!({ "index": index, "embedding": vec![0.5; 1536] }))
                    .collect::<Vec<_>>()
            }))
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, Router::new().route("/embeddings", post(embed)))
                .await
                .unwrap();
        });
        let embeddings = EmbeddingsClient::new(&config(&["--openai-base-url", &base_url])).unwrap();
        (embeddings, server)
    }

    fn request(body: Value) -> PublishRequest {
        serde_json::from_value(body).unwrap()
    }

    /// A principal without any source identity.
    fn anyone() -> PrincipalIdentity {
        PrincipalIdentity {
            slack_user_id: None,
            google_subjects: Vec::new(),
            granola_subjects: Vec::new(),
        }
    }

    async fn search(pool: &PgPool, body: Value) -> Vec<(String, String)> {
        let request: QueryRequest = serde_json::from_value(body).unwrap();
        let mut results: Vec<_> = query::search(pool, None, &anyone(), &request)
            .await
            .unwrap()
            .into_iter()
            .map(|result| (result.document.document_id, result.document.text))
            .collect();
        results.sort();
        results
    }

    #[tokio::test]
    async fn published_documents_are_public_replaced_and_deleted() {
        let Ok(database_url) = env::var("COMPANY_CONTEXT_TEST_DATABASE_URL") else {
            eprintln!("skipping: set COMPANY_CONTEXT_TEST_DATABASE_URL to a ParadeDB Postgres URL");
            return;
        };
        let database = TestDatabase::create(&database_url, "publish").await;
        let pool = &database.pool;
        let (embeddings, server) = fake_embeddings().await;
        let publish = |external_id: &'static str, body: Value| {
            let embeddings = embeddings.clone();
            async move {
                publish(
                    pool,
                    &embeddings,
                    6000,
                    "notes",
                    external_id,
                    &request(body),
                )
                .await
            }
        };

        let note = json!({
            "title": "Falcon",
            "body": "falcon launch retro",
            "metadata": { "author": "Ada", "url": "https://notes.example.com/1" },
            "occurred_at": "2020-01-02T00:00:00Z",
        });
        assert_eq!(
            publish("note/1", note.clone()).await.unwrap().status,
            PublishStatus::Published
        );
        assert_eq!(
            publish("note/1", note.clone()).await.unwrap().status,
            PublishStatus::Unchanged
        );
        // Any principal finds and reads it, even without source identities.
        assert_eq!(
            search(pool, json!({ "query": "falcon" })).await,
            [(
                "published:notes:note/1:000000".to_owned(),
                "falcon launch retro".to_owned()
            )]
        );
        let document = query::document(pool, &anyone(), "published:notes:note/1:000000")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            document.metadata,
            json!({
                "source": "notes",
                "external_id": "note/1",
                "author": "Ada",
                "url": "https://notes.example.com/1",
            })
        );
        assert!(
            search(
                pool,
                json!({ "query": "falcon", "filters": { "sources": ["wiki"] } })
            )
            .await
            .is_empty()
        );
        let error = query::search(
            pool,
            None,
            &anyone(),
            &serde_json::from_value(
                json!({ "query": "falcon", "filters": { "types": ["drive_doc"], "sources": ["notes"] } }),
            )
            .unwrap(),
        )
        .await
        .unwrap_err();
        assert!(error.is::<query::InvalidQuery>());

        // A changed document replaces its previous chunks.
        let changed = json!({ "title": "Falcon", "body": "falcon retro\n\nsecond part" });
        let published = || {
            sqlx::query_scalar::<_, String>(
                "SELECT body FROM company_context_data.published_documents ORDER BY document_id",
            )
            .fetch_all(pool)
        };
        assert_eq!(
            publish("note/1", changed.clone()).await.unwrap().status,
            PublishStatus::Published
        );
        assert_eq!(published().await.unwrap(), ["falcon retro\n\nsecond part"]);
        publish("note/2", json!({ "body": "falcon budget" }))
            .await
            .unwrap();

        assert!(delete(pool, "notes", "note/1").await.unwrap());
        assert!(!delete(pool, "notes", "note/1").await.unwrap());
        assert_eq!(
            search(pool, json!({ "query": "falcon" })).await,
            [(
                "published:notes:note/2:000000".to_owned(),
                "falcon budget".to_owned()
            )]
        );

        for invalid in [
            json!({ "body": "  " }),
            json!({ "body": "x", "metadata": { "notes": "n".repeat(65_536) } }),
            json!({ "body": "x", "title": "t".repeat(1_001) }),
        ] {
            let error = publish("note/4", invalid.clone()).await.unwrap_err();
            assert!(error.is::<query::InvalidQuery>(), "{invalid}");
        }

        server.abort();
        database.drop().await;
    }

    #[test]
    fn writes_require_the_ingestion_api_key() {
        let headers = |key: &str| {
            let mut headers = HeaderMap::new();
            headers.insert(API_KEY_HEADER, HeaderValue::from_str(key).unwrap());
            headers
        };
        let key = "k".repeat(32);

        assert!(authorize(Some(&key), &headers(&key)).is_ok());
        assert!(matches!(
            authorize(Some(&key), &headers(&"x".repeat(32))),
            Err(ApiError::Unauthorized(_))
        ));
        assert!(matches!(
            authorize(Some(&key), &HeaderMap::new()),
            Err(ApiError::Unauthorized(_))
        ));
        assert!(matches!(
            authorize(None, &headers(&key)),
            Err(ApiError::Forbidden(_))
        ));
    }
}
