//! `POST /query` and `GET /documents/{document_id}`: hybrid retrieval and
//! document reads over the published corpora on behalf of the Console
//! principal named by the request's API JWT.
//!
//! Access mirrors the reader role's row-level security: a principal sees a
//! document only while an active broker observation for its Google subject or
//! Slack user ID still reaches the document's file or conversation. Queries
//! run as [`QUERY_ROLE`], which can only read `company_context_data`.

use std::{
    collections::HashMap,
    sync::{Arc, LazyLock},
    time::Duration,
};

use anyhow::{Context, Result};
use axum::{
    Json, Router,
    extract::{Path, State, rejection::JsonRejection},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
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
    config::{
        Config, GOOGLE_DRIVE_DOCUMENT_ID_PREFIX, SLACK_DOCUMENT_ID_PREFIX,
        SLACK_FILE_DOCUMENT_ID_PREFIX,
    },
    credentials::{ConsoleCredentials, PrincipalIdentity},
    embeddings::EmbeddingsClient,
};

const DEFAULT_LIMIT: usize = 10;
const MAX_LIMIT: usize = 50;
const MAX_QUERY_CHARS: usize = 2_000;
const MAX_FILTER_IDS: usize = 100;
/// Candidates each lane contributes to rank fusion.
const MIN_CANDIDATES: usize = 20;
const RRF_K: f64 = 60.0;
const JWT_LEEWAY_SECONDS: u64 = 30;
const EMBEDDING_TIMEOUT: Duration = Duration::from_secs(10);
/// Database role with read-only access to `company_context_data` only.
const QUERY_ROLE: &str = "centaur_company_context_v2_query";

#[derive(Clone)]
pub struct QueryState {
    pub pool: PgPool,
    pub credentials: Arc<ConsoleCredentials>,
    pub embeddings: EmbeddingsClient,
    pub jwt: Arc<JwtVerifier>,
}

