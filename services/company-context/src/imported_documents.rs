//! Projection of imported documents: rows operators write to
//! `company_context_system.imported_documents` are chunked, embedded, and
//! published to `company_context_data` for retrieval. Each run projects rows
//! whose chunks are missing or were projected from other content; deleting a
//! row removes its chunks through a cascading foreign key.

use absurd::TaskContext;
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{PgPool, types::Json};
use tracing::{info, warn};

use crate::{
    config::{IMPORTED_DOCUMENT_ID_PREFIX, IMPORTED_PROJECT_TASK},
    embeddings::EmbeddingsClient,
    errors::is_rejected,
    extraction::{chunk_text, hex_sha256},
    tasks::{TaskState, bounded_error, run_task},
};

/// Rows embedded together.
const BATCH_SIZE: i64 = 50;
/// Bounds a run; the next run continues with the remaining rows.
const MAX_BATCHES_PER_RUN: usize = 20;

#[derive(Debug, Deserialize, Serialize)]
pub struct ImportedProjectParams {
    pub bucket: u64,
}

#[derive(Debug, Default, PartialEq, Eq, Serialize)]
pub struct ProjectSummary {
    pub projected: usize,
    pub rejected: usize,
}

pub fn register(state: &TaskState) -> Result<()> {
    let project_state = state.clone();
    state.absurd.register_task(
        IMPORTED_PROJECT_TASK,
        move |params: ImportedProjectParams, ctx| {
            let state = project_state.clone();
            async move { run_task(&ctx, project_task(&state, params, &ctx)).await }
        },
    )?;
    Ok(())
}

async fn project_task(
    state: &TaskState,
    params: ImportedProjectParams,
    ctx: &TaskContext,
) -> Result<ProjectSummary> {
    let summary = project(&state.pool, &state.embeddings, state.config.chunk_chars).await?;
    info!(
        event = "company_context_imported_documents_projected",
        task_id = ctx.task_id(),
        bucket = params.bucket,
        projected = summary.projected,
        rejected = summary.rejected
    );
    Ok(summary)
}

#[derive(sqlx::FromRow)]
struct Pending {
    source: String,
    external_id: String,
    title: String,
    body: String,
    occurred_at: Option<DateTime<Utc>>,
    metadata: Json<Value>,
    content_hash: String,
}

/// A chunk ready to embed: document ID, chunk ID, body, and embedding input.
struct Chunk {
    document_id: String,
    chunk_id: String,
    body: String,
    input: String,
}

impl Pending {
    fn chunks(&self, chunk_chars: usize) -> Vec<Chunk> {
        chunk_text(&self.body, chunk_chars)
            .into_iter()
            .map(|chunk| Chunk {
                document_id: format!(
                    "{IMPORTED_DOCUMENT_ID_PREFIX}{}:{}:{}",
                    self.source, self.external_id, chunk.chunk_id
                ),
                input: if self.title.is_empty() {
                    chunk.body.clone()
                } else {
                    format!("{}\n\n{}", self.title, chunk.body)
                },
                chunk_id: chunk.chunk_id,
                body: chunk.body,
            })
            .collect()
    }
}

