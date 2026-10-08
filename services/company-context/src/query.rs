//! `POST /query`: hybrid retrieval over the published corpora on behalf of the
//! Console principal named by the request's API JWT.
//!
//! Access mirrors the reader role's row-level security: a principal sees a
//! document only while an active broker observation for its Google subject or
//! Slack user ID still reaches the document's file or conversation.

use std::{
    collections::HashMap,
    sync::{Arc, LazyLock},
    time::Duration,
};

use anyhow::{Context, Result};
use axum::{
    Json, Router,
    extract::{State, rejection::JsonRejection},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::post,
};
use chrono::{DateTime, Utc};
use jsonwebtoken::{Algorithm, DecodingKey, Validation};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqids::Sqids;
use sqlx::{PgPool, Postgres, Transaction, types::Json as SqlJson};
use tracing::{error, warn};

use crate::{
    config::Config,
    credentials::{ConsoleCredentials, PrincipalIdentity},
    embeddings::EmbeddingsClient,
};

const DEFAULT_LIMIT: usize = 10;
const MAX_LIMIT: usize = 50;
const MAX_QUERY_CHARS: usize = 2_000;
/// Candidates each lane contributes to rank fusion.
const MIN_CANDIDATES: usize = 20;
const RRF_K: f64 = 60.0;
const JWT_LEEWAY_SECONDS: u64 = 30;
const EMBEDDING_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone)]
pub struct QueryState {
    pub pool: PgPool,
    pub credentials: Arc<ConsoleCredentials>,
    pub embeddings: EmbeddingsClient,
    pub jwt: Arc<JwtVerifier>,
}