pub fn router(state: QueryState) -> Router {
    Router::new()
        .route("/query", post(handle_query))
        .route("/documents/{document_id}", get(handle_document))
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
    /// Inclusive lower bound on when a document's content occurred.
    pub occurred_after: Option<DateTime<Utc>>,
    /// Exclusive upper bound on when a document's content occurred.
    pub occurred_before: Option<DateTime<Utc>>,
    /// Slack conversation IDs. Limits the search to Slack messages in, and
    /// Slack files shared in, these conversations.
    #[serde(default)]
    pub channel_ids: Vec<String>,
    /// Slack or Drive file IDs. Limits the search to these files' documents.
    #[serde(default)]
    pub file_ids: Vec<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DataType {
    SlackMessage,
    SlackFile,
    DriveDoc,
}

/// How one data type's documents are selected. Queries bind the principal's
/// subject to `$2` and the filters to `$4` through `$7`.
struct Source {
    /// Prefix of the type's document IDs.
    id_prefix: &'static str,
    documents: &'static str,
    embeddings: &'static str,
    columns: &'static str,
    /// Documents visible to subject `$2`.
    visible: &'static str,
    /// Expressions for when the document's content starts and ends.
    starts_at: &'static str,
    ends_at: &'static str,
    /// Documents in the conversations `$6`, or `None` if the type has none.
    in_channels: Option<&'static str>,
    /// The type's file ID column, or `None` if it has none.
    file_id: Option<&'static str>,
}

impl Source {
    /// The filter predicate. A filter the type cannot apply excludes the type
    /// before it is queried, so it only needs to accept an unset filter here.
    fn filters(&self) -> String {
        let in_channels = self.in_channels.unwrap_or("FALSE");
        let in_files = self
            .file_id
            .map(|column| format!("{column} = ANY($7)"))
            .unwrap_or_else(|| "FALSE".to_owned());
        format!(
            r#"($4::timestamptz IS NULL OR {ends_at} >= $4)
               AND ($5::timestamptz IS NULL OR {starts_at} < $5)
               AND ($6::text[] IS NULL OR {in_channels})
               AND ($7::text[] IS NULL OR {in_files})"#,
            ends_at = self.ends_at,
            starts_at = self.starts_at,
        )
    }
}

impl DataType {
    const ALL: [Self; 3] = [Self::SlackMessage, Self::SlackFile, Self::DriveDoc];

    fn subject(self, identity: &PrincipalIdentity) -> Option<&str> {
        match self {
            Self::SlackMessage | Self::SlackFile => identity.slack_user_id.as_deref(),
            Self::DriveDoc => identity.google_subject.as_deref(),
        }
    }

    fn source(self) -> Source {
        match self {
            Self::SlackMessage => Source {
                id_prefix: SLACK_DOCUMENT_ID_PREFIX,
                documents: "company_context_data.slack_documents",
                embeddings: "company_context_data.slack_document_embeddings",
                columns: r#"d.document_id, d.title, d.body, NULL::text AS url,
                   d.first_message_at AS occurred_at,
                   jsonb_build_object(
                       'conversation_id', d.conversation_id,
                       'channel_name', d.channel_name,
                       'conversation_kind', d.conversation_kind,
                       'day', d.day,
                       'first_message_at', d.first_message_at,
                       'last_message_at', d.last_message_at
                   ) AS metadata"#,
                visible: r#"d.conversation_id IN (
                       SELECT o.conversation_id
                       FROM company_context_data.slack_broker_observations o
                       WHERE o.active AND o.provider_subject = $2
                   )"#,
                // A channel day chunk matches a window its messages overlap.
                starts_at: "d.first_message_at",
                ends_at: "d.last_message_at",
                in_channels: Some("d.conversation_id = ANY($6)"),
                file_id: None,
            },
            Self::SlackFile => Source {
                id_prefix: SLACK_FILE_DOCUMENT_ID_PREFIX,
                documents: "company_context_data.slack_file_documents",
                embeddings: "company_context_data.slack_file_document_embeddings",
                columns: r#"d.document_id, d.title, d.body, NULLIF(d.url, '') AS url,
                   d.source_created_at AS occurred_at,
                   jsonb_build_object(
                       'file_id', d.file_id,
                       'mimetype', d.mimetype,
                       'filetype', d.filetype,
                       'author_id', d.author_id
                   ) AS metadata"#,
                visible: r#"d.file_id IN (
                       SELECT s.file_id
                       FROM company_context_data.slack_file_shares s
                       JOIN company_context_data.slack_broker_observations o
                         ON o.conversation_id = s.conversation_id
                       WHERE o.active AND o.provider_subject = $2
                   )"#,
                starts_at: "d.source_created_at",
                ends_at: "d.source_created_at",
                // Only shares the subject can see count, so a filter does not
                // reveal where else a file was shared.
                in_channels: Some(
                    r#"d.file_id IN (
                       SELECT s.file_id
                       FROM company_context_data.slack_file_shares s
                       JOIN company_context_data.slack_broker_observations o
                         ON o.conversation_id = s.conversation_id
                       WHERE o.active AND o.provider_subject = $2
                         AND s.conversation_id = ANY($6)
                   )"#,
                ),
                file_id: Some("d.file_id"),
            },
            Self::DriveDoc => Source {
                id_prefix: GOOGLE_DRIVE_DOCUMENT_ID_PREFIX,
                documents: "company_context_data.google_drive_documents",
                embeddings: "company_context_data.google_drive_document_embeddings",
                columns: r#"d.document_id, d.title, d.body, NULLIF(d.url, '') AS url,
                   COALESCE(d.source_modified_at, d.source_created_at) AS occurred_at,
                   jsonb_build_object(
                       'file_id', d.file_id,
                       'document_type', d.document_type,
                       'mime_type', d.mime_type,
                       'drive_id', d.drive_id,
                       'page_start', d.page_start,
                       'page_end', d.page_end
                   ) AS metadata"#,
                visible: r#"d.file_id IN (
                       SELECT o.file_id
                       FROM company_context_data.google_drive_broker_observations o
                       WHERE o.active AND o.provider_subject = $2
                   )"#,
                starts_at: "COALESCE(d.source_modified_at, d.source_created_at)",
                ends_at: "COALESCE(d.source_modified_at, d.source_created_at)",
                in_channels: None,
                file_id: Some("d.file_id"),
            },
        }
    }

    /// The type of the document with `document_id`, by its prefix.
    fn of_document(document_id: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|data_type| document_id.starts_with(data_type.source().id_prefix))
    }

    /// Whether the type can satisfy every set filter.
    fn supports(self, filters: &Filters) -> bool {
        let source = self.source();
        (filters.channel_ids.is_empty() || source.in_channels.is_some())
            && (filters.file_ids.is_empty() || source.file_id.is_some())
    }
}