/// Projects rows whose chunks are missing or stale, skipping rows the
/// embeddings API rejected in their current form.
pub async fn project(
    pool: &PgPool,
    embeddings: &EmbeddingsClient,
    chunk_chars: usize,
) -> Result<ProjectSummary> {
    let mut summary = ProjectSummary::default();
    for _ in 0..MAX_BATCHES_PER_RUN {
        let batch: Vec<Pending> = sqlx::query_as(
            r#"
            SELECT i.source, i.external_id, i.title, i.body, i.occurred_at,
                   i.metadata, i.content_hash
            FROM company_context_system.imported_documents i
            WHERE NOT EXISTS (
                SELECT 1 FROM company_context_data.imported_document_chunks c
                WHERE c.source = i.source
                  AND c.external_id = i.external_id
                  AND c.item_hash = i.content_hash
            )
              AND NOT EXISTS (
                SELECT 1 FROM company_context_system.imported_document_rejections r
                WHERE r.source = i.source
                  AND r.external_id = i.external_id
                  AND r.content_hash = i.content_hash
            )
            ORDER BY i.updated_at, i.source, i.external_id
            LIMIT $1
            "#,
        )
        .bind(BATCH_SIZE)
        .fetch_all(pool)
        .await?;
        let done = (batch.len() as i64) < BATCH_SIZE;
        let items: Vec<(Pending, Vec<Chunk>)> = batch
            .into_iter()
            .map(|item| {
                let chunks = item.chunks(chunk_chars);
                (item, chunks)
            })
            .collect();
        let inputs: Vec<String> = items
            .iter()
            .flat_map(|(_, chunks)| chunks.iter().map(|chunk| chunk.input.clone()))
            .collect();
        match embeddings.embed(&inputs).await {
            Ok(vectors) => {
                let mut vectors = vectors.into_iter();
                for (item, chunks) in &items {
                    let item_vectors: Vec<Vec<f32>> = vectors.by_ref().take(chunks.len()).collect();
                    if write(pool, embeddings, item, chunks, &item_vectors).await? {
                        summary.projected += 1;
                    }
                }
            }
            // Embed rows one at a time to find the ones rejected.
            Err(error) if is_rejected(&error) => {
                for (item, chunks) in &items {
                    let inputs: Vec<String> =
                        chunks.iter().map(|chunk| chunk.input.clone()).collect();
                    match embeddings.embed(&inputs).await {
                        Ok(vectors) => {
                            if write(pool, embeddings, item, chunks, &vectors).await? {
                                summary.projected += 1;
                            }
                        }
                        Err(error) if is_rejected(&error) => {
                            reject(pool, item, &error).await?;
                            summary.rejected += 1;
                        }
                        Err(error) => return Err(error),
                    }
                }
            }
            Err(error) => return Err(error),
        }
        if done {
            break;
        }
    }
    Ok(summary)
}