pub fn router(state: QueryState) -> Router {
    Router::new()
        .route("/query", post(handle))
        .with_state(state)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QueryRequest {
    pub query: String,
    #[serde(default)]
    pub filters: Filters,
    pub limit: Option<usize>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Filters {
    /// Data types to search; empty searches every type.
    #[serde(default)]
    pub types: Vec<DataType>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DataType {
    SlackMessage,
    SlackFile,
    DriveDoc,
}

impl DataType {
    const ALL: [Self; 3] = [Self::SlackMessage, Self::SlackFile, Self::DriveDoc];

    fn subject(self, identity: &PrincipalIdentity) -> Option<&str> {
        match self {
            Self::SlackMessage | Self::SlackFile => identity.slack_user_id.as_deref(),
            Self::DriveDoc => identity.google_subject.as_deref(),
        }
    }

    /// The document and embedding tables, the selected columns, and the
    /// visibility predicate for subject `$2`.
    fn source(self) -> (&'static str, &'static str, &'static str, &'static str) {
        match self {
            Self::SlackMessage => (
                "company_context_data.slack_documents",
                "company_context_data.slack_document_embeddings",
                r#"d.document_id, d.title, d.body, NULL::text AS url,
                   d.first_message_at AS occurred_at,
                   jsonb_build_object(
                       'conversation_id', d.conversation_id,
                       'channel_name', d.channel_name,
                       'conversation_kind', d.conversation_kind,
                       'day', d.day,
                       'first_message_at', d.first_message_at,
                       'last_message_at', d.last_message_at
                   ) AS metadata"#,
                r#"d.conversation_id IN (
                       SELECT o.conversation_id
                       FROM company_context_data.slack_broker_observations o
                       WHERE o.active AND o.provider_subject = $2
                   )"#,
            ),
            Self::SlackFile => (
                "company_context_data.slack_file_documents",
                "company_context_data.slack_file_document_embeddings",
                r#"d.document_id, d.title, d.body, NULLIF(d.url, '') AS url,
                   d.source_created_at AS occurred_at,
                   jsonb_build_object(
                       'file_id', d.file_id,
                       'mimetype', d.mimetype,
                       'filetype', d.filetype,
                       'author_id', d.author_id
                   ) AS metadata"#,
                r#"d.file_id IN (
                       SELECT s.file_id
                       FROM company_context_system.slack_file_shares s
                       JOIN company_context_data.slack_broker_observations o
                         ON o.conversation_id = s.conversation_id
                       WHERE o.active AND o.provider_subject = $2
                   )"#,
            ),
            Self::DriveDoc => (
                "company_context_data.google_drive_documents",
                "company_context_data.google_drive_document_embeddings",
                r#"d.document_id, d.title, d.body, NULLIF(d.url, '') AS url,
                   COALESCE(d.source_modified_at, d.source_created_at) AS occurred_at,
                   jsonb_build_object(
                       'file_id', d.file_id,
                       'document_type', d.document_type,
                       'mime_type', d.mime_type,
                       'drive_id', d.drive_id,
                       'page_start', d.page_start,
                       'page_end', d.page_end
                   ) AS metadata"#,
                r#"d.file_id IN (
                       SELECT o.file_id
                       FROM company_context_data.google_drive_broker_observations o
                       WHERE o.active AND o.provider_subject = $2
                   )"#,
            ),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct QueryResponse {
    pub results: Vec<QueryResult>,
}

#[derive(Debug, Serialize)]
pub struct QueryResult {
    pub document_id: String,
    #[serde(rename = "type")]
    pub data_type: DataType,
    pub title: String,
    pub url: Option<String>,
    pub text: String,
    pub occurred_at: Option<DateTime<Utc>>,
    pub score: f64,
    pub metadata: Value,
}

#[derive(sqlx::FromRow)]
struct Row {
    document_id: String,
    title: String,
    body: String,
    url: Option<String>,
    occurred_at: Option<DateTime<Utc>>,
    metadata: SqlJson<Value>,
}

#[derive(Debug)]
enum ApiError {
    BadRequest(String),
    Unauthorized,
    Forbidden,
    Internal,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            Self::BadRequest(message) => (StatusCode::BAD_REQUEST, message),
            Self::Unauthorized => (StatusCode::UNAUTHORIZED, "invalid bearer token".to_owned()),
            Self::Forbidden => (StatusCode::FORBIDDEN, "unknown principal".to_owned()),
            Self::Internal => (StatusCode::INTERNAL_SERVER_ERROR, "query failed".to_owned()),
        };
        (status, Json(json!({ "error": message }))).into_response()
    }
}

async fn handle(
    State(state): State<QueryState>,
    headers: HeaderMap,
    request: Result<Json<QueryRequest>, JsonRejection>,
) -> Result<Json<QueryResponse>, ApiError> {
    // Authenticate before looking at the body.
    let principal_id = bearer_token(&headers)
        .and_then(|token| state.jwt.principal_id(token))
        .ok_or(ApiError::Unauthorized)?;
    let Json(request) = request.map_err(|rejection| ApiError::BadRequest(rejection.body_text()))?;
    let identity = state
        .credentials
        .principal_identity(principal_id)
        .await
        .map_err(|error| {
            error!(event = "company_context_query_principal_failed", error = %format!("{error:#}"));
            ApiError::Internal
        })?
        .ok_or(ApiError::Forbidden)?;
    let results = search(&state.pool, Some(&state.embeddings), &identity, &request)
        .await
        .map_err(|error| match error.downcast::<InvalidQuery>() {
            Ok(InvalidQuery(message)) => ApiError::BadRequest(message),
            Err(error) => {
                error!(event = "company_context_query_failed", error = %format!("{error:#}"));
                ApiError::Internal
            }
        })?;
    Ok(Json(QueryResponse { results }))
}

#[derive(Debug)]
struct InvalidQuery(String);

impl std::fmt::Display for InvalidQuery {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for InvalidQuery {}

/// Searches the requested data types visible to `identity`, fusing keyword and
/// vector ranks. Without embeddings, or when embedding the query fails, only
/// keyword ranks are used.
pub async fn search(
    pool: &PgPool,
    embeddings: Option<&EmbeddingsClient>,
    identity: &PrincipalIdentity,
    request: &QueryRequest,
) -> Result<Vec<QueryResult>> {
    let query = request.query.trim();
    if query.is_empty() {
        return Err(InvalidQuery("query must not be empty".to_owned()).into());
    }
    if query.chars().count() > MAX_QUERY_CHARS {
        return Err(InvalidQuery(format!(
            "query must be at most {MAX_QUERY_CHARS} characters"
        ))
        .into());
    }
    let limit = request.limit.unwrap_or(DEFAULT_LIMIT);
    if !(1..=MAX_LIMIT).contains(&limit) {
        return Err(InvalidQuery(format!("limit must be between 1 and {MAX_LIMIT}")).into());
    }
    let types = if request.filters.types.is_empty() {
        &DataType::ALL[..]
    } else {
        &request.filters.types[..]
    };
    let searches: Vec<(DataType, &str)> = DataType::ALL
        .into_iter()
        .filter(|data_type| types.contains(data_type))
        .filter_map(|data_type| Some((data_type, data_type.subject(identity)?)))
        .collect();
    if searches.is_empty() {
        return Ok(Vec::new());
    }

    let vector = match embeddings {
        Some(embeddings) => query_vector(embeddings, query).await,
        None => None,
    };
    let candidates = limit.max(MIN_CANDIDATES) as i64;
    let mut tx = pool.begin().await?;
    sqlx::query("SET TRANSACTION READ ONLY")
        .execute(&mut *tx)
        .await?;
    sqlx::query("SET LOCAL statement_timeout = '10s'")
        .execute(&mut *tx)
        .await?;
    // Keep scanning the HNSW index until enough visible rows are found,
    // instead of filtering a fixed candidate set down to few or none.
    sqlx::query("SET LOCAL hnsw.iterative_scan = strict_order")
        .execute(&mut *tx)
        .await?;

    let mut lanes = Vec::new();
    for (data_type, subject) in searches {
        lanes.push((
            data_type,
            keyword_lane(&mut tx, data_type, query, subject, candidates).await?,
        ));
        if let Some((model, vector)) = &vector {
            lanes.push((
                data_type,
                vector_lane(&mut tx, data_type, vector, model, subject, candidates).await?,
            ));
        }
    }
    tx.commit().await?;
    Ok(fuse(lanes, limit))
}

async fn query_vector(embeddings: &EmbeddingsClient, query: &str) -> Option<(String, String)> {
    let input = [query.to_owned()];
    let result = tokio::time::timeout(EMBEDDING_TIMEOUT, embeddings.embed(&input))
        .await
        .context("embedding the query timed out")
        .and_then(|result| result);
    match result.and_then(|mut vectors| vectors.pop().context("no query embedding")) {
        Ok(vector) => Some((
            embeddings.model().to_owned(),
            serde_json::to_string(&vector).ok()?,
        )),
        Err(error) => {
            warn!(event = "company_context_query_embedding_failed", error = %format!("{error:#}"));
            None
        }
    }
}

async fn keyword_lane(
    tx: &mut Transaction<'_, Postgres>,
    data_type: DataType,
    query: &str,
    subject: &str,
    limit: i64,
) -> Result<Vec<Row>> {
    let (documents, _, columns, visible) = data_type.source();
    sqlx::query_as(&format!(
        r#"
        SELECT {columns}
        FROM {documents} d
        WHERE (d.title ||| $1::text::pdb.boost(2) OR d.body ||| $1::text)
          AND {visible}
        ORDER BY paradedb.score(d.document_id) DESC, d.document_id
        LIMIT $3
        "#
    ))
    .bind(query)
    .bind(subject)
    .bind(limit)
    .fetch_all(&mut **tx)
    .await
    .with_context(|| format!("keyword search {data_type:?}"))
}

async fn vector_lane(
    tx: &mut Transaction<'_, Postgres>,
    data_type: DataType,
    vector: &str,
    model: &str,
    subject: &str,
    limit: i64,
) -> Result<Vec<Row>> {
    let (documents, embeddings, columns, visible) = data_type.source();
    sqlx::query_as(&format!(
        r#"
        SELECT {columns}
        FROM {embeddings} e
        JOIN {documents} d ON d.document_id = e.document_id
        WHERE e.model = $4
          AND {visible}
        ORDER BY e.embedding <=> $1::text::vector, d.document_id
        LIMIT $3
        "#
    ))
    .bind(vector)
    .bind(subject)
    .bind(limit)
    .bind(model)
    .fetch_all(&mut **tx)
    .await
    .with_context(|| format!("vector search {data_type:?}"))
}

/// Reciprocal rank fusion across every lane of every data type.
fn fuse(lanes: Vec<(DataType, Vec<Row>)>, limit: usize) -> Vec<QueryResult> {
    let mut fused: HashMap<String, QueryResult> = HashMap::new();
    for (data_type, rows) in lanes {
        for (rank, row) in rows.into_iter().enumerate() {
            let score = 1.0 / (RRF_K + rank as f64 + 1.0);
            fused
                .entry(row.document_id.clone())
                .and_modify(|result| result.score += score)
                .or_insert_with(|| QueryResult {
                    document_id: row.document_id,
                    data_type,
                    title: row.title,
                    url: row.url,
                    text: row.body,
                    occurred_at: row.occurred_at,
                    score,
                    metadata: row.metadata.0,
                });
        }
    }
    let mut results: Vec<_> = fused.into_values().collect();
    results.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then_with(|| a.document_id.cmp(&b.document_id))
    });
    results.truncate(limit);
    results
}

fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    let (scheme, token) = headers
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .split_once(' ')?;
    let token = token.trim();
    (scheme.eq_ignore_ascii_case("Bearer") && !token.is_empty()).then_some(token)
}

/// Verifies the principal API JWTs the Console mints for api-rs.
pub struct JwtVerifier {
    key: DecodingKey,
    validation: Validation,
}

#[derive(Deserialize)]
struct Claims {
    sub: String,
    iat: i64,
}

impl JwtVerifier {
    pub fn new(config: &Config) -> Self {
        let mut validation = Validation::new(Algorithm::HS256);
        validation.leeway = JWT_LEEWAY_SECONDS;
        validation.validate_nbf = true;
        validation.set_audience(&[&config.jwt_audience]);
        validation.set_issuer(&[&config.jwt_issuer]);
        validation.set_required_spec_claims(&["exp", "iss", "sub", "aud"]);
        Self {
            key: DecodingKey::from_secret(config.jwt_signing_secret.as_bytes()),
            validation,
        }
    }

    /// The Console principal ID a valid token was issued for.
    fn principal_id(&self, token: &str) -> Option<i64> {
        let claims = jsonwebtoken::decode::<Claims>(token, &self.key, &self.validation)
            .inspect_err(
                |error| warn!(event = "company_context_query_jwt_rejected", error = ?error.kind()),
            )
            .ok()?
            .claims;
        if claims.iat > Utc::now().timestamp() + JWT_LEEWAY_SECONDS as i64 {
            warn!(
                event = "company_context_query_jwt_rejected",
                error = "iat_in_future"
            );
            return None;
        }
        principal_id(&claims.sub)
    }
}