#[derive(Debug, Serialize)]
pub struct QueryResponse {
    pub results: Vec<QueryResult>,
}

#[derive(Debug, Serialize)]
pub struct QueryResult {
    #[serde(flatten)]
    pub document: Document,
    pub score: f64,
}

#[derive(Debug, Serialize)]
pub struct Document {
    pub document_id: String,
    #[serde(rename = "type")]
    pub data_type: DataType,
    pub title: String,
    pub url: Option<String>,
    pub text: String,
    pub occurred_at: Option<DateTime<Utc>>,
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

impl Row {
    fn into_document(self, data_type: DataType) -> Document {
        Document {
            document_id: self.document_id,
            data_type,
            title: self.title,
            url: self.url,
            text: self.body,
            occurred_at: self.occurred_at,
            metadata: self.metadata.0,
        }
    }
}

#[derive(Debug)]
enum ApiError {
    BadRequest(String),
    Unauthorized,
    Forbidden,
    NotFound,
    Internal,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            Self::BadRequest(message) => (StatusCode::BAD_REQUEST, message),
            Self::Unauthorized => (StatusCode::UNAUTHORIZED, "invalid bearer token".to_owned()),
            Self::Forbidden => (StatusCode::FORBIDDEN, "unknown principal".to_owned()),
            Self::NotFound => (StatusCode::NOT_FOUND, "document not found".to_owned()),
            Self::Internal => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "request failed".to_owned(),
            ),
        };
        (status, Json(json!({ "error": message }))).into_response()
    }
}

/// The identity of the principal named by the request's API JWT.
async fn authenticate(
    state: &QueryState,
    headers: &HeaderMap,
) -> Result<PrincipalIdentity, ApiError> {
    let principal_id = bearer_token(headers)
        .and_then(|token| state.jwt.principal_id(token))
        .ok_or(ApiError::Unauthorized)?;
    state
        .credentials
        .principal_identity(principal_id)
        .await
        .map_err(|error| {
            error!(event = "company_context_query_principal_failed", error = %format!("{error:#}"));
            ApiError::Internal
        })?
        .ok_or(ApiError::Forbidden)
}

fn internal(event: &'static str) -> impl FnOnce(anyhow::Error) -> ApiError {
    move |error| match error.downcast::<InvalidQuery>() {
        Ok(InvalidQuery(message)) => ApiError::BadRequest(message),
        Err(error) => {
            error!(event, error = %format!("{error:#}"));
            ApiError::Internal
        }
    }
}

async fn handle_query(
    State(state): State<QueryState>,
    headers: HeaderMap,
    request: Result<Json<QueryRequest>, JsonRejection>,
) -> Result<Json<QueryResponse>, ApiError> {
    // Authenticate before looking at the body.
    let identity = authenticate(&state, &headers).await?;
    let Json(request) = request.map_err(|rejection| ApiError::BadRequest(rejection.body_text()))?;
    let results = search(&state.pool, Some(&state.embeddings), &identity, &request)
        .await
        .map_err(internal("company_context_query_failed"))?;
    Ok(Json(QueryResponse { results }))
}

async fn handle_document(
    State(state): State<QueryState>,
    headers: HeaderMap,
    Path(document_id): Path<String>,
) -> Result<Json<Document>, ApiError> {
    let identity = authenticate(&state, &headers).await?;
    document(&state.pool, &identity, &document_id)
        .await
        .map_err(internal("company_context_document_failed"))?
        .map(Json)
        .ok_or(ApiError::NotFound)
}

#[derive(Debug)]
struct InvalidQuery(String);

