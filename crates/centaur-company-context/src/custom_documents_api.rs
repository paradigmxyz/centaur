//! `/custom-documents`: lets principals create custom documents, such as
//! memories, and list, replace, and delete the ones they created.
//!
//! Any principal can create a document in any source. The service names it
//! by a UUIDv7 and records the principal as its writer; only the writer can
//! reach it through these endpoints, and every other principal gets 404.
//! Readers default to the writer and may name only the writer or its roles,
//! never `public`, so a principal cannot publish to everyone's searches. Rows
//! written directly to the table have no writer and cannot be reached here.
//! Documents are searched and read through `/query` and `/documents`.

use std::time::{SystemTime, UNIX_EPOCH};

use absurd::SpawnOptions;
use axum::{
    Json, Router,
    extract::{
        Path, Query, State,
        rejection::{JsonRejection, QueryRejection},
    },
    http::{HeaderMap, StatusCode},
    routing::{post, put},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sqlx::{PgPool, types::Json as SqlJson};
use tracing::{error, warn};
use uuid::Uuid;

use crate::{
    config::CUSTOM_PROJECT_TASK,
    credentials::PrincipalIdentity,
    custom_documents::CustomProjectParams,
    query::{ApiError, QueryState, authenticate, custom_readers, principal_oid},
    telemetry,
};

const DEFAULT_LIST_LIMIT: i64 = 20;
const MAX_LIST_LIMIT: i64 = 100;

/// A document's columns and projection status, from rows `d`.
const SELECT: &str = r#"
    SELECT d.source, d.external_id, d.title, d.body, d.occurred_at, d.readers,
           d.metadata, d.written_by, d.created_at, d.updated_at,
           CASE
               WHEN EXISTS (
                   SELECT 1 FROM company_context_data.custom_document_chunks c
                   WHERE c.source = d.source
                     AND c.external_id = d.external_id
                     AND c.item_hash = d.content_hash
               ) THEN 'projected'
               WHEN r.error IS NOT NULL THEN 'rejected'
               ELSE 'pending'
           END AS status,
           r.error
    FROM d
    LEFT JOIN company_context_system.custom_document_rejections r
      ON r.source = d.source
     AND r.external_id = d.external_id
     AND r.content_hash = d.content_hash
"#;

pub fn router(state: QueryState) -> Router {
    Router::new()
        .route(
            "/custom-documents/{source}",
            post(handle_create).get(handle_list),
        )
        .route(
            "/custom-documents/{source}/{external_id}",
            put(handle_replace).delete(handle_delete),
        )
        .with_state(state)
}

/// A document's content. Replacing a document replaces all of it.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DocumentRequest {
    #[serde(default)]
    pub title: String,
    pub body: String,
    pub occurred_at: Option<DateTime<Utc>>,
    /// Defaults to the writer alone.
    pub readers: Option<Vec<String>>,
    #[serde(default)]
    pub metadata: Map<String, Value>,
}