/// Decodes a Console principal opaque ID (`prn_...`), accepting only its
/// canonical encoding.
fn principal_id(oid: &str) -> Option<i64> {
    // Mirrors the Rails OpaqueId concern: Sqids with an alphabet shuffled per
    // prefix and a minimum length of 8.
    static PRINCIPAL_SQIDS: LazyLock<Sqids> = LazyLock::new(|| {
        let mut alphabet: Vec<char> =
            "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789"
                .chars()
                .collect();
        alphabet.sort_by_cached_key(|character| {
            format!(
                "{:x}",
                Sha256::digest(format!("prn:{character}").as_bytes())
            )
        });
        Sqids::builder()
            .alphabet(alphabet)
            .min_length(8)
            .build()
            .expect("valid sqids settings")
    });
    let encoded = oid.strip_prefix("prn_")?;
    let [id] = PRINCIPAL_SQIDS.decode(encoded)[..] else {
        return None;
    };
    let canonical = PRINCIPAL_SQIDS.encode(&[id]).ok()?;
    (canonical == encoded).then_some(id.try_into().ok()?)
}

#[cfg(test)]
mod tests {
    use std::env;

    use clap::Parser;
    use jsonwebtoken::{EncodingKey, Header};
    use sqlx::Executor;

    use super::*;
    use crate::test_support::TestDatabase;

    #[test]
    fn principal_ids_decode_rails_opaque_ids() {
        assert_eq!(principal_id("prn_5CO4fITZ"), Some(1));
        assert_eq!(principal_id("prn_xUC2fVYG"), Some(3));
        assert_eq!(principal_id("prn_yRoWctYw"), Some(123));
        for invalid in [
            "5CO4fITZ",
            "prx_5CO4fITZ",
            "prn_",
            "prn_5CO4fIT!",
            "centaur-console",
        ] {
            assert_eq!(principal_id(invalid), None, "{invalid}");
        }
    }

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

    fn verifier() -> JwtVerifier {
        JwtVerifier::new(&config(&[]))
    }

