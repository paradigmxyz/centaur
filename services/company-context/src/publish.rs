//! Ingestion API: producers publish public documents to a source, which every
//! principal can then retrieve through the query endpoints.
//!
//! Every write is authenticated with the configured ingestion API key, sent in
//! the `X-Api-Key` header. A full sync replaces a source's documents:
//! documents not published during the sync are removed when it completes.

use std::sync::Arc;

use anyhow::{Context, Result};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, State, rejection::JsonRejection},
    http::HeaderMap,
    routing::{post, put},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Postgres, Transaction};
use tracing::info;
use uuid::Uuid;

use crate::{
    config::{PUBLISHED_DOCUMENT_ID_PREFIX, is_source_name},
    embeddings::EmbeddingsClient,
    extraction::{chunk_text, hex_sha256},
    query::{ApiError, internal, invalid},
};

const MAX_EXTERNAL_ID_BYTES: usize = 512;
const MAX_TITLE_CHARS: usize = 1_000;
const MAX_AUTHOR_CHARS: usize = 500;
const MAX_URL_CHARS: usize = 2_048;
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
        .route("/sources/{source}/syncs", post(handle_start_sync))
        .route(
            "/sources/{source}/syncs/{sync_id}/complete",
            post(handle_complete_sync),
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
    pub url: Option<String>,
    pub author: Option<String>,
    pub occurred_at: Option<DateTime<Utc>>,
    /// The full sync this publication belongs to.
    pub sync_id: Option<String>,
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

async fn handle_start_sync(
    State(state): State<PublishState>,
    headers: HeaderMap,
    Path(source): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    authorize(state.api_key.as_deref(), &headers)?;
    let sync_id = start_sync(&state.pool, &source)
        .await
        .map_err(internal("company_context_sync_start_failed"))?;
    Ok(Json(json!({ "sync_id": sync_id })))
}

async fn handle_complete_sync(
    State(state): State<PublishState>,
    headers: HeaderMap,
    Path((source, sync_id)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>, ApiError> {
    authorize(state.api_key.as_deref(), &headers)?;
    let removed = complete_sync(&state.pool, &source, &sync_id)
        .await
        .map_err(internal("company_context_sync_complete_failed"))?;
    Ok(Json(json!({ "removed": removed })))
}

/// Serializes completing a source's full sync against every other write to
/// the source: completions and sync starts lock it exclusively, publications
/// and deletions share it.
async fn lock_source(
    tx: &mut Transaction<'_, Postgres>,
    source: &str,
    exclusive: bool,
) -> Result<()> {
    let function = if exclusive {
        "pg_advisory_xact_lock"
    } else {
        "pg_advisory_xact_lock_shared"
    };
    sqlx::query(&format!(
        "SELECT {function}(hashtextextended('company_context.published:' || $1, 0))"
    ))
    .bind(source)
    .execute(&mut **tx)
    .await?;
    Ok(())
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

fn parse_sync_id(sync_id: &str) -> Result<Uuid> {
    Uuid::parse_str(sync_id).map_err(|_| invalid("sync_id must be a UUID"))
}

/// A document's fields after validation.
struct Fields<'a> {
    title: &'a str,
    body: &'a str,
    url: &'a str,
    author: &'a str,
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
        let author = request.author.as_deref().unwrap_or_default().trim();
        if author.chars().count() > MAX_AUTHOR_CHARS {
            return Err(invalid(format!(
                "author must be at most {MAX_AUTHOR_CHARS} characters"
            )));
        }
        let url = request.url.as_deref().unwrap_or_default().trim();
        if !url.is_empty()
            && (url.chars().count() > MAX_URL_CHARS
                || !url::Url::parse(url).is_ok_and(|url| matches!(url.scheme(), "http" | "https")))
        {
            return Err(invalid(format!(
                "url must be an http or https URL of at most {MAX_URL_CHARS} characters"
            )));
        }
        Ok(Self {
            title,
            body,
            url,
            author,
            occurred_at: request.occurred_at,
        })
    }

    fn content_hash(&self) -> String {
        hex_sha256(
            json!([
                self.title,
                self.body,
                self.url,
                self.author,
                self.occurred_at
            ])
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

/// Checks that the sync exists for `source` and has not completed.
async fn check_sync(tx: &mut Transaction<'_, Postgres>, source: &str, sync_id: Uuid) -> Result<()> {
    let completed: Option<bool> = sqlx::query_scalar(
        r#"
        SELECT completed_at IS NOT NULL
        FROM company_context_system.published_syncs
        WHERE sync_id = $1::text::uuid AND source = $2
        "#,
    )
    .bind(sync_id.to_string())
    .bind(source)
    .fetch_optional(&mut **tx)
    .await?;
    match completed {
        None => Err(ApiError::NotFound("sync not found").into()),
        Some(true) => Err(ApiError::Conflict("sync already completed".to_owned()).into()),
        Some(false) => Ok(()),
    }
}

/// Publishes a document, replacing any previous version. Unchanged documents
/// are only recorded as part of the sync, without being embedded again.
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
    let sync_id = request.sync_id.as_deref().map(parse_sync_id).transpose()?;
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
    // Embed outside the transaction; a document that changes meanwhile is
    // embedded on the next pass.
    let mut chunks = None;
    if current.as_deref() != Some(content_hash.as_str()) {
        chunks = Some(embed_chunks(embeddings, chunk_chars, source, external_id, &fields).await?);
    }
    loop {
        let mut tx = pool.begin().await?;
        lock_source(&mut tx, source, false).await?;
        if let Some(sync_id) = sync_id {
            check_sync(&mut tx, source, sync_id).await?;
        }
        let previous: Option<String> = sqlx::query_scalar(
            r#"
            SELECT content_hash FROM company_context_system.published_items
            WHERE source = $1 AND external_id = $2
            FOR UPDATE
            "#,
        )
        .bind(source)
        .bind(external_id)
        .fetch_optional(&mut *tx)
        .await?;
        if previous.as_deref() == Some(content_hash.as_str()) {
            sqlx::query(
                r#"
                UPDATE company_context_system.published_items
                SET sync_id = $3::text::uuid, updated_at = NOW()
                WHERE source = $1 AND external_id = $2
                "#,
            )
            .bind(source)
            .bind(external_id)
            .bind(sync_id.map(|id| id.to_string()))
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
            return Ok(PublishResponse {
                status: PublishStatus::Unchanged,
                chunks: 0,
            });
        }
        let Some(chunks) = &chunks else {
            // Republished concurrently with other content than was read.
            tx.rollback().await?;
            chunks =
                Some(embed_chunks(embeddings, chunk_chars, source, external_id, &fields).await?);
            continue;
        };
        write_document(
            &mut tx,
            embeddings,
            source,
            external_id,
            &fields,
            &content_hash,
            sync_id,
            chunks,
        )
        .await?;
        tx.commit().await?;
        info!(
            event = "company_context_document_published",
            source,
            chunks = chunks.len()
        );
        return Ok(PublishResponse {
            status: PublishStatus::Published,
            chunks: chunks.len(),
        });
    }
}

#[allow(clippy::too_many_arguments)]
async fn write_document(
    tx: &mut Transaction<'_, Postgres>,
    embeddings: &EmbeddingsClient,
    source: &str,
    external_id: &str,
    fields: &Fields<'_>,
    content_hash: &str,
    sync_id: Option<Uuid>,
    chunks: &[Chunk],
) -> Result<()> {
    sqlx::query(
        r#"
        INSERT INTO company_context_system.published_items
            (source, external_id, content_hash, sync_id)
        VALUES ($1, $2, $3, $4::text::uuid)
        ON CONFLICT (source, external_id) DO UPDATE
        SET content_hash = EXCLUDED.content_hash,
            sync_id = EXCLUDED.sync_id,
            updated_at = NOW()
        "#,
    )
    .bind(source)
    .bind(external_id)
    .bind(content_hash)
    .bind(sync_id.map(|id| id.to_string()))
    .execute(&mut **tx)
    .await?;
    sqlx::query(
        "DELETE FROM company_context_data.published_documents WHERE source = $1 AND external_id = $2",
    )
    .bind(source)
    .bind(external_id)
    .execute(&mut **tx)
    .await?;
    for (document_id, chunk_id, body, chunk_hash, vector) in chunks {
        sqlx::query(
            r#"
            INSERT INTO company_context_data.published_documents (
                document_id, source, external_id, chunk_id, title, body, url,
                author, occurred_at, content_hash
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
            "#,
        )
        .bind(document_id)
        .bind(source)
        .bind(external_id)
        .bind(chunk_id)
        .bind(fields.title)
        .bind(body)
        .bind(fields.url)
        .bind(fields.author)
        .bind(fields.occurred_at)
        .bind(chunk_hash)
        .execute(&mut **tx)
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
        .execute(&mut **tx)
        .await
        .context("insert published document embedding")?;
    }
    Ok(())
}

/// Removes a published document. Returns whether it existed.
pub async fn delete(pool: &PgPool, source: &str, external_id: &str) -> Result<bool> {
    validate_source(source)?;
    validate_external_id(external_id)?;
    let mut tx = pool.begin().await?;
    lock_source(&mut tx, source, false).await?;
    let deleted = sqlx::query(
        "DELETE FROM company_context_system.published_items WHERE source = $1 AND external_id = $2",
    )
    .bind(source)
    .bind(external_id)
    .execute(&mut *tx)
    .await?
    .rows_affected()
        > 0;
    tx.commit().await?;
    Ok(deleted)
}

/// Starts a full sync of `source`, superseding any sync in progress.
pub async fn start_sync(pool: &PgPool, source: &str) -> Result<String> {
    validate_source(source)?;
    let sync_id = Uuid::new_v4().to_string();
    let mut tx = pool.begin().await?;
    lock_source(&mut tx, source, true).await?;
    // clock_timestamp orders syncs by when they took the lock.
    sqlx::query(
        r#"
        INSERT INTO company_context_system.published_syncs (sync_id, source, started_at)
        VALUES ($1::text::uuid, $2, clock_timestamp())
        "#,
    )
    .bind(&sync_id)
    .bind(source)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(sync_id)
}

/// Completes the latest full sync of `source`, removing the source's documents
/// it did not publish. Returns the number removed. Completing an already
/// completed sync removes nothing.
pub async fn complete_sync(pool: &PgPool, source: &str, sync_id: &str) -> Result<u64> {
    validate_source(source)?;
    let sync_id = parse_sync_id(sync_id)?.to_string();
    let mut tx = pool.begin().await?;
    lock_source(&mut tx, source, true).await?;
    let latest: Option<(String, bool)> = sqlx::query_as(
        r#"
        SELECT sync_id::text, completed_at IS NOT NULL
        FROM company_context_system.published_syncs
        WHERE source = $1
        ORDER BY started_at DESC
        LIMIT 1
        "#,
    )
    .bind(source)
    .fetch_optional(&mut *tx)
    .await?;
    let completed = match latest {
        Some((latest, completed)) if latest == sync_id => completed,
        _ => {
            let exists: bool = sqlx::query_scalar(
                r#"
                SELECT EXISTS (
                    SELECT 1 FROM company_context_system.published_syncs
                    WHERE sync_id = $1::text::uuid AND source = $2
                )
                "#,
            )
            .bind(&sync_id)
            .bind(source)
            .fetch_one(&mut *tx)
            .await?;
            return Err(if exists {
                ApiError::Conflict("a newer sync has started".to_owned())
            } else {
                ApiError::NotFound("sync not found")
            }
            .into());
        }
    };
    if completed {
        return Ok(0);
    }
    let removed = sqlx::query(
        r#"
        DELETE FROM company_context_system.published_items
        WHERE source = $1 AND sync_id IS DISTINCT FROM $2::text::uuid
        "#,
    )
    .bind(source)
    .bind(&sync_id)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    sqlx::query(
        "UPDATE company_context_system.published_syncs SET completed_at = NOW() WHERE sync_id = $1::text::uuid",
    )
    .bind(&sync_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    info!(event = "company_context_sync_completed", source, removed);
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use std::env;

    use axum::http::HeaderValue;
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

    fn api_error(error: anyhow::Error) -> ApiError {
        error.downcast::<ApiError>().unwrap()
    }

    #[tokio::test]
    async fn published_documents_are_public_and_replaced_by_full_syncs() {
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
            "author": "Ada",
            "url": "https://notes.example.com/1",
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
        assert_eq!(document.url.as_deref(), Some("https://notes.example.com/1"));
        assert_eq!(
            document.metadata,
            json!({ "source": "notes", "external_id": "note/1", "author": "Ada" })
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

        // A full sync removes the documents it did not publish.
        let sync_id = start_sync(pool, "notes").await.unwrap();
        let in_sync = |mut body: Value| {
            body["sync_id"] = json!(sync_id);
            body
        };
        assert_eq!(
            publish("note/1", in_sync(changed.clone()))
                .await
                .unwrap()
                .status,
            PublishStatus::Unchanged
        );
        assert_eq!(complete_sync(pool, "notes", &sync_id).await.unwrap(), 1);
        assert_eq!(complete_sync(pool, "notes", &sync_id).await.unwrap(), 0);
        assert_eq!(published().await.unwrap(), ["falcon retro\n\nsecond part"]);
        assert!(matches!(
            api_error(
                publish("note/3", in_sync(json!({ "body": "late" })))
                    .await
                    .unwrap_err()
            ),
            ApiError::Conflict(_)
        ));

        // Only the latest sync of the source completes.
        let superseded = start_sync(pool, "notes").await.unwrap();
        start_sync(pool, "notes").await.unwrap();
        assert!(matches!(
            api_error(complete_sync(pool, "notes", &superseded).await.unwrap_err()),
            ApiError::Conflict(_)
        ));
        assert!(matches!(
            api_error(complete_sync(pool, "wiki", &superseded).await.unwrap_err()),
            ApiError::NotFound(_)
        ));

        assert!(delete(pool, "notes", "note/1").await.unwrap());
        assert!(!delete(pool, "notes", "note/1").await.unwrap());
        assert!(search(pool, json!({ "query": "falcon" })).await.is_empty());

        for invalid in [
            json!({ "body": "  " }),
            json!({ "body": "x", "url": "javascript:alert(1)" }),
            json!({ "body": "x", "sync_id": "not-a-uuid" }),
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