impl std::fmt::Display for InvalidQuery {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for InvalidQuery {}

fn invalid(message: impl Into<String>) -> anyhow::Error {
    InvalidQuery(message.into()).into()
}

/// Bound filter parameters `$4` through `$7`.
struct FilterParams {
    occurred_after: Option<DateTime<Utc>>,
    occurred_before: Option<DateTime<Utc>>,
    channel_ids: Option<Vec<String>>,
    file_ids: Option<Vec<String>>,
}

impl FilterParams {
    fn new(filters: &Filters) -> Result<Self> {
        if let (Some(after), Some(before)) = (filters.occurred_after, filters.occurred_before)
            && after >= before
        {
            return Err(invalid("occurred_after must be before occurred_before"));
        }
        Ok(Self {
            occurred_after: filters.occurred_after,
            occurred_before: filters.occurred_before,
            channel_ids: ids(&filters.channel_ids, "channel_ids")?,
            file_ids: ids(&filters.file_ids, "file_ids")?,
        })
    }
}

fn ids(values: &[String], name: &str) -> Result<Option<Vec<String>>> {
    if values.len() > MAX_FILTER_IDS {
        return Err(invalid(format!(
            "{name} must have at most {MAX_FILTER_IDS} entries"
        )));
    }
    let ids: Vec<String> = values.iter().map(|id| id.trim().to_owned()).collect();
    if ids.iter().any(String::is_empty) {
        return Err(invalid(format!("{name} must not contain empty IDs")));
    }
    Ok((!ids.is_empty()).then_some(ids))
}

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
        return Err(invalid("query must not be empty"));
    }
    if query.chars().count() > MAX_QUERY_CHARS {
        return Err(invalid(format!(
            "query must be at most {MAX_QUERY_CHARS} characters"
        )));
    }
    let limit = request.limit.unwrap_or(DEFAULT_LIMIT);
    if !(1..=MAX_LIMIT).contains(&limit) {
        return Err(invalid(format!("limit must be between 1 and {MAX_LIMIT}")));
    }
    let filters = &request.filters;
    let params = FilterParams::new(filters)?;
    let types: Vec<DataType> = DataType::ALL
        .into_iter()
        .filter(|data_type| filters.types.is_empty() || filters.types.contains(data_type))
        .filter(|data_type| data_type.supports(filters))
        .collect();
    if types.is_empty() {
        return Err(invalid("no requested type supports every filter"));
    }
    let searches: Vec<(DataType, &str)> = types
        .into_iter()
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
    let mut tx = read_only(pool).await?;
    // ParadeDB cannot evaluate a parameterized `|||` in the generic plan
    // Postgres switches cached statements to after five executions.
    sqlx::query("SET LOCAL plan_cache_mode = force_custom_plan")
        .execute(&mut *tx)
        .await?;
    // Keep scanning the HNSW index until enough visible rows are found,
    // instead of filtering a fixed candidate set down to few or none.
    sqlx::query("SET LOCAL hnsw.iterative_scan = strict_order")
        .execute(&mut *tx)
        .await?;

    let mut lanes = Vec::new();
    for (data_type, subject) in searches {
        let source = data_type.source();
        let keyword = format!(
            r#"
            SELECT {columns}
            FROM {documents} d
            WHERE (d.title ||| $1::text::pdb.boost(2) OR d.body ||| $1::text)
              AND {visible}
              AND {filters}
            ORDER BY paradedb.score(d.document_id) DESC, d.document_id
            LIMIT $3
            "#,
            columns = source.columns,
            documents = source.documents,
            visible = source.visible,
            filters = source.filters(),
        );
        let rows = lane(&mut tx, &keyword, query, None, subject, candidates, &params)
            .await
            .with_context(|| format!("keyword search {data_type:?}"))?;
        lanes.push((data_type, rows));

        if let Some((model, vector)) = &vector {
            let semantic = format!(
                r#"
                SELECT {columns}
                FROM {embeddings} e
                JOIN {documents} d ON d.document_id = e.document_id
                WHERE e.model = $8
                  AND {visible}
                  AND {filters}
                ORDER BY e.embedding <=> $1::text::vector, d.document_id
                LIMIT $3
                "#,
                columns = source.columns,
                embeddings = source.embeddings,
                documents = source.documents,
                visible = source.visible,
                filters = source.filters(),
            );
            let rows = lane(
                &mut tx,
                &semantic,
                vector,
                Some(model),
                subject,
                candidates,
                &params,
            )
            .await
            .with_context(|| format!("vector search {data_type:?}"))?;
            lanes.push((data_type, rows));
        }
    }
    tx.commit().await?;
    Ok(fuse(lanes, limit))
}