impl DocumentRequest {
    /// The table accepts bodies of only whitespace other than spaces, which
    /// have no text to index.
    fn validate(&self) -> Result<(), ApiError> {
        if self.body.trim().is_empty() {
            return Err(ApiError::BadRequest("body must not be blank".to_owned()));
        }
        Ok(())
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListParams {
    pub limit: Option<i64>,
    /// Lists documents created before the one with this ID.
    pub before: Option<String>,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct CustomDocument {
    pub source: String,
    pub external_id: String,
    pub title: String,
    pub body: String,
    pub occurred_at: Option<DateTime<Utc>>,
    pub readers: Vec<String>,
    pub metadata: SqlJson<Value>,
    pub written_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// `pending` until the current content is searchable as `projected`, or
    /// `rejected` with an `error` if it cannot be embedded.
    pub status: String,
    pub error: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ListResponse {
    pub documents: Vec<CustomDocument>,
}

async fn handle_create(
    State(state): State<QueryState>,
    headers: HeaderMap,
    Path(source): Path<String>,
    request: Result<Json<DocumentRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<CustomDocument>), ApiError> {
    // Authenticate before looking at the body.
    let identity = authenticate(&state, &headers).await?;
    let Json(request) = request.map_err(|rejection| ApiError::BadRequest(rejection.body_text()))?;
    let document = create(&state.pool, &identity, &source, &request).await?;
    enqueue_projection(&state, &document).await;
    Ok((StatusCode::CREATED, Json(document)))
}

async fn handle_list(
    State(state): State<QueryState>,
    headers: HeaderMap,
    Path(source): Path<String>,
    params: Result<Query<ListParams>, QueryRejection>,
) -> Result<Json<ListResponse>, ApiError> {
    let identity = authenticate(&state, &headers).await?;
    let Query(params) = params.map_err(|rejection| ApiError::BadRequest(rejection.body_text()))?;
    let documents = list(&state.pool, &identity, &source, &params).await?;
    Ok(Json(ListResponse { documents }))
}

async fn handle_replace(
    State(state): State<QueryState>,
    headers: HeaderMap,
    Path((source, external_id)): Path<(String, String)>,
    request: Result<Json<DocumentRequest>, JsonRejection>,
) -> Result<Json<CustomDocument>, ApiError> {
    let identity = authenticate(&state, &headers).await?;
    let Json(request) = request.map_err(|rejection| ApiError::BadRequest(rejection.body_text()))?;
    let document = replace(&state.pool, &identity, &source, &external_id, &request).await?;
    enqueue_projection(&state, &document).await;
    Ok(Json(document))
}

async fn handle_delete(
    State(state): State<QueryState>,
    headers: HeaderMap,
    Path((source, external_id)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    let identity = authenticate(&state, &headers).await?;
    delete(&state.pool, &identity, &source, &external_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Creates a document written by `identity`, named by a new UUIDv7.
pub async fn create(
    pool: &PgPool,
    identity: &PrincipalIdentity,
    source: &str,
    request: &DocumentRequest,
) -> Result<CustomDocument, ApiError> {
    let source = valid_source(source)?;
    request.validate()?;
    let writer = writer(identity)?;
    let readers = readers(identity, &writer, request.readers.as_deref())?;
    sqlx::query_as(&format!(
        r#"
        WITH d AS (
            INSERT INTO company_context_system.custom_documents
                (source, external_id, title, body, occurred_at, readers, metadata, written_by)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
            RETURNING *
        )
        {SELECT}
        "#
    ))
    .bind(source)
    .bind(Uuid::now_v7().to_string())
    .bind(&request.title)
    .bind(&request.body)
    .bind(request.occurred_at)
    .bind(&readers)
    .bind(SqlJson(&request.metadata))
    .bind(&writer)
    .fetch_one(pool)
    .await
    .map_err(database_error(
        "company_context_custom_document_create_failed",
    ))
}

/// Lists the documents `identity` wrote in `source`, newest first.
pub async fn list(
    pool: &PgPool,
    identity: &PrincipalIdentity,
    source: &str,
    params: &ListParams,
) -> Result<Vec<CustomDocument>, ApiError> {
    let source = valid_source(source)?;
    let limit = params.limit.unwrap_or(DEFAULT_LIST_LIMIT);
    if !(1..=MAX_LIST_LIMIT).contains(&limit) {
        return Err(ApiError::BadRequest(format!(
            "limit must be between 1 and {MAX_LIST_LIMIT}"
        )));
    }
    let before = params
        .before
        .as_deref()
        .map(|before| {
            Uuid::try_parse(before)
                .map(|id| id.to_string())
                .map_err(|_| ApiError::BadRequest("before must be a document ID".to_owned()))
        })
        .transpose()?;
    sqlx::query_as(&format!(
        r#"
        WITH d AS (
            SELECT * FROM company_context_system.custom_documents
            WHERE written_by = $1
              AND source = $2
              AND ($3::text IS NULL OR external_id < $3)
            ORDER BY external_id DESC
            LIMIT $4
        )
        {SELECT}
        ORDER BY d.external_id DESC
        "#
    ))
    .bind(writer(identity)?)
    .bind(source)
    .bind(before)
    .bind(limit)
    .fetch_all(pool)
    .await
    .map_err(database_error(
        "company_context_custom_document_list_failed",
    ))
}

/// Replaces the content and readers of a document `identity` wrote.
pub async fn replace(
    pool: &PgPool,
    identity: &PrincipalIdentity,
    source: &str,
    external_id: &str,
    request: &DocumentRequest,
) -> Result<CustomDocument, ApiError> {
    let source = valid_source(source)?;
    let external_id = document_id(external_id)?;
    request.validate()?;
    let writer = writer(identity)?;
    let readers = readers(identity, &writer, request.readers.as_deref())?;
    sqlx::query_as(&format!(
        r#"
        WITH d AS (
            UPDATE company_context_system.custom_documents
            SET title = $3, body = $4, occurred_at = $5, readers = $6, metadata = $7
            WHERE source = $1 AND external_id = $2 AND written_by = $8
            RETURNING *
        )
        {SELECT}
        "#
    ))
    .bind(source)
    .bind(external_id)
    .bind(&request.title)
    .bind(&request.body)
    .bind(request.occurred_at)
    .bind(&readers)
    .bind(SqlJson(&request.metadata))
    .bind(&writer)
    .fetch_optional(pool)
    .await
    .map_err(database_error(
        "company_context_custom_document_replace_failed",
    ))?
    .ok_or(ApiError::NotFound)
}

/// Deletes a document `identity` wrote, and with it its published chunks.
pub async fn delete(
    pool: &PgPool,
    identity: &PrincipalIdentity,
    source: &str,
    external_id: &str,
) -> Result<(), ApiError> {
    let source = valid_source(source)?;
    let external_id = document_id(external_id)?;
    let deleted = sqlx::query(
        r#"
        DELETE FROM company_context_system.custom_documents
        WHERE source = $1 AND external_id = $2 AND written_by = $3
        "#,
    )
    .bind(source)
    .bind(external_id)
    .bind(writer(identity)?)
    .execute(pool)
    .await
    .map_err(database_error(
        "company_context_custom_document_delete_failed",
    ))?
    .rows_affected();
    if deleted == 0 {
        return Err(ApiError::NotFound);
    }
    Ok(())
}

/// Projects new content right away rather than at the next scheduled run.
/// The scheduled run still projects it if enqueueing fails.
async fn enqueue_projection(state: &QueryState, document: &CustomDocument) {
    let Some(client) = &state.custom_projection else {
        return;
    };
    if document.status != "pending" {
        return;
    }
    let bucket = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    match client
        .spawn(
            CUSTOM_PROJECT_TASK,
            CustomProjectParams { bucket },
            SpawnOptions::default(),
        )
        .await
    {
        Ok(result) => telemetry::task_enqueued(CUSTOM_PROJECT_TASK, result.created),
        Err(error) => warn!(
            event = "company_context_custom_project_enqueue_failed",
            error = %error
        ),
    }
}

/// Mirrors the table's check on `source`.
fn valid_source(source: &str) -> Result<&str, ApiError> {
    let mut chars = source.chars();
    let valid = chars
        .next()
        .is_some_and(|first| first.is_ascii_lowercase() || first.is_ascii_digit())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && source.len() <= 64;
    if valid {
        Ok(source)
    } else {
        Err(ApiError::BadRequest(
            "source must be 1-64 lowercase letters, digits, or hyphens, starting with a letter or digit"
                .to_owned(),
        ))
    }
}

/// A document ID in canonical form; anything else names no document.
fn document_id(external_id: &str) -> Result<String, ApiError> {
    Uuid::try_parse(external_id)
        .map(|id| id.to_string())
        .map_err(|_| ApiError::NotFound)
}

fn writer(identity: &PrincipalIdentity) -> Result<String, ApiError> {
    principal_oid(identity).ok_or(ApiError::Internal)
}

/// The requested readers, which may name only the writer and its roles, or
/// the writer alone by default.
fn readers(
    identity: &PrincipalIdentity,
    writer: &str,
    requested: Option<&[String]>,
) -> Result<Vec<String>, ApiError> {
    let Some(requested) = requested else {
        return Ok(vec![format!("principal:{writer}")]);
    };
    let allowed = custom_readers(identity);
    if let Some(reader) = requested
        .iter()
        .find(|reader| *reader == "public" || !allowed.contains(reader))
    {
        return Err(ApiError::Forbidden(format!(
            "reader {reader:?} is neither you nor one of your roles"
        )));
    }
    Ok(requested.to_vec())
}

/// Reports rows the table's checks reject as bad requests.
fn database_error(event: &'static str) -> impl FnOnce(sqlx::Error) -> ApiError {
    move |error| {
        if let sqlx::Error::Database(database) = &error {
            let message = match (database.code().as_deref(), database.constraint()) {
                (Some("23514"), Some("custom_documents_title_check")) => {
                    Some("title must be at most 1000 characters")
                }
                (Some("23514"), Some("custom_documents_body_check")) => {
                    Some("body must not be blank and must be at most 1000000 bytes")
                }
                (Some("23514"), Some("custom_documents_metadata_check")) => {
                    Some("metadata must be at most 65536 bytes")
                }
                (Some("23514"), Some("custom_documents_readers_check")) => {
                    Some("readers must be principal:<id> or role:<id>")
                }
                // Postgres text and jsonb cannot hold NUL characters.
                (Some("22021" | "22P05"), _) => Some("text must not contain NUL characters"),
                _ => None,
            };
            if let Some(message) = message {
                return ApiError::BadRequest(message.to_owned());
            }
        }
        error!(event, error = %error);
        ApiError::Internal
    }
}

#[cfg(test)]
mod tests {
    use std::env;

    use serde_json::json;
    use sqlx::Executor;

    use super::*;
    use crate::{
        custom_documents::project,
        query::{self, QueryRequest},
        test_support::{TestDatabase, fake_embeddings},
    };

    /// prn_5CO4fITZ, in role role_1Uc5uCJR with foreign ID "eng".
    fn ada() -> PrincipalIdentity {
        PrincipalIdentity {
            principal_id: 1,
            principal_foreign_id: Some("ada".to_owned()),
            role_ids: vec![1],
            role_foreign_ids: vec!["eng".to_owned()],
            ..Default::default()
        }
    }

    /// prn_xUC2fVYG, also in role "eng".
    fn bob() -> PrincipalIdentity {
        PrincipalIdentity {
            principal_id: 3,
            role_ids: vec![1],
            role_foreign_ids: vec!["eng".to_owned()],
            ..Default::default()
        }
    }

    /// prn_yRoWctYw, without roles.
    fn carol() -> PrincipalIdentity {
        PrincipalIdentity {
            principal_id: 123,
            ..Default::default()
        }
    }

    fn request(body: Value) -> DocumentRequest {
        serde_json::from_value(body).unwrap()
    }

    async fn found(pool: &PgPool, identity: &PrincipalIdentity, query: &str) -> Vec<String> {
        let request: QueryRequest = serde_json::from_value(json!({ "query": query })).unwrap();
        query::search(pool, None, identity, &request)
            .await
            .unwrap()
            .into_iter()
            .map(|result| result.document.text)
            .collect()
    }

    /// The document `identity` lists with `id`, if any.
    async fn listed(
        pool: &PgPool,
        identity: &PrincipalIdentity,
        id: &str,
    ) -> Option<CustomDocument> {
        let all = ListParams {
            limit: Some(MAX_LIST_LIMIT),
            before: None,
        };
        list(pool, identity, "memory", &all)
            .await
            .unwrap()
            .into_iter()
            .find(|document| document.external_id == id)
    }

    #[tokio::test]
    async fn principals_reach_only_the_documents_they_created() {
        let Ok(database_url) = env::var("COMPANY_CONTEXT_TEST_DATABASE_URL") else {
            eprintln!("skipping: set COMPANY_CONTEXT_TEST_DATABASE_URL to a ParadeDB Postgres URL");
            return;
        };
        let database = TestDatabase::create(&database_url, "custom_api").await;
        let pool = &database.pool;
        let (embeddings, server) = fake_embeddings().await;
        let project = || project(pool, &embeddings, 6000);

        // A created document is readable only by its writer by default, and
        // searchable once projected.
        let memory = create(
            pool,
            &ada(),
            "memory",
            &request(json!({
                "title": "Editor",
                "body": "Ada prefers Neovim",
                "metadata": { "written_by": "prn_xUC2fVYG" },
            })),
        )
        .await
        .unwrap();
        assert_eq!(
            Uuid::try_parse(&memory.external_id)
                .unwrap()
                .get_version_num(),
            7
        );
        assert_eq!(memory.readers, ["principal:prn_5CO4fITZ"]);
        assert_eq!(memory.written_by, "prn_5CO4fITZ");
        assert_eq!(memory.status, "pending");
        let id = memory.external_id.as_str();
        project().await.unwrap();
        assert_eq!(listed(pool, &ada(), id).await.unwrap().status, "projected");
        assert_eq!(found(pool, &ada(), "neovim").await, ["Ada prefers Neovim"]);
        // Results name the writer, whatever the writer's metadata says.
        let document = query::document(pool, &ada(), &format!("custom:memory:{id}:000000"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(document.metadata["written_by"], "prn_5CO4fITZ");
        assert!(found(pool, &bob(), "neovim").await.is_empty());

        // Other principals cannot reach it, even when they can read it.
        let replacement = request(json!({ "body": "Ada prefers Helix", "readers": ["role:eng"] }));
        assert_eq!(
            replace(pool, &bob(), "memory", id, &replacement)
                .await
                .unwrap_err(),
            ApiError::NotFound
        );
        let replaced = replace(pool, &ada(), "memory", id, &replacement)
            .await
            .unwrap();
        assert_eq!(
            (replaced.title.as_str(), replaced.status.as_str()),
            ("", "pending")
        );
        project().await.unwrap();
        assert_eq!(found(pool, &bob(), "helix").await, ["Ada prefers Helix"]);
        assert!(found(pool, &carol(), "helix").await.is_empty());
        for result in [
            delete(pool, &bob(), "memory", id).await,
            delete(pool, &ada(), "notes", id).await,
            delete(pool, &ada(), "memory", "not-a-uuid").await,
        ] {
            assert_eq!(result.unwrap_err(), ApiError::NotFound);
        }
        let ids = |documents: Vec<CustomDocument>| {
            documents
                .into_iter()
                .map(|document| document.external_id)
                .collect::<Vec<_>>()
        };
        let all = ListParams::default();
        assert_eq!(ids(list(pool, &ada(), "memory", &all).await.unwrap()), [id]);
        assert!(list(pool, &bob(), "memory", &all).await.unwrap().is_empty());

        // Content the embeddings API rejects reports the error.
        let rejected = create(
            pool,
            &ada(),
            "memory",
            &request(json!({ "body": "REJECT" })),
        )
        .await
        .unwrap();
        project().await.unwrap();
        let rejected = listed(pool, &ada(), &rejected.external_id).await.unwrap();
        assert_eq!(rejected.status, "rejected");
        assert!(rejected.error.is_some());

        // Lists are newest first and page backward from `before`.
        let newest = create(pool, &ada(), "memory", &request(json!({ "body": "third" })))
            .await
            .unwrap();
        let page = |before: Option<&str>| ListParams {
            limit: Some(2),
            before: before.map(str::to_owned),
        };
        assert_eq!(
            ids(list(pool, &ada(), "memory", &page(None)).await.unwrap()),
            [newest.external_id.as_str(), rejected.external_id.as_str()]
        );
        assert_eq!(
            ids(
                list(pool, &ada(), "memory", &page(Some(&rejected.external_id)))
                    .await
                    .unwrap()
            ),
            [id]
        );

        // Deleting removes the document from search at once.
        delete(pool, &ada(), "memory", id).await.unwrap();
        assert!(found(pool, &bob(), "helix").await.is_empty());
        assert!(listed(pool, &ada(), id).await.is_none());

        // Rows written directly to the table have no writer to reach them.
        let imported = Uuid::now_v7().to_string();
        pool.execute(
            format!(
                "INSERT INTO company_context_system.custom_documents (source, external_id, body, readers) VALUES ('memory', '{imported}', 'imported', '{{principal:prn_5CO4fITZ}}')"
            )
            .as_str(),
        )
        .await
        .unwrap();
        assert_eq!(
            delete(pool, &ada(), "memory", &imported).await.unwrap_err(),
            ApiError::NotFound
        );

        server.abort();
        database.drop().await;
    }

    #[tokio::test]
    async fn writes_name_only_the_writer_and_its_roles_as_readers() {
        let Ok(database_url) = env::var("COMPANY_CONTEXT_TEST_DATABASE_URL") else {
            eprintln!("skipping: set COMPANY_CONTEXT_TEST_DATABASE_URL to a ParadeDB Postgres URL");
            return;
        };
        let database = TestDatabase::create(&database_url, "custom_api_validation").await;
        let pool = &database.pool;
        let write = |body: Value| async move {
            create(pool, &ada(), "memory", &request(body))
                .await
                .map(|document| document.readers)
        };

        for readers in [
            json!(["principal:prn_5CO4fITZ", "principal:ada"]),
            json!(["role:role_1Uc5uCJR", "role:eng"]),
            json!([]),
        ] {
            assert_eq!(
                json!(
                    write(json!({ "body": "note", "readers": readers }))
                        .await
                        .unwrap()
                ),
                readers
            );
        }
        for reader in ["public", "principal:prn_xUC2fVYG", "role:sales", "user:ada"] {
            assert!(
                matches!(
                    write(json!({ "body": "note", "readers": [reader] })).await,
                    Err(ApiError::Forbidden(_))
                ),
                "{reader}"
            );
        }

        for (source, body) in [
            ("Memory", json!({ "body": "note" })),
            ("memory", json!({ "body": "  " })),
            ("memory", json!({ "body": "\n\t" })),
            ("memory", json!({ "body": "a\u{0}b" })),
            (
                "memory",
                json!({ "body": "note", "title": "t".repeat(1001) }),
            ),
        ] {
            assert!(
                matches!(
                    create(pool, &ada(), source, &request(body.clone())).await,
                    Err(ApiError::BadRequest(_))
                ),
                "{source} {body}"
            );
        }
        let limit = ListParams {
            limit: Some(0),
            before: None,
        };
        assert!(matches!(
            list(pool, &ada(), "memory", &limit).await,
            Err(ApiError::BadRequest(_))
        ));

        database.drop().await;
    }
}