/// Replaces the row's chunks unless the row changed or was deleted since it
/// was read. Returns whether the chunks were written.
async fn write(
    pool: &PgPool,
    embeddings: &EmbeddingsClient,
    item: &Pending,
    chunks: &[Chunk],
    vectors: &[Vec<f32>],
) -> Result<bool> {
    if vectors.len() != chunks.len() {
        anyhow::bail!("embeddings response omitted a chunk");
    }
    let mut tx = pool.begin().await?;
    // Holds off concurrent updates and deletes of the row until committed.
    let current: Option<String> = sqlx::query_scalar(
        r#"
        SELECT content_hash FROM company_context_system.imported_documents
        WHERE source = $1 AND external_id = $2
        FOR SHARE
        "#,
    )
    .bind(&item.source)
    .bind(&item.external_id)
    .fetch_optional(&mut *tx)
    .await?;
    if current.as_deref() != Some(item.content_hash.as_str()) {
        tx.rollback().await?;
        return Ok(false);
    }
    sqlx::query(
        r#"
        DELETE FROM company_context_data.imported_document_chunks
        WHERE source = $1 AND external_id = $2
        "#,
    )
    .bind(&item.source)
    .bind(&item.external_id)
    .execute(&mut *tx)
    .await?;
    for (chunk, vector) in chunks.iter().zip(vectors) {
        let content_hash = hex_sha256(chunk.input.as_bytes());
        sqlx::query(
            r#"
            INSERT INTO company_context_data.imported_document_chunks (
                document_id, source, external_id, chunk_id, title, body,
                occurred_at, metadata, item_hash, content_hash
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
            "#,
        )
        .bind(&chunk.document_id)
        .bind(&item.source)
        .bind(&item.external_id)
        .bind(&chunk.chunk_id)
        .bind(&item.title)
        .bind(&chunk.body)
        .bind(item.occurred_at)
        .bind(&item.metadata)
        .bind(&item.content_hash)
        .bind(&content_hash)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            r#"
            INSERT INTO company_context_data.imported_document_chunk_embeddings (
                document_id, model, dimensions, content_hash, embedding
            )
            VALUES ($1, $2, $3, $4, $5::vector)
            "#,
        )
        .bind(&chunk.document_id)
        .bind(embeddings.model())
        .bind(embeddings.dimensions() as i32)
        .bind(&content_hash)
        .bind(serde_json::to_string(vector)?)
        .execute(&mut *tx)
        .await
        .context("insert imported document chunk embedding")?;
    }
    sqlx::query(
        r#"
        DELETE FROM company_context_system.imported_document_rejections
        WHERE source = $1 AND external_id = $2
        "#,
    )
    .bind(&item.source)
    .bind(&item.external_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(true)
}

/// Records that the row's current content was rejected, so it is not retried
/// until it changes.
async fn reject(pool: &PgPool, item: &Pending, error: &anyhow::Error) -> Result<()> {
    warn!(
        event = "company_context_imported_document_rejected",
        source = item.source,
        error = %error
    );
    // A row changed or deleted meanwhile is projected or removed instead.
    sqlx::query(
        r#"
        INSERT INTO company_context_system.imported_document_rejections
            (source, external_id, content_hash, error)
        SELECT source, external_id, content_hash, $4
        FROM company_context_system.imported_documents
        WHERE source = $1 AND external_id = $2 AND content_hash = $3
        ON CONFLICT (source, external_id) DO UPDATE
        SET content_hash = EXCLUDED.content_hash,
            error = EXCLUDED.error,
            created_at = NOW()
        "#,
    )
    .bind(&item.source)
    .bind(&item.external_id)
    .bind(&item.content_hash)
    .bind(bounded_error(error))
    .execute(pool)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::env;

    use axum::{Json as AxumJson, Router, http::StatusCode, routing::post};
    use clap::Parser;
    use serde_json::json;
    use sqlx::Executor;

    use super::*;
    use crate::{
        config::Config,
        credentials::PrincipalIdentity,
        query::{self, QueryRequest},
        test_support::TestDatabase,
    };

    /// Serves embeddings, rejecting batches with an input containing "REJECT".
    async fn fake_embeddings() -> (EmbeddingsClient, tokio::task::JoinHandle<()>) {
        async fn embed(AxumJson(body): AxumJson<Value>) -> (StatusCode, AxumJson<Value>) {
            let inputs = body["input"].as_array().unwrap();
            if inputs
                .iter()
                .any(|input| input.as_str().unwrap().contains("REJECT"))
            {
                return (StatusCode::BAD_REQUEST, AxumJson(json!({})));
            }
            let data: Vec<Value> = (0..inputs.len())
                .map(|index| json!({ "index": index, "embedding": vec![0.5; 1536] }))
                .collect();
            (StatusCode::OK, AxumJson(json!({ "data": data })))
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, Router::new().route("/embeddings", post(embed)))
                .await
                .unwrap();
        });
        let config = Config::try_parse_from([
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
            "--openai-base-url",
            &base_url,
        ])
        .unwrap();
        (EmbeddingsClient::new(&config).unwrap(), server)
    }

    /// A principal without any source identity.
    fn anyone() -> PrincipalIdentity {
        PrincipalIdentity {
            slack_user_id: None,
            google_subjects: Vec::new(),
            granola_subjects: Vec::new(),
        }
    }

    async fn search(pool: &PgPool, body: Value) -> Result<Vec<(String, String)>> {
        let request: QueryRequest = serde_json::from_value(body).unwrap();
        let mut results: Vec<_> = query::search(pool, None, &anyone(), &request)
            .await?
            .into_iter()
            .map(|result| (result.document.document_id, result.document.text))
            .collect();
        results.sort();
        Ok(results)
    }

    fn summary(projected: usize, rejected: usize) -> ProjectSummary {
        ProjectSummary {
            projected,
            rejected,
        }
    }

    #[tokio::test]
    async fn imported_rows_are_projected_public_and_follow_their_rows() {
        let Ok(database_url) = env::var("COMPANY_CONTEXT_TEST_DATABASE_URL") else {
            eprintln!("skipping: set COMPANY_CONTEXT_TEST_DATABASE_URL to a ParadeDB Postgres URL");
            return;
        };
        let database = TestDatabase::create(&database_url, "imported").await;
        let pool = &database.pool;
        let (embeddings, server) = fake_embeddings().await;
        let project = || project(pool, &embeddings, 6000);

        pool.execute(
            r#"
            INSERT INTO company_context_system.imported_documents
                (source, external_id, title, body, occurred_at, metadata)
            VALUES
                ('archive', 'note/1', 'Falcon', 'falcon launch retro', '2020-01-02Z',
                 '{"author": "Ada", "source": "spoofed"}'),
                ('archive', 'note/2', '', 'falcon budget REJECT', NULL, '{}');
            "#,
        )
        .await
        .unwrap();
        // A rejected row does not hold back the rest of its batch.
        assert_eq!(project().await.unwrap(), summary(1, 1));
        assert_eq!(project().await.unwrap(), summary(0, 0));

        // Any principal finds and reads it, even without source identities.
        assert_eq!(
            search(pool, json!({ "query": "falcon" })).await.unwrap(),
            [(
                "imported:archive:note/1:000000".to_owned(),
                "falcon launch retro".to_owned()
            )]
        );
        let document = query::document(pool, &anyone(), "imported:archive:note/1:000000")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            document.metadata,
            json!({ "author": "Ada", "source": "archive", "external_id": "note/1" })
        );
        assert!(
            search(
                pool,
                json!({ "query": "falcon", "filters": { "sources": ["wiki"] } })
            )
            .await
            .unwrap()
            .is_empty()
        );
        // Metadata filters match the metadata documents are returned with.
        let metadata =
            |metadata: Value| json!({ "query": "falcon", "filters": { "metadata": metadata } });
        for matching in [
            json!({ "author": "Ada" }),
            json!({ "author": "Ada", "source": "archive" }),
            json!({ "external_id": "note/1" }),
        ] {
            assert_eq!(
                search(pool, metadata(matching.clone()))
                    .await
                    .unwrap()
                    .len(),
                1,
                "{matching}"
            );
        }
        for missing in [
            json!({ "author": "Bob" }),
            json!({ "author": "ada" }),
            json!({ "source": "spoofed" }),
        ] {
            assert!(
                search(pool, metadata(missing.clone()))
                    .await
                    .unwrap()
                    .is_empty(),
                "{missing}"
            );
        }
        assert!(
            search(
                pool,
                json!({ "query": "falcon", "filters": {
                    "types": ["drive_doc"],
                    "metadata": { "author": "Ada" },
                } })
            )
            .await
            .unwrap_err()
            .to_string()
            .contains("no requested type supports every filter")
        );

        // Changed rows are projected again, replacing their chunks.
        pool.execute(
            r#"
            UPDATE company_context_system.imported_documents
            SET body = 'falcon retro' || chr(10) || chr(10) || 'second part'
            WHERE external_id = 'note/1';
            UPDATE company_context_system.imported_documents
            SET body = 'falcon budget'
            WHERE external_id = 'note/2';
            "#,
        )
        .await
        .unwrap();
        assert_eq!(project().await.unwrap(), summary(2, 0));
        let chunks: Vec<(String, String)> = sqlx::query_as(
            "SELECT external_id, body FROM company_context_data.imported_document_chunks ORDER BY document_id",
        )
        .fetch_all(pool)
        .await
        .unwrap();
        assert_eq!(
            chunks,
            [
                (
                    "note/1".to_owned(),
                    "falcon retro\n\nsecond part".to_owned()
                ),
                ("note/2".to_owned(), "falcon budget".to_owned())
            ]
        );
        let rejections: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM company_context_system.imported_document_rejections",
        )
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(rejections, 0);

        // Deleting a row removes its chunks and their embeddings at once.
        pool.execute(
            "DELETE FROM company_context_system.imported_documents WHERE external_id = 'note/1'",
        )
        .await
        .unwrap();
        assert_eq!(
            search(pool, json!({ "query": "falcon" })).await.unwrap(),
            [(
                "imported:archive:note/2:000000".to_owned(),
                "falcon budget".to_owned()
            )]
        );
        let embedded: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM company_context_data.imported_document_chunk_embeddings",
        )
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(embedded, 1);

        // Invalid rows are refused on write.
        for invalid in [
            "('Archive', 'x', 'body', '{}')",
            "('archive', '', 'body', '{}')",
            "('archive', 'x', '  ', '{}')",
            "('archive', 'x', 'body', '[]')",
        ] {
            let result = pool
                .execute(
                    format!(
                        "INSERT INTO company_context_system.imported_documents (source, external_id, body, metadata) VALUES {invalid}"
                    )
                    .as_str(),
                )
                .await;
            assert!(result.is_err(), "{invalid}");
        }

        server.abort();
        database.drop().await;
    }
}