/// Returns the document with `document_id` if it is visible to `identity`.
pub async fn document(
    pool: &PgPool,
    identity: &PrincipalIdentity,
    document_id: &str,
) -> Result<Option<Document>> {
    let Some(data_type) = DataType::of_document(document_id) else {
        return Ok(None);
    };
    let Some(subject) = data_type.subject(identity) else {
        return Ok(None);
    };
    let source = data_type.source();
    let mut tx = read_only(pool).await?;
    let row: Option<Row> = sqlx::query_as(&format!(
        "SELECT {columns} FROM {documents} d WHERE d.document_id = $1 AND {visible}",
        columns = source.columns,
        documents = source.documents,
        visible = source.visible,
    ))
    .bind(document_id)
    .bind(subject)
    .fetch_optional(&mut *tx)
    .await
    .with_context(|| format!("read {data_type:?} document"))?;
    tx.commit().await?;
    Ok(row.map(|row| row.into_document(data_type)))
}

/// Begins a read-only transaction running as [`QUERY_ROLE`].
async fn read_only(pool: &PgPool) -> Result<Transaction<'static, Postgres>> {
    let mut tx = pool.begin().await?;
    sqlx::query("SET TRANSACTION READ ONLY")
        .execute(&mut *tx)
        .await?;
    sqlx::query(&format!("SET LOCAL ROLE {QUERY_ROLE}"))
        .execute(&mut *tx)
        .await?;
    sqlx::query("SET LOCAL statement_timeout = '10s'")
        .execute(&mut *tx)
        .await?;
    Ok(tx)
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