    async fn fake_embeddings(Json(body): Json<Value>) -> Json<Value> {
        let count = body["input"].as_array().unwrap().len();
        Json(json!({
            "data": (0..count)
                .map(|index| json!({ "index": index, "embedding": vec![0.5; 1536] }))
                .collect::<Vec<_>>()
        }))
    }

    fn token(secret: &str, claims: Value) -> String {
        jsonwebtoken::encode(
            &Header::new(Algorithm::HS256),
            &claims,
            &EncodingKey::from_secret(secret.as_bytes()),
        )
        .unwrap()
    }

    #[test]
    fn verifier_accepts_only_console_principal_api_tokens() {
        let now = Utc::now().timestamp();
        let claims = |overrides: Value| {
            let mut claims = json!({
                "iss": "centaur-console",
                "aud": "centaur-api",
                "sub": "prn_5CO4fITZ",
                "iat": now,
                "exp": now + 3600,
            });
            claims
                .as_object_mut()
                .unwrap()
                .extend(overrides.as_object().unwrap().clone());
            claims
        };
        let verifier = verifier();
        assert_eq!(
            verifier.principal_id(&token("secret", claims(json!({})))),
            Some(1)
        );
        for rejected in [
            token("other", claims(json!({}))),
            token(
                "secret",
                claims(json!({ "aud": "centaur-console-sandbox-entitlements" })),
            ),
            token("secret", claims(json!({ "iss": "someone-else" }))),
            token("secret", claims(json!({ "exp": now - 3600 }))),
            token("secret", claims(json!({ "iat": now + 3600 }))),
            token("secret", claims(json!({ "sub": "centaur-console" }))),
        ] {
            assert_eq!(verifier.principal_id(&rejected), None);
        }
    }

    fn request(query: &str, types: &[DataType]) -> QueryRequest {
        QueryRequest {
            query: query.to_owned(),
            filters: Filters {
                types: types.to_vec(),
            },
            limit: None,
        }
    }

    async fn ids(
        pool: &PgPool,
        embeddings: Option<&EmbeddingsClient>,
        identity: &PrincipalIdentity,
        request: QueryRequest,
    ) -> Vec<String> {
        let mut ids: Vec<_> = search(pool, embeddings, identity, &request)
            .await
            .unwrap()
            .into_iter()
            .map(|result| result.document_id)
            .collect();
        ids.sort();
        ids
    }