/// Runs one ranked lane: `$1` is the query text or vector, `$2` the subject,
/// `$3` the limit, `$4`..`$7` the filters, and `$8` the embedding model.
async fn lane(
    tx: &mut Transaction<'_, Postgres>,
    sql: &str,
    input: &str,
    model: Option<&str>,
    subject: &str,
    limit: i64,
    params: &FilterParams,
) -> Result<Vec<Row>> {
    let mut query = sqlx::query_as(sql)
        .bind(input)
        .bind(subject)
        .bind(limit)
        .bind(params.occurred_after)
        .bind(params.occurred_before)
        .bind(params.channel_ids.as_deref())
        .bind(params.file_ids.as_deref());
    if let Some(model) = model {
        query = query.bind(model);
    }
    Ok(query.fetch_all(&mut **tx).await?)
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
                    document: row.into_document(data_type),
                    score,
                });
        }
    }
    let mut results: Vec<_> = fused.into_values().collect();
    results.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then_with(|| a.document.document_id.cmp(&b.document.document_id))
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

    fn request(body: Value) -> QueryRequest {
        serde_json::from_value(body).unwrap()
    }

    async fn ids(
        pool: &PgPool,
        embeddings: Option<&EmbeddingsClient>,
        identity: &PrincipalIdentity,
        body: Value,
    ) -> Vec<String> {
        let mut ids: Vec<_> = search(pool, embeddings, identity, &request(body))
            .await
            .unwrap()
            .into_iter()
            .map(|result| result.document.document_id)
            .collect();
        ids.sort();
        ids
    }

    fn ada() -> PrincipalIdentity {
        PrincipalIdentity {
            google_subject: Some("G-ADA".to_owned()),
            slack_user_id: Some("U-ADA".to_owned()),
        }
    }

    /// Bob has no Google identity.
    fn bob() -> PrincipalIdentity {
        PrincipalIdentity {
            google_subject: None,
            slack_user_id: Some("U-BOB".to_owned()),
        }
    }

    /// Ada observes Drive files F1 and F4 (not F3, whose observation is
    /// inactive) and Slack conversations C1 and C3; Bob observes C2. Slack
    /// file SF1 is shared in C1 and SF2 in both C2 and C3.
    async fn seed(pool: &PgPool) {
        pool.execute(
            r#"
            INSERT INTO company_context_data.google_drive_broker_observations
                (broker_credential_id, file_id, provider_subject, active)
            VALUES (1, 'F1', 'G-ADA', true), (2, 'F2', 'G-BOB', true),
                   (3, 'F3', 'G-ADA', false), (4, 'F4', 'G-ADA', true);
            INSERT INTO company_context_data.google_drive_documents
                (document_id, file_id, chunk_id, document_type, mime_type, title, body,
                 source_modified_at, content_hash)
            VALUES
                ('google-drive:F1', 'F1', '0', 'google_doc', 'text/plain', 'Roadmap', 'launch plan for falcon', '2024-01-10Z', 'hash'),
                ('google-drive:F2', 'F2', '0', 'google_doc', 'text/plain', 'Roadmap', 'launch plan for falcon', '2024-01-10Z', 'hash'),
                ('google-drive:F3', 'F3', '0', 'google_doc', 'text/plain', 'Roadmap', 'launch plan for falcon', '2024-01-10Z', 'hash'),
                ('google-drive:F4', 'F4', '0', 'pdf', 'application/pdf', 'Retro', 'falcon launch retro', '2024-03-01Z', 'hash');

            INSERT INTO company_context_system.slack_conversations (conversation_id, team_id, kind, name)
            VALUES ('C1', 'T1', 'public_channel', 'general'),
                   ('C2', 'T1', 'private_channel', 'secret'),
                   ('C3', 'T1', 'public_channel', 'launch');
            INSERT INTO company_context_data.slack_broker_observations
                (broker_credential_id, conversation_id, provider_subject, active)
            VALUES (10, 'C1', 'U-ADA', true), (10, 'C3', 'U-ADA', true), (11, 'C2', 'U-BOB', true);
            INSERT INTO company_context_system.slack_channel_days
                (conversation_id, day, projection_version, content_hash, rendered_at)
            VALUES ('C1', '2024-01-02', 1, 'hash', now()), ('C2', '2024-01-02', 1, 'hash', now()),
                   ('C3', '2024-02-01', 1, 'hash', now());
            INSERT INTO company_context_data.slack_documents
                (document_id, conversation_id, day, chunk_id, title, body, channel_name,
                 conversation_kind, first_message_at, last_message_at, content_hash)
            VALUES
                ('slack:C1:2024-01-02:000000', 'C1', '2024-01-02', '0', '#general', 'falcon launch is friday', 'general', 'public_channel',
                 '2024-01-02T09:00Z', '2024-01-02T17:00Z', 'hash'),
                ('slack:C2:2024-01-02:000000', 'C2', '2024-01-02', '0', '#secret', 'falcon launch is friday', 'secret', 'private_channel',
                 '2024-01-02T09:00Z', '2024-01-02T17:00Z', 'hash'),
                ('slack:C3:2024-02-01:000000', 'C3', '2024-02-01', '0', '#launch', 'falcon launch went well', 'launch', 'public_channel',
                 '2024-02-01T09:00Z', '2024-02-01T10:00Z', 'hash');

            INSERT INTO company_context_system.slack_messages
                (conversation_id, message_ts, text, occurred_at, raw_payload)
            VALUES ('C1', '1.0', 'deck', now(), '{}'), ('C2', '2.0', 'deck', now(), '{}'),
                   ('C3', '3.0', 'deck', now(), '{}');
            INSERT INTO company_context_system.slack_files (file_id, source_version, extraction_status)
            VALUES ('SF1', 'v1', 'completed'), ('SF2', 'v1', 'completed');
            INSERT INTO company_context_data.slack_file_shares (file_id, conversation_id, message_ts)
            VALUES ('SF1', 'C1', '1.0'), ('SF2', 'C2', '2.0'), ('SF2', 'C3', '3.0');
            INSERT INTO company_context_data.slack_file_documents
                (document_id, file_id, chunk_id, title, body, source_created_at, content_hash)
            VALUES ('slack-file:SF1', 'SF1', '0', 'falcon.pdf', 'falcon launch deck', '2024-01-02Z', 'hash'),
                   ('slack-file:SF2', 'SF2', '0', 'falcon-retro.pdf', 'falcon launch retro deck', '2024-02-01Z', 'hash');
            "#,
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn search_returns_only_matching_documents_the_principal_observes() {
        let Ok(database_url) = env::var("COMPANY_CONTEXT_TEST_DATABASE_URL") else {
            eprintln!("skipping: set COMPANY_CONTEXT_TEST_DATABASE_URL to a ParadeDB Postgres URL");
            return;
        };
        let database = TestDatabase::create(&database_url, "query").await;
        let pool = &database.pool;
        seed(pool).await;
        let (ada, bob) = (ada(), bob());

        assert_eq!(
            ids(pool, None, &ada, json!({ "query": "Falcon launch?" })).await,
            [
                "google-drive:F1",
                "google-drive:F4",
                "slack-file:SF1",
                "slack-file:SF2",
                "slack:C1:2024-01-02:000000",
                "slack:C3:2024-02-01:000000"
            ]
        );
        assert_eq!(
            ids(
                pool,
                None,
                &ada,
                json!({ "query": "falcon", "filters": { "types": ["slack_message"] } })
            )
            .await,
            ["slack:C1:2024-01-02:000000", "slack:C3:2024-02-01:000000"]
        );
        assert!(
            ids(pool, None, &ada, json!({ "query": "unrelated" }))
                .await
                .is_empty()
        );
        assert_eq!(
            ids(pool, None, &bob, json!({ "query": "falcon" })).await,
            ["slack-file:SF2", "slack:C2:2024-01-02:000000"]
        );

        // Channel days match windows their messages overlap.
        assert_eq!(
            ids(
                pool,
                None,
                &ada,
                json!({ "query": "falcon", "filters": {
                    "occurred_after": "2024-01-02T12:00:00Z",
                    "occurred_before": "2024-01-03T00:00:00Z",
                } })
            )
            .await,
            ["slack:C1:2024-01-02:000000"]
        );
        assert_eq!(
            ids(
                pool,
                None,
                &ada,
                json!({ "query": "falcon", "filters": { "occurred_after": "2024-02-01T00:00:00Z" } })
            )
            .await,
            ["google-drive:F4", "slack-file:SF2", "slack:C3:2024-02-01:000000"]
        );

        // Channel filters search Slack only, through visible shares only.
        let channels = |channel_ids: Value| json!({ "query": "falcon", "filters": { "channel_ids": channel_ids } });
        assert_eq!(
            ids(pool, None, &ada, channels(json!(["C1"]))).await,
            ["slack-file:SF1", "slack:C1:2024-01-02:000000"]
        );
        assert!(
            ids(pool, None, &ada, channels(json!(["C2"])))
                .await
                .is_empty()
        );
        assert_eq!(
            ids(pool, None, &ada, channels(json!(["C3"]))).await,
            ["slack-file:SF2", "slack:C3:2024-02-01:000000"]
        );
        assert_eq!(
            ids(
                pool,
                None,
                &ada,
                json!({ "query": "falcon", "filters": { "file_ids": ["F4", "SF1", "F2"] } })
            )
            .await,
            ["google-drive:F4", "slack-file:SF1"]
        );

        for invalid in [
            json!({ "query": "  " }),
            json!({ "query": "falcon", "limit": 0 }),
            json!({ "query": "falcon", "filters": { "types": ["drive_doc"], "channel_ids": ["C1"] } }),
            json!({ "query": "falcon", "filters": { "file_ids": [" "] } }),
            json!({ "query": "falcon", "filters": {
                "occurred_after": "2024-02-01T00:00:00Z",
                "occurred_before": "2024-01-01T00:00:00Z",
            } }),
        ] {
            let error = search(pool, None, &ada, &request(invalid.clone()))
                .await
                .unwrap_err();
            assert!(error.is::<InvalidQuery>(), "{invalid}");
        }

        // Semantic matches are limited to the same visible, filtered documents.
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
            ids(
                pool,
                Some(&embeddings),
                &ada,
                json!({ "query": "unrelated" })
            )
            .await,
            [
                "google-drive:F1",
                "google-drive:F4",
                "slack-file:SF1",
                "slack-file:SF2",
                "slack:C1:2024-01-02:000000",
                "slack:C3:2024-02-01:000000"
            ]
        );
        assert_eq!(
            ids(
                pool,
                Some(&embeddings),
                &ada,
                json!({ "query": "unrelated", "filters": { "channel_ids": ["C3"] } })
            )
            .await,
            ["slack-file:SF2", "slack:C3:2024-02-01:000000"]
        );

        server.abort();
        database.drop().await;
    }

    #[tokio::test]
    async fn queries_can_only_read_the_data_schema() {
        let Ok(database_url) = env::var("COMPANY_CONTEXT_TEST_DATABASE_URL") else {
            eprintln!("skipping: set COMPANY_CONTEXT_TEST_DATABASE_URL to a ParadeDB Postgres URL");
            return;
        };
        let database = TestDatabase::create(&database_url, "query_role").await;
        let pool = &database.pool;

        let privileges: Vec<(String, String, bool, bool)> = sqlx::query_as(
            r#"
            SELECT n.nspname::text, c.relname::text,
                   has_table_privilege($1, c.oid, 'SELECT'),
                   has_table_privilege($1, c.oid, 'INSERT, UPDATE, DELETE, TRUNCATE')
            FROM pg_class c
            JOIN pg_namespace n ON n.oid = c.relnamespace
            WHERE n.nspname IN ('company_context_data', 'company_context_system')
              AND c.relkind IN ('r', 'p', 'v', 'm')
            "#,
        )
        .bind(QUERY_ROLE)
        .fetch_all(pool)
        .await
        .unwrap();
        assert!(
            privileges
                .iter()
                .any(|(schema, ..)| schema == "company_context_system")
        );
        for (schema, table, select, write) in privileges {
            assert_eq!(select, schema == "company_context_data", "{schema}.{table}");
            assert!(!write, "{schema}.{table}");
        }

        let mut tx = read_only(pool).await.unwrap();
        let role: String = sqlx::query_scalar("SELECT current_user::text")
            .fetch_one(&mut *tx)
            .await
            .unwrap();
        assert_eq!(role, QUERY_ROLE);
        let error = sqlx::query("SELECT 1 FROM company_context_system.slack_messages")
            .execute(&mut *tx)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("permission denied"), "{error}");
        drop(tx);

        database.drop().await;
    }

    #[tokio::test]
    async fn documents_are_returned_only_to_principals_that_observe_them() {
        let Ok(database_url) = env::var("COMPANY_CONTEXT_TEST_DATABASE_URL") else {
            eprintln!("skipping: set COMPANY_CONTEXT_TEST_DATABASE_URL to a ParadeDB Postgres URL");
            return;
        };
        let database = TestDatabase::create(&database_url, "query_document").await;
        let pool = &database.pool;
        seed(pool).await;
        let (ada, bob) = (ada(), bob());

        let drive = document(pool, &ada, "google-drive:F4")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(drive.data_type, DataType::DriveDoc);
        assert_eq!(drive.text, "falcon launch retro");
        assert_eq!(drive.metadata["document_type"], "pdf");
        let file = document(pool, &ada, "slack-file:SF2")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(file.data_type, DataType::SlackFile);
        let message = document(pool, &bob, "slack:C2:2024-01-02:000000")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(message.data_type, DataType::SlackMessage);
        assert_eq!(message.metadata["channel_name"], "secret");

        for (identity, document_id) in [
            (&ada, "google-drive:F2"),
            (&ada, "google-drive:F3"),
            (&ada, "slack:C2:2024-01-02:000000"),
            (&ada, "missing"),
            (&bob, "google-drive:F1"),
            (&bob, "slack-file:SF1"),
        ] {
            assert!(
                document(pool, identity, document_id)
                    .await
                    .unwrap()
                    .is_none(),
                "{document_id}"
            );
        }

        database.drop().await;
    }
}