    #[tokio::test]
    async fn search_returns_only_requested_types_the_principal_observes() {
        let Ok(database_url) = env::var("COMPANY_CONTEXT_TEST_DATABASE_URL") else {
            eprintln!("skipping: set COMPANY_CONTEXT_TEST_DATABASE_URL to a ParadeDB Postgres URL");
            return;
        };
        let database = TestDatabase::create(&database_url, "query").await;
        let pool = &database.pool;
        pool.execute(
            r#"
            INSERT INTO company_context_data.google_drive_broker_observations
                (broker_credential_id, file_id, provider_subject, active)
            VALUES (1, 'F1', 'G-ADA', true), (2, 'F2', 'G-BOB', true), (3, 'F3', 'G-ADA', false);
            INSERT INTO company_context_data.google_drive_documents
                (document_id, file_id, chunk_id, document_type, mime_type, title, body, content_hash)
            SELECT 'drive:' || id, id, '0', 'google_doc', 'text/plain', 'Roadmap ' || id,
                   'launch plan for falcon', 'hash'
            FROM unnest(ARRAY['F1', 'F2', 'F3']) AS id;

            INSERT INTO company_context_system.slack_conversations (conversation_id, team_id, kind, name)
            VALUES ('C1', 'T1', 'public_channel', 'general'), ('C2', 'T1', 'private_channel', 'secret');
            INSERT INTO company_context_data.slack_broker_observations
                (broker_credential_id, conversation_id, provider_subject, active)
            VALUES (10, 'C1', 'U-ADA', true), (11, 'C2', 'U-BOB', true);
            INSERT INTO company_context_system.slack_channel_days
                (conversation_id, day, projection_version, content_hash, rendered_at)
            VALUES ('C1', '2024-01-02', 1, 'hash', now()), ('C2', '2024-01-02', 1, 'hash', now());
            INSERT INTO company_context_data.slack_documents
                (document_id, conversation_id, day, chunk_id, title, body, conversation_kind,
                 first_message_at, last_message_at, content_hash)
            SELECT 'slack:' || id, id, '2024-01-02', '0', '#' || id, 'falcon launch is friday',
                   'public_channel', now(), now(), 'hash'
            FROM unnest(ARRAY['C1', 'C2']) AS id;

            INSERT INTO company_context_system.slack_messages
                (conversation_id, message_ts, text, occurred_at, raw_payload)
            VALUES ('C1', '1.0', 'deck', now(), '{}'), ('C2', '2.0', 'deck', now(), '{}');
            INSERT INTO company_context_system.slack_files (file_id, source_version, extraction_status)
            VALUES ('SF1', 'v1', 'completed'), ('SF2', 'v1', 'completed');
            INSERT INTO company_context_system.slack_file_shares (file_id, conversation_id, message_ts)
            VALUES ('SF1', 'C1', '1.0'), ('SF2', 'C2', '2.0');
            INSERT INTO company_context_data.slack_file_documents
                (document_id, file_id, chunk_id, title, body, content_hash)
            SELECT 'file:' || id, id, '0', 'falcon.pdf', 'falcon launch deck', 'hash'
            FROM unnest(ARRAY['SF1', 'SF2']) AS id;
            "#,
        )
        .await
        .unwrap();

        let ada = PrincipalIdentity {
            google_subject: Some("G-ADA".to_owned()),
            slack_user_id: Some("U-ADA".to_owned()),
        };
        assert_eq!(
            ids(pool, None, &ada, request("Falcon launch?", &[])).await,
            ["drive:F1", "file:SF1", "slack:C1"]
        );
        assert_eq!(
            ids(
                pool,
                None,
                &ada,
                request("falcon", &[DataType::SlackMessage])
            )
            .await,
            ["slack:C1"]
        );
        assert_eq!(
            ids(
                pool,
                None,
                &ada,
                request("falcon", &[DataType::DriveDoc, DataType::SlackFile])
            )
            .await,
            ["drive:F1", "file:SF1"]
        );
        assert!(
            ids(pool, None, &ada, request("unrelated", &[]))
                .await
                .is_empty()
        );

        // A principal without a Google identity sees no Drive documents.
        let slack_only = PrincipalIdentity {
            google_subject: None,
            slack_user_id: Some("U-BOB".to_owned()),
        };
        assert_eq!(
            ids(pool, None, &slack_only, request("falcon", &[])).await,
            ["file:SF2", "slack:C2"]
        );

        // Semantic matches are limited to the same visible documents.
        pool.execute(
            r#"
            INSERT INTO company_context_data.google_drive_document_embeddings
                (document_id, model, dimensions, content_hash, embedding)
            SELECT document_id, 'text-embedding-3-small', 1536, 'hash', array_fill(0.5, ARRAY[1536])::vector
            FROM company_context_data.google_drive_documents;
            INSERT INTO company_context_data.slack_document_embeddings
                (document_id, model, dimensions, content_hash, embedding)
            SELECT document_id, 'text-embedding-3-small', 1536, 'hash', array_fill(0.5, ARRAY[1536])::vector
            FROM company_context_data.slack_documents;
            INSERT INTO company_context_data.slack_file_document_embeddings
                (document_id, model, dimensions, content_hash, embedding)
            SELECT document_id, 'text-embedding-3-small', 1536, 'hash', array_fill(0.5, ARRAY[1536])::vector
            FROM company_context_data.slack_file_documents;
            "#,
        )
        .await
        .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let router = Router::new().route("/embeddings", post(fake_embeddings));
        let server = tokio::spawn(async move { axum::serve(listener, router).await });
        let base_url = format!("http://{address}");
        let embeddings = EmbeddingsClient::new(&config(&["--openai-base-url", &base_url])).unwrap();
        assert_eq!(
            ids(pool, Some(&embeddings), &ada, request("unrelated", &[])).await,
            ["drive:F1", "file:SF1", "slack:C1"]
        );

        let invalid = search(pool, None, &ada, &request("  ", &[]))
            .await
            .unwrap_err();
        assert!(invalid.is::<InvalidQuery>());

        server.abort();
        database.drop().await;
    }
}
