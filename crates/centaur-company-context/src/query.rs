//! `POST /query` and `GET /documents/{document_id}`: hybrid retrieval and
//! document reads over the published corpora on behalf of the Console
//! principal named by the request's API JWT.
//!
//! A principal sees a document only while an active broker observation for its
//! Slack user ID, or for the subject of a Google or Granola credential granted
//! to it, still reaches the document's file, conversation, or Granola note. A
//! Slack conversation is also visible to its members while any live
//! credential observes it. Slack identities match by workspace and user ID.
//! A principal permitted to read a Slack channel's history through the proxy,
//! such as a Slack channel's own principal, also sees that channel while any
//! live credential observes it.
//! Calendar events are stored per Google identity, as Google showed them to it,
//! and are visible only to that identity. A custom document is visible to
//! the principals, roles, or everyone its readers name.
//! Queries run as [`QUERY_ROLE`], which can only read `company_context_data`.

use std::{
    collections::{HashMap, HashSet},
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
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use sqids::Sqids;
use sqlx::{
    Acquire, PgPool, Postgres, Transaction, postgres::PgArguments, query::QueryAs,
    types::Json as SqlJson,
};
use tracing::{error, warn};

use crate::{
    config::{
        CUSTOM_DOCUMENT_ID_PREFIX, Config, GOOGLE_CALENDAR_DOCUMENT_ID_PREFIX,
        GOOGLE_DRIVE_DOCUMENT_ID_PREFIX, GRANOLA_DOCUMENT_ID_PREFIX, SLACK_DOCUMENT_ID_PREFIX,
        SLACK_FILE_DOCUMENT_ID_PREFIX,
    },
    credentials::{ConsoleCredentials, PrincipalIdentity},
    embeddings::EmbeddingsClient,
};

const DEFAULT_LIMIT: usize = 10;
const MAX_LIMIT: usize = 50;
const MAX_QUERY_CHARS: usize = 2_000;
const MAX_FILTER_IDS: usize = 100;
/// Matches the size limit on custom documents' metadata.
const MAX_METADATA_FILTER_BYTES: usize = 65_536;
/// Candidates each lane contributes to rank fusion.
const MIN_CANDIDATES: usize = 20;
const RRF_K: f64 = 60.0;
const JWT_LEEWAY_SECONDS: u64 = 30;
const EMBEDDING_TIMEOUT: Duration = Duration::from_secs(10);
/// Database role with read-only access to `company_context_data` only.
const QUERY_ROLE: &str = "centaur_company_context_v2_query";

/// A custom document's metadata: the writer's, with its source, external
/// ID, and the principal that wrote it through the API (null for rows
/// written directly).
macro_rules! custom_metadata {
    () => {
        "d.metadata || jsonb_build_object('source', d.source, 'external_id', d.external_id, 'written_by', d.written_by)"
    };
}

/// The Slack conversations visible to the subjects `$2`: those the Slack user
/// `[$2[1], $2[2]]` reads, and the channels `$2[3:]` the principal may read
/// the history of while a live credential observes them.
macro_rules! slack_readable {
    () => {
        r#"(
                       SELECT r.conversation_id
                       FROM company_context_data.slack_conversation_readers r
                       WHERE r.team_id = $2[1] AND r.user_id = $2[2]
                       UNION
                       SELECT o.conversation_id
                       FROM company_context_data.slack_broker_observations o
                       WHERE o.active AND o.conversation_id = ANY($2[3:])
                   )"#
    };
}

#[derive(Clone)]
pub struct QueryState {
    pub pool: PgPool,
    pub credentials: Arc<ConsoleCredentials>,
    pub embeddings: EmbeddingsClient,
    pub jwt: Arc<JwtVerifier>,
    /// Enqueues projection of custom documents written through the API;
    /// absent while the custom indexer is disabled.
    pub custom_projection: Option<absurd::Client>,
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
    pub mode: Mode,
    #[serde(default)]
    pub filters: Filters,
    pub limit: Option<usize>,
    /// The `next_cursor` of the previous keyword page.
    pub cursor: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// Keyword and embedding similarity ranks, fused.
    #[default]
    Hybrid,
    /// Every document containing all of the query's terms, newest first and
    /// unranked, paged with a cursor.
    Keyword,
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
    /// Custom document sources. Limits the search to documents in them.
    #[serde(default)]
    pub sources: Vec<String>,
    /// A JSON object the returned metadata of custom documents must
    /// contain. Limits the search to custom documents.
    #[serde(default)]
    pub metadata: Map<String, Value>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DataType {
    SlackMessage,
    SlackFile,
    DriveDoc,
    CalendarEvent,
    GranolaNote,
    CustomDoc,
}

/// How one data type's documents are selected. Queries bind the principal's
/// subjects to `$2` and the filters to `$4` through `$9`.
struct Source {
    /// Prefix of the type's document IDs.
    id_prefix: &'static str,
    documents: &'static str,
    /// The type's embeddings table, or `None` if it is searched by keyword only.
    embeddings: Option<&'static str>,
    columns: &'static str,
    /// Documents visible to any subject in `$2`.
    visible: &'static str,
    /// Expressions for when the document's content starts and ends.
    starts_at: &'static str,
    ends_at: &'static str,
    /// Whether content ends before `ends_at`, as calendar events do.
    end_exclusive: bool,
    /// Documents in the conversations `$6`, or `None` if the type has none.
    in_channels: Option<&'static str>,
    /// The type's file ID column, or `None` if it has none.
    file_id: Option<&'static str>,
    /// The type's source column, or `None` if it has none.
    source: Option<&'static str>,
    /// The metadata expression `$9` must be contained in, or `None` if the
    /// type cannot be filtered by metadata.
    metadata: Option<&'static str>,
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
        let in_sources = self
            .source
            .map(|column| format!("{column} = ANY($8)"))
            .unwrap_or_else(|| "FALSE".to_owned());
        let in_metadata = self
            .metadata
            .map(|expression| format!("({expression}) @> $9"))
            .unwrap_or_else(|| "FALSE".to_owned());
        format!(
            r#"($4::timestamptz IS NULL OR {ends_at} {ends_after} $4)
               AND ($5::timestamptz IS NULL OR {starts_at} < $5)
               AND ($6::text[] IS NULL OR {in_channels})
               AND ($7::text[] IS NULL OR {in_files})
               AND ($8::text[] IS NULL OR {in_sources})
               AND ($9::jsonb IS NULL OR {in_metadata})"#,
            ends_at = self.ends_at,
            ends_after = if self.end_exclusive { ">" } else { ">=" },
            starts_at = self.starts_at,
        )
    }
}

impl DataType {
    const ALL: [Self; 6] = [
        Self::SlackMessage,
        Self::SlackFile,
        Self::DriveDoc,
        Self::CalendarEvent,
        Self::GranolaNote,
        Self::CustomDoc,
    ];

    /// The principal's identities the type's visibility is keyed by, or
    /// `None` if the principal can see none of the type's documents.
    fn subjects(self, identity: &PrincipalIdentity) -> Option<Vec<String>> {
        let subjects: Vec<String> = match self {
            // Slack user IDs are scoped to a workspace, so the subjects start
            // with the pair `[team_id, user_id]`, empty without a Slack user,
            // followed by the channels whose history the principal may read.
            Self::SlackMessage | Self::SlackFile => {
                let user = identity
                    .slack_team_id
                    .clone()
                    .zip(identity.slack_user_id.clone());
                if user.is_none() && identity.slack_history_channel_ids.is_empty() {
                    return None;
                }
                let (team_id, user_id) = user.unwrap_or_default();
                [team_id, user_id]
                    .into_iter()
                    .chain(identity.slack_history_channel_ids.iter().cloned())
                    .collect()
            }
            Self::DriveDoc | Self::CalendarEvent => identity.google_subjects.clone(),
            Self::GranolaNote => identity.granola_subjects.clone(),
            Self::CustomDoc => custom_readers(identity),
        };
        (!subjects.is_empty()).then_some(subjects)
    }

    fn source(self) -> Source {
        match self {
            Self::SlackMessage => Source {
                id_prefix: SLACK_DOCUMENT_ID_PREFIX,
                documents: "company_context_data.slack_documents",
                embeddings: Some("company_context_data.slack_document_embeddings"),
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
                visible: concat!("d.conversation_id IN ", slack_readable!()),
                // A channel day chunk matches a window its messages overlap.
                starts_at: "d.first_message_at",
                ends_at: "d.last_message_at",
                end_exclusive: false,
                in_channels: Some("d.conversation_id = ANY($6)"),
                file_id: None,
                source: None,
                metadata: None,
            },
            Self::SlackFile => Source {
                id_prefix: SLACK_FILE_DOCUMENT_ID_PREFIX,
                documents: "company_context_data.slack_file_documents",
                embeddings: Some("company_context_data.slack_file_document_embeddings"),
                columns: r#"d.document_id, d.title, d.body, NULLIF(d.url, '') AS url,
                   d.source_created_at AS occurred_at,
                   jsonb_build_object(
                       'file_id', d.file_id,
                       'mimetype', d.mimetype,
                       'filetype', d.filetype,
                       'author_id', d.author_id
                   ) AS metadata"#,
                visible: concat!(
                    r#"d.file_id IN (
                       SELECT s.file_id
                       FROM company_context_data.slack_file_shares s
                       WHERE s.conversation_id IN "#,
                    slack_readable!(),
                    ")"
                ),
                starts_at: "d.source_created_at",
                ends_at: "d.source_created_at",
                end_exclusive: false,
                // Only shares the subject can see count, so a filter does not
                // reveal where else a file was shared.
                in_channels: Some(concat!(
                    r#"d.file_id IN (
                       SELECT s.file_id
                       FROM company_context_data.slack_file_shares s
                       WHERE s.conversation_id = ANY($6)
                         AND s.conversation_id IN "#,
                    slack_readable!(),
                    ")"
                )),
                file_id: Some("d.file_id"),
                source: None,
                metadata: None,
            },
            Self::DriveDoc => Source {
                id_prefix: GOOGLE_DRIVE_DOCUMENT_ID_PREFIX,
                documents: "company_context_data.google_drive_documents",
                embeddings: Some("company_context_data.google_drive_document_embeddings"),
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
                       WHERE o.active AND o.provider_subject = ANY($2)
                   )"#,
                starts_at: "COALESCE(d.source_modified_at, d.source_created_at)",
                ends_at: "COALESCE(d.source_modified_at, d.source_created_at)",
                end_exclusive: false,
                in_channels: None,
                file_id: Some("d.file_id"),
                source: None,
                metadata: None,
            },
            Self::CalendarEvent => Source {
                id_prefix: GOOGLE_CALENDAR_DOCUMENT_ID_PREFIX,
                documents: "company_context_data.google_calendar_documents",
                embeddings: None,
                columns: r#"d.document_id, d.title, d.body, NULLIF(d.url, '') AS url,
                   d.starts_at AS occurred_at,
                   jsonb_build_object(
                       'calendar_id', d.calendar_id,
                       'event_id', d.event_id,
                       'starts_at', d.starts_at,
                       'ends_at', d.ends_at,
                       'organizer_email', d.organizer_email
                   ) AS metadata"#,
                visible: "d.provider_subject = ANY($2)",
                // An event matches a window it overlaps; a recurring event
                // by its first occurrence. Events are not embedded.
                starts_at: "d.starts_at",
                ends_at: "d.ends_at",
                end_exclusive: true,
                in_channels: None,
                file_id: None,
                source: None,
                metadata: None,
            },
            Self::GranolaNote => Source {
                id_prefix: GRANOLA_DOCUMENT_ID_PREFIX,
                documents: "company_context_data.granola_documents",
                embeddings: Some("company_context_data.granola_document_embeddings"),
                columns: r#"d.document_id, d.title, d.body, NULL::text AS url,
                   d.occurred_at,
                   jsonb_build_object(
                       'note_id', d.note_id,
                       'owner_email', d.owner_email,
                       'owner_name', d.owner_name,
                       'attendees', d.attendees
                   ) AS metadata"#,
                visible: r#"d.note_id IN (
                       SELECT o.note_id
                       FROM company_context_data.granola_broker_observations o
                       WHERE o.active AND o.provider_subject = ANY($2)
                   )"#,
                starts_at: "d.occurred_at",
                ends_at: "d.occurred_at",
                end_exclusive: false,
                in_channels: None,
                file_id: None,
                source: None,
                metadata: None,
            },
            Self::CustomDoc => Source {
                id_prefix: CUSTOM_DOCUMENT_ID_PREFIX,
                documents: "company_context_data.custom_document_chunks",
                embeddings: Some("company_context_data.custom_document_chunk_embeddings"),
                columns: concat!(
                    "d.document_id, d.title, d.body, NULL::text AS url, d.occurred_at, ",
                    custom_metadata!(),
                    " AS metadata"
                ),
                visible: "d.readers && $2",
                starts_at: "d.occurred_at",
                ends_at: "d.occurred_at",
                end_exclusive: false,
                in_channels: None,
                file_id: None,
                source: Some("d.source"),
                // Filters match the metadata documents are returned with.
                metadata: Some(custom_metadata!()),
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
            && (filters.sources.is_empty() || source.source.is_some())
            && (filters.metadata.is_empty() || source.metadata.is_some())
    }
}

#[derive(Debug, Serialize)]
pub struct QueryResponse {
    pub results: Vec<QueryResult>,
    /// Continues a keyword search that has more results.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct QueryResult {
    #[serde(flatten)]
    pub document: Document,
    /// The fused rank score; keyword results are unranked.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub score: Option<f64>,
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

#[derive(Clone, sqlx::FromRow)]
struct Row {
    document_id: String,
    title: String,
    body: String,
    url: Option<String>,
    occurred_at: Option<DateTime<Utc>>,
    metadata: SqlJson<Value>,
}

/// A document matched by one ranked lane, with its BM25 score or its cosine
/// distance from the query.
#[derive(sqlx::FromRow)]
struct Hit {
    #[sqlx(flatten)]
    row: Row,
    rank_score: f64,
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

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ApiError {
    BadRequest(String),
    Unauthorized,
    Forbidden(String),
    NotFound,
    Internal,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            Self::BadRequest(message) => (StatusCode::BAD_REQUEST, message),
            Self::Unauthorized => (StatusCode::UNAUTHORIZED, "invalid bearer token".to_owned()),
            Self::Forbidden(message) => (StatusCode::FORBIDDEN, message),
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
pub(crate) async fn authenticate(
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
        .ok_or_else(|| ApiError::Forbidden("unknown principal".to_owned()))
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
    let response = search(&state.pool, Some(&state.embeddings), &identity, &request)
        .await
        .map_err(internal("company_context_query_failed"))?;
    Ok(Json(response))
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

/// Bound filter parameters `$4` through `$9`.
struct FilterParams {
    occurred_after: Option<DateTime<Utc>>,
    occurred_before: Option<DateTime<Utc>>,
    channel_ids: Option<Vec<String>>,
    file_ids: Option<Vec<String>>,
    sources: Option<Vec<String>>,
    metadata: Option<SqlJson<Map<String, Value>>>,
}

impl FilterParams {
    fn new(filters: &Filters) -> Result<Self> {
        if let (Some(after), Some(before)) = (filters.occurred_after, filters.occurred_before)
            && after >= before
        {
            return Err(invalid("occurred_after must be before occurred_before"));
        }
        if serde_json::to_vec(&filters.metadata)?.len() > MAX_METADATA_FILTER_BYTES {
            return Err(invalid(format!(
                "metadata must be at most {MAX_METADATA_FILTER_BYTES} bytes"
            )));
        }
        Ok(Self {
            occurred_after: filters.occurred_after,
            occurred_before: filters.occurred_before,
            channel_ids: ids(&filters.channel_ids, "channel_ids")?,
            file_ids: ids(&filters.file_ids, "file_ids")?,
            sources: ids(&filters.sources, "sources")?,
            metadata: (!filters.metadata.is_empty()).then(|| SqlJson(filters.metadata.clone())),
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

/// Where a keyword page ends: the last result's `occurred_at` and document ID.
struct Cursor {
    occurred_at: Option<DateTime<Utc>>,
    document_id: String,
}

impl Cursor {
    fn after(document: &Document) -> String {
        let occurred_at = document
            .occurred_at
            .map(|at| at.to_rfc3339_opts(chrono::SecondsFormat::AutoSi, true))
            .unwrap_or_default();
        format!("{occurred_at}|{}", document.document_id)
    }

    fn parse(cursor: &str) -> Result<Self> {
        let parsed = cursor
            .split_once('|')
            .and_then(|(occurred_at, document_id)| {
                let occurred_at = match occurred_at {
                    "" => None,
                    at => Some(DateTime::parse_from_rfc3339(at).ok()?.to_utc()),
                };
                (!document_id.is_empty()).then(|| Self {
                    occurred_at,
                    document_id: document_id.to_owned(),
                })
            });
        parsed.ok_or_else(|| invalid("cursor is invalid"))
    }
}

/// Searches the requested data types visible to `identity`. Hybrid searches
/// fuse keyword and vector ranks, each taken across every type; without
/// embeddings, or when embedding the query fails, only keyword ranks are used.
/// Keyword searches return matches newest first, unranked.
pub async fn search(
    pool: &PgPool,
    embeddings: Option<&EmbeddingsClient>,
    identity: &PrincipalIdentity,
    request: &QueryRequest,
) -> Result<QueryResponse> {
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
    let cursor = match (&request.cursor, request.mode) {
        (None, _) => None,
        (Some(cursor), Mode::Keyword) => Some(Cursor::parse(cursor)?),
        (Some(_), Mode::Hybrid) => return Err(invalid("cursor requires keyword mode")),
    };
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
    let searches: Vec<(DataType, Vec<String>)> = types
        .into_iter()
        .filter_map(|data_type| Some((data_type, data_type.subjects(identity)?)))
        .collect();
    if searches.is_empty() {
        return Ok(QueryResponse {
            results: Vec::new(),
            next_cursor: None,
        });
    }
    if request.mode == Mode::Keyword {
        return keyword_search(pool, query, searches, limit, &params, cursor.as_ref()).await;
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

    let mut keyword_hits: Vec<(DataType, Hit)> = Vec::new();
    let mut semantic_hits: Vec<(DataType, Hit)> = Vec::new();
    for (data_type, subjects) in searches {
        let source = data_type.source();
        let keyword = format!(
            r#"
            SELECT {columns}, paradedb.score(d.document_id)::float8 AS rank_score
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
        let hits = lane(
            &mut tx, &keyword, query, None, &subjects, candidates, &params,
        )
        .await
        .with_context(|| format!("keyword search {data_type:?}"))?;

        if let (Some((model, vector)), Some(embeddings)) = (&vector, source.embeddings) {
            // Order by distance alone: the HNSW index cannot serve a
            // tie-breaker, and without the index every visible embedding is
            // compared and sorted.
            let semantic = format!(
                r#"
                SELECT {columns}, (e.embedding <=> $1::text::vector)::float8 AS rank_score
                FROM {embeddings} e
                JOIN {documents} d ON d.document_id = e.document_id
                WHERE e.model = $10
                  AND {visible}
                  AND {filters}
                ORDER BY e.embedding <=> $1::text::vector
                LIMIT $3
                "#,
                columns = source.columns,
                documents = source.documents,
                visible = source.visible,
                filters = source.filters(),
            );
            // Keyword matches are ranked by similarity too, even when they are
            // not among the nearest neighbors, as a long document mentioning
            // the query once may not be. They are already visible and filtered.
            let distances = format!(
                r#"
                SELECT e.document_id, (e.embedding <=> $1::text::vector)::float8
                FROM {embeddings} e
                WHERE e.model = $2 AND e.document_id = ANY($3)
                "#
            );
            let ids: Vec<&str> = hits
                .iter()
                .map(|hit| hit.row.document_id.as_str())
                .collect();
            // A slow or failing vector lane, such as an HNSW scan hitting the
            // statement timeout under a selective visibility filter, falls
            // back to keyword ranks. The savepoint keeps the transaction usable.
            let mut savepoint = tx.begin().await?;
            let result = async {
                let nearest = lane(
                    &mut savepoint,
                    &semantic,
                    vector,
                    Some(model),
                    &subjects,
                    candidates,
                    &params,
                )
                .await?;
                let distances: HashMap<String, f64> = sqlx::query_as(&distances)
                    .bind(vector)
                    .bind(model)
                    .bind(&ids)
                    .fetch_all(&mut *savepoint)
                    .await?
                    .into_iter()
                    .collect();
                anyhow::Ok((nearest, distances))
            }
            .await;
            match result {
                Ok((nearest, distances)) => {
                    savepoint.commit().await?;
                    semantic_hits.extend(nearest.into_iter().map(|hit| (data_type, hit)));
                    semantic_hits.extend(hits.iter().filter_map(|hit| {
                        let distance = *distances.get(&hit.row.document_id)?;
                        Some((
                            data_type,
                            Hit {
                                row: hit.row.clone(),
                                rank_score: distance,
                            },
                        ))
                    }));
                }
                Err(error) => {
                    savepoint.rollback().await?;
                    warn!(
                        event = "company_context_vector_search_failed",
                        data_type = ?data_type,
                        error = %format!("{error:#}"),
                    );
                }
            }
        }
        keyword_hits.extend(hits.into_iter().map(|hit| (data_type, hit)));
    }
    tx.commit().await?;

    // Rank across types, so each type does not place its own best match,
    // however weak, first. Distances share one model; BM25 scores come from
    // separate indexes, but are close enough to compare.
    keyword_hits.sort_by(|(_, a), (_, b)| {
        b.rank_score
            .total_cmp(&a.rank_score)
            .then_with(|| a.row.document_id.cmp(&b.row.document_id))
    });
    if let Some(embeddings) = embeddings {
        semantic_hits.retain(|(_, hit)| hit.rank_score <= embeddings.max_query_distance());
    }
    semantic_hits.sort_by(|(_, a), (_, b)| {
        a.rank_score
            .total_cmp(&b.rank_score)
            .then_with(|| a.row.document_id.cmp(&b.row.document_id))
    });
    // A keyword match may also be a nearest neighbor.
    let mut seen = HashSet::new();
    semantic_hits.retain(|(_, hit)| seen.insert(hit.row.document_id.clone()));
    Ok(QueryResponse {
        results: fuse([keyword_hits, semantic_hits], limit),
        next_cursor: None,
    })
}

/// Returns the next `limit` documents matching every query term, ordered by
/// `occurred_at` (missing last) and then document ID, both descending, from
/// after `cursor`.
async fn keyword_search(
    pool: &PgPool,
    query: &str,
    searches: Vec<(DataType, Vec<String>)>,
    limit: usize,
    params: &FilterParams,
    cursor: Option<&Cursor>,
) -> Result<QueryResponse> {
    let mut tx = read_only(pool).await?;
    sqlx::query("SET LOCAL plan_cache_mode = force_custom_plan")
        .execute(&mut *tx)
        .await?;
    let mut matches: Vec<(DataType, Row)> = Vec::new();
    for (data_type, subjects) in searches {
        let source = data_type.source();
        // Document IDs compare bytewise, as Rust orders them when merging.
        // Every type's `occurred_at` is its `starts_at`.
        let sql = format!(
            r#"
            SELECT {columns}
            FROM {documents} d
            WHERE (d.title &&& $1::text OR d.body &&& $1::text)
              AND {visible}
              AND {filters}
              AND ($10::text IS NULL OR CASE
                  WHEN $11::timestamptz IS NULL
                      THEN {at} IS NULL AND d.document_id COLLATE "C" < $10
                  ELSE {at} < $11 OR {at} IS NULL
                      OR ({at} = $11 AND d.document_id COLLATE "C" < $10)
              END)
            ORDER BY {at} DESC NULLS LAST, d.document_id COLLATE "C" DESC
            LIMIT $3
            "#,
            columns = source.columns,
            documents = source.documents,
            visible = source.visible,
            filters = source.filters(),
            at = source.starts_at,
        );
        // One extra row shows whether another page follows.
        let rows: Vec<Row> = bind_filters(
            sqlx::query_as(&sql)
                .bind(query)
                .bind(&subjects)
                .bind(limit as i64 + 1),
            params,
        )
        .bind(cursor.map(|cursor| cursor.document_id.as_str()))
        .bind(cursor.and_then(|cursor| cursor.occurred_at))
        .fetch_all(&mut *tx)
        .await
        .with_context(|| format!("keyword search {data_type:?}"))?;
        matches.extend(rows.into_iter().map(|row| (data_type, row)));
    }
    tx.commit().await?;

    matches.sort_by(|(_, a), (_, b)| {
        b.occurred_at
            .cmp(&a.occurred_at)
            .then_with(|| b.document_id.cmp(&a.document_id))
    });
    let more = matches.len() > limit;
    matches.truncate(limit);
    let results: Vec<QueryResult> = matches
        .into_iter()
        .map(|(data_type, row)| QueryResult {
            document: row.into_document(data_type),
            score: None,
        })
        .collect();
    let next_cursor = more
        .then(|| results.last().map(|result| Cursor::after(&result.document)))
        .flatten();
    Ok(QueryResponse {
        results,
        next_cursor,
    })
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
    let Some(subjects) = data_type.subjects(identity) else {
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
    .bind(&subjects)
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

/// Runs one ranked lane: `$1` is the query text or vector, `$2` the subjects,
/// `$3` the limit, `$4`..`$9` the filters, and `$10` the embedding model.
async fn lane(
    tx: &mut Transaction<'_, Postgres>,
    sql: &str,
    input: &str,
    model: Option<&str>,
    subjects: &[String],
    limit: i64,
    params: &FilterParams,
) -> Result<Vec<Hit>> {
    let mut query = bind_filters(
        sqlx::query_as(sql).bind(input).bind(subjects).bind(limit),
        params,
    );
    if let Some(model) = model {
        query = query.bind(model);
    }
    Ok(query.fetch_all(&mut **tx).await?)
}

/// Binds the filters to `$4` through `$9`.
fn bind_filters<'q, O>(
    query: QueryAs<'q, Postgres, O, PgArguments>,
    params: &'q FilterParams,
) -> QueryAs<'q, Postgres, O, PgArguments> {
    query
        .bind(params.occurred_after)
        .bind(params.occurred_before)
        .bind(params.channel_ids.as_deref())
        .bind(params.file_ids.as_deref())
        .bind(params.sources.as_deref())
        .bind(params.metadata.as_ref())
}

/// Reciprocal rank fusion of ranked lanes.
fn fuse(lanes: impl IntoIterator<Item = Vec<(DataType, Hit)>>, limit: usize) -> Vec<QueryResult> {
    let mut fused: HashMap<String, (Document, f64)> = HashMap::new();
    for lane in lanes {
        for (rank, (data_type, Hit { row, .. })) in lane.into_iter().enumerate() {
            let score = 1.0 / (RRF_K + rank as f64 + 1.0);
            fused
                .entry(row.document_id.clone())
                .or_insert_with(|| (row.into_document(data_type), 0.0))
                .1 += score;
        }
    }
    let mut results: Vec<_> = fused
        .into_values()
        .map(|(document, score)| QueryResult {
            document,
            score: Some(score),
        })
        .collect();
    results.sort_by(|a, b| {
        b.score
            .unwrap_or_default()
            .total_cmp(&a.score.unwrap_or_default())
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
        validation.set_audience(&["centaur-api"]);
        validation.set_issuer(&["centaur-console"]);
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

/// Mirrors the Rails OpaqueId concern: Sqids with an alphabet shuffled per
/// prefix and a minimum length of 8.
fn opaque_ids(prefix: &str) -> Sqids {
    let mut alphabet: Vec<char> = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789"
        .chars()
        .collect();
    alphabet.sort_by_cached_key(|character| {
        hex::encode(Sha256::digest(format!("{prefix}:{character}").as_bytes()))
    });
    Sqids::builder()
        .alphabet(alphabet)
        .min_length(8)
        .build()
        .expect("valid sqids settings")
}

static PRINCIPAL_SQIDS: LazyLock<Sqids> = LazyLock::new(|| opaque_ids("prn"));
static ROLE_SQIDS: LazyLock<Sqids> = LazyLock::new(|| opaque_ids("role"));

/// Encodes a Console record ID as its opaque ID, such as `prn_...`.
fn opaque_id(prefix: &str, sqids: &Sqids, id: i64) -> Option<String> {
    Some(format!(
        "{prefix}_{}",
        sqids.encode(&[id.try_into().ok()?]).ok()?
    ))
}

/// The principal's Console opaque ID (`prn_...`).
pub(crate) fn principal_oid(identity: &PrincipalIdentity) -> Option<String> {
    opaque_id("prn", &PRINCIPAL_SQIDS, identity.principal_id)
}

/// The custom document readers that match the principal: everyone, the
/// principal, and its roles, each by opaque ID and foreign ID.
pub(crate) fn custom_readers(identity: &PrincipalIdentity) -> Vec<String> {
    let principals = opaque_id("prn", &PRINCIPAL_SQIDS, identity.principal_id)
        .into_iter()
        .chain(identity.principal_foreign_id.clone());
    let roles = identity
        .role_ids
        .iter()
        .filter_map(|&id| opaque_id("role", &ROLE_SQIDS, id))
        .chain(identity.role_foreign_ids.iter().cloned());
    std::iter::once("public".to_owned())
        .chain(principals.map(|id| format!("principal:{id}")))
        .chain(roles.map(|id| format!("role:{id}")))
        .collect()
}

/// Decodes a Console principal opaque ID (`prn_...`), accepting only its
/// canonical encoding.
fn principal_id(oid: &str) -> Option<i64> {
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

    #[test]
    fn custom_readers_name_the_principal_and_its_roles_by_rails_ids() {
        let identity = PrincipalIdentity {
            principal_id: 123,
            principal_foreign_id: Some("ada".to_owned()),
            role_ids: vec![1, 123],
            role_foreign_ids: vec!["eng".to_owned()],
            ..Default::default()
        };
        assert_eq!(
            custom_readers(&identity),
            [
                "public",
                "principal:prn_yRoWctYw",
                "principal:ada",
                "role:role_1Uc5uCJR",
                "role:role_5iuMPpQI",
                "role:eng"
            ]
        );
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
            .results
            .into_iter()
            .map(|result| result.document.document_id)
            .collect();
        ids.sort();
        ids
    }

    fn ada() -> PrincipalIdentity {
        PrincipalIdentity {
            slack_user_id: Some("U-ADA".to_owned()),
            slack_team_id: Some("T1".to_owned()),
            google_subjects: vec!["G-ADA".to_owned()],
            granola_subjects: vec!["GR-ADA".to_owned()],
            ..Default::default()
        }
    }

    /// Bob has no Google identity.
    fn bob() -> PrincipalIdentity {
        PrincipalIdentity {
            slack_user_id: Some("U-BOB".to_owned()),
            slack_team_id: Some("T1".to_owned()),
            google_subjects: Vec::new(),
            granola_subjects: vec!["GR-BOB".to_owned(), "GR-BOB-2".to_owned()],
            ..Default::default()
        }
    }

    /// Ada observes Drive files F1 and F4 (not F3, whose observation is
    /// inactive) and Slack conversations C1 and C3; Bob observes C2. Slack
    /// file SF1 is shared in C1 and SF2 in both C2 and C3. Ada's Granola
    /// account observes note N1 (not N3, whose observation is inactive); Bob's
    /// second Granola account observes N2. Ada's Google account synced
    /// calendar events E1 and E2; Carol's synced E3.
    async fn seed(pool: &PgPool) {
        pool.execute(
            r#"
            INSERT INTO company_context_system.google_calendars
                (broker_credential_id, calendar_id, provider_subject)
            VALUES (1, 'CAL-ADA', 'G-ADA'), (5, 'CAL-TEAM', 'G-CAROL');
            INSERT INTO company_context_data.google_calendar_documents
                (document_id, broker_credential_id, provider_subject, calendar_id, event_id,
                 title, body, starts_at, ends_at, content_hash)
            VALUES
                ('google-calendar:1:CAL-ADA:E1', 1, 'G-ADA', 'CAL-ADA', 'E1',
                 'Falcon prep', 'falcon launch prep', '2024-01-03T10:00Z', '2024-01-03T11:00Z', 'hash'),
                ('google-calendar:1:CAL-ADA:E2', 1, 'G-ADA', 'CAL-ADA', 'E2',
                 'Falcon review', 'falcon launch review', '2024-01-04T10:00Z', '2024-01-04T11:00Z', 'hash'),
                ('google-calendar:5:CAL-TEAM:E3', 5, 'G-CAROL', 'CAL-TEAM', 'E3',
                 'Falcon 1:1', 'falcon launch private', '2024-01-04T12:00Z', '2024-01-04T13:00Z', 'hash');
            "#,
        )
        .await
        .unwrap();
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
            INSERT INTO company_context_data.slack_broker_identities
                (broker_credential_id, team_id, provider_subject)
            VALUES (10, 'T1', 'U-ADA'), (11, 'T1', 'U-BOB');
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

            INSERT INTO company_context_data.granola_broker_observations
                (broker_credential_id, note_id, provider_subject, active)
            VALUES (20, 'N1', 'GR-ADA', true), (21, 'N2', 'GR-BOB-2', true),
                   (20, 'N3', 'GR-ADA', false);
            INSERT INTO company_context_data.granola_documents
                (document_id, note_id, chunk_id, title, body, owner_email, occurred_at, content_hash)
            VALUES
                ('granola:N1:000000', 'N1', '000000', 'Falcon sync', 'falcon launch timeline', 'ada@example.com', '2024-01-05Z', 'hash'),
                ('granola:N2:000000', 'N2', '000000', 'Falcon sync', 'falcon launch timeline', 'bob@example.com', '2024-01-05Z', 'hash'),
                ('granola:N3:000000', 'N3', '000000', 'Falcon sync', 'falcon launch timeline', 'carol@example.com', '2024-01-05Z', 'hash');
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
                "google-calendar:1:CAL-ADA:E1",
                "google-calendar:1:CAL-ADA:E2",
                "google-drive:F1",
                "google-drive:F4",
                "granola:N1:000000",
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
        // Events match windows they overlap.
        assert_eq!(
            ids(
                pool,
                None,
                &ada,
                json!({ "query": "falcon", "filters": {
                    "types": ["calendar_event"],
                    "occurred_after": "2024-01-03T10:30:00Z",
                    "occurred_before": "2024-01-04T00:00:00Z",
                } })
            )
            .await,
            ["google-calendar:1:CAL-ADA:E1"]
        );
        // An event ends before its end time.
        assert_eq!(
            ids(
                pool,
                None,
                &ada,
                json!({ "query": "falcon", "filters": {
                    "types": ["calendar_event"],
                    "occurred_after": "2024-01-03T11:00:00Z",
                } })
            )
            .await,
            ["google-calendar:1:CAL-ADA:E2"]
        );
        assert!(
            ids(pool, None, &ada, json!({ "query": "unrelated" }))
                .await
                .is_empty()
        );
        assert_eq!(
            ids(pool, None, &bob, json!({ "query": "falcon" })).await,
            [
                "granola:N2:000000",
                "slack-file:SF2",
                "slack:C2:2024-01-02:000000"
            ]
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
            json!({ "query": "falcon", "filters": { "types": ["drive_doc"], "sources": ["archive"] } }),
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
            INSERT INTO company_context_data.granola_document_embeddings
                (document_id, model, dimensions, content_hash, embedding)
            SELECT document_id, 'text-embedding-3-small', 1536, 'hash', array_fill(0.5, ARRAY[1536])::vector
            FROM company_context_data.granola_documents;

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
            // Calendar events are matched by keyword only.
            [
                "google-drive:F1",
                "google-drive:F4",
                "granola:N1:000000",
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
    async fn search_ranks_matches_across_types_by_relevance() {
        let Ok(database_url) = env::var("COMPANY_CONTEXT_TEST_DATABASE_URL") else {
            eprintln!("skipping: set COMPANY_CONTEXT_TEST_DATABASE_URL to a ParadeDB Postgres URL");
            return;
        };
        let database = TestDatabase::create(&database_url, "query_ranking").await;
        let pool = &database.pool;
        seed(pool).await;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let router = Router::new().route("/embeddings", post(fake_embeddings));
        let server = tokio::spawn(async move { axum::serve(listener, router).await });
        let base_url = format!("http://{address}");
        let embeddings = EmbeddingsClient::new(&config(&["--openai-base-url", &base_url])).unwrap();
        let ranked = |body: Value| {
            let embeddings = &embeddings;
            async move {
                search(pool, Some(embeddings), &ada(), &request(body))
                    .await
                    .unwrap()
                    .results
                    .into_iter()
                    .map(|result| result.document.document_id)
                    .collect::<Vec<_>>()
            }
        };

        // Every query embeds to the all-0.5 vector: N1 is at cosine distance
        // 0, F1 at 0.5, and C1 at 1, beyond the default maximum.
        pool.execute(
            r#"
            INSERT INTO company_context_data.granola_document_embeddings
                (document_id, model, dimensions, content_hash, embedding)
            VALUES ('granola:N1:000000', 'text-embedding-3-small', 1536, 'hash',
                    array_fill(0.5, ARRAY[1536])::vector);
            INSERT INTO company_context_data.google_drive_document_embeddings
                (document_id, model, dimensions, content_hash, embedding)
            VALUES ('google-drive:F1', 'text-embedding-3-small', 1536, 'hash',
                    array_cat(array_fill(1, ARRAY[384]), array_fill(0, ARRAY[1152]))::vector);
            INSERT INTO company_context_data.slack_document_embeddings
                (document_id, model, dimensions, content_hash, embedding)
            VALUES ('slack:C1:2024-01-02:000000', 'text-embedding-3-small', 1536, 'hash',
                    array_cat(array_fill(1, ARRAY[768]), array_fill(-1, ARRAY[768]))::vector);
            "#,
        )
        .await
        .unwrap();
        // Semantic matches rank by similarity across types, down to a floor.
        assert_eq!(
            ranked(json!({ "query": "unrelated" })).await,
            ["granola:N1:000000", "google-drive:F1"]
        );

        // C3 mentions falcon but is not among the nearest neighbors the
        // vector lane returns; its distance still counts.
        pool.execute(
            r#"
            INSERT INTO company_context_data.slack_document_embeddings
                (document_id, model, dimensions, content_hash, embedding)
            VALUES ('slack:C3:2024-02-01:000000', 'text-embedding-3-small', 1536, 'hash',
                    array_cat(array_fill(1, ARRAY[384]), array_fill(0, ARRAY[1152]))::vector);
            INSERT INTO company_context_data.slack_documents
                (document_id, conversation_id, day, chunk_id, title, body, channel_name,
                 conversation_kind, first_message_at, last_message_at, content_hash)
            SELECT format('slack:C1:2024-01-02:n%s', n), 'C1', '2024-01-02', format('n%s', n),
                   '#general', 'weekly notes', 'general', 'public_channel',
                   '2024-01-02T09:00Z', '2024-01-02T17:00Z', 'hash'
            FROM generate_series(10, 34) n;
            INSERT INTO company_context_data.slack_document_embeddings
                (document_id, model, dimensions, content_hash, embedding)
            SELECT document_id, 'text-embedding-3-small', 1536, 'hash', array_fill(0.5, ARRAY[1536])::vector
            FROM company_context_data.slack_documents
            WHERE chunk_id LIKE 'n%';
            "#,
        )
        .await
        .unwrap();
        assert_eq!(
            ranked(json!({
                "query": "falcon",
                "filters": { "types": ["slack_message"] },
                "limit": 1,
            }))
            .await,
            ["slack:C3:2024-02-01:000000"]
        );

        server.abort();
        database.drop().await;
    }

    #[tokio::test]
    async fn keyword_search_pages_through_every_match_newest_first() {
        let Ok(database_url) = env::var("COMPANY_CONTEXT_TEST_DATABASE_URL") else {
            eprintln!("skipping: set COMPANY_CONTEXT_TEST_DATABASE_URL to a ParadeDB Postgres URL");
            return;
        };
        let database = TestDatabase::create(&database_url, "query_keyword").await;
        let pool = &database.pool;
        seed(pool).await;
        // A second C1 chunk starts at the same time as the first, and a file
        // in C1 has no creation time.
        pool.execute(
            r#"
            INSERT INTO company_context_data.slack_documents
                (document_id, conversation_id, day, chunk_id, title, body, channel_name,
                 conversation_kind, first_message_at, last_message_at, content_hash)
            VALUES ('slack:C1:2024-01-02:000001', 'C1', '2024-01-02', '1', '#general',
                    'falcon launch moved', 'general', 'public_channel',
                    '2024-01-02T09:00Z', '2024-01-02T10:00Z', 'hash');
            INSERT INTO company_context_system.slack_files (file_id, source_version, extraction_status)
            VALUES ('SF3', 'v1', 'completed');
            INSERT INTO company_context_data.slack_file_shares (file_id, conversation_id, message_ts)
            VALUES ('SF3', 'C1', '1.0');
            INSERT INTO company_context_data.slack_file_documents
                (document_id, file_id, chunk_id, title, body, content_hash)
            VALUES ('slack-file:SF3', 'SF3', '0', 'notes.txt', 'falcon launch notes', 'hash');
            "#,
        )
        .await
        .unwrap();
        let keyword = |body: Value| async move {
            let mut body = body;
            body["mode"] = json!("keyword");
            search(pool, None, &ada(), &request(body)).await.unwrap()
        };
        let slack = json!(["slack_message", "slack_file"]);

        let mut pages = Vec::new();
        let mut cursor = Value::Null;
        loop {
            let page = keyword(json!({
                "query": "falcon launch",
                "filters": { "types": slack },
                "limit": 2,
                "cursor": cursor,
            }))
            .await;
            assert!(page.results.iter().all(|result| result.score.is_none()));
            pages.push(
                page.results
                    .into_iter()
                    .map(|result| result.document.document_id)
                    .collect::<Vec<_>>(),
            );
            match page.next_cursor {
                Some(next) => cursor = json!(next),
                None => break,
            }
        }
        assert_eq!(
            pages,
            [
                vec!["slack:C3:2024-02-01:000000", "slack-file:SF2"],
                vec!["slack:C1:2024-01-02:000001", "slack:C1:2024-01-02:000000"],
                vec!["slack-file:SF1", "slack-file:SF3"],
            ]
        );

        // Every term must match.
        let matched = keyword(json!({
            "query": "falcon friday",
            "filters": { "types": slack },
        }))
        .await;
        assert_eq!(matched.results.len(), 1);
        assert_eq!(
            matched.results[0].document.document_id,
            "slack:C1:2024-01-02:000000"
        );
        assert_eq!(matched.next_cursor, None);

        // Filters apply as in hybrid searches.
        let filtered = keyword(json!({
            "query": "falcon",
            "filters": {
                "types": ["slack_message"],
                "channel_ids": ["C1", "C2"],
                "occurred_before": "2024-01-02T09:30:00Z",
            },
        }))
        .await;
        assert_eq!(
            filtered
                .results
                .iter()
                .map(|result| result.document.document_id.as_str())
                .collect::<Vec<_>>(),
            ["slack:C1:2024-01-02:000001", "slack:C1:2024-01-02:000000"]
        );

        for invalid in [
            json!({ "query": "falcon", "mode": "keyword", "cursor": "garbage" }),
            json!({ "query": "falcon", "cursor": "|slack-file:SF3" }),
        ] {
            let error = search(pool, None, &ada(), &request(invalid.clone()))
                .await
                .unwrap_err();
            assert!(error.is::<InvalidQuery>(), "{invalid}");
        }

        database.drop().await;
    }

    #[tokio::test]
    async fn slack_conversations_are_visible_to_members_while_observed() {
        let Ok(database_url) = env::var("COMPANY_CONTEXT_TEST_DATABASE_URL") else {
            eprintln!("skipping: set COMPANY_CONTEXT_TEST_DATABASE_URL to a ParadeDB Postgres URL");
            return;
        };
        let database = TestDatabase::create(&database_url, "query_members").await;
        let pool = &database.pool;
        seed(pool).await;
        // Carol has no Slack credential; she is a member of Bob's C2.
        let carol = PrincipalIdentity {
            slack_user_id: Some("U-CAROL".to_owned()),
            slack_team_id: Some("T1".to_owned()),
            ..Default::default()
        };
        pool.execute(
            r#"
            INSERT INTO company_context_data.slack_conversation_members
                (conversation_id, team_id, user_id)
            VALUES ('C2', 'T1', 'U-CAROL');
            "#,
        )
        .await
        .unwrap();
        let query = json!({ "query": "falcon" });

        // The same user ID in another workspace is someone else.
        let elsewhere = PrincipalIdentity {
            slack_team_id: Some("T2".to_owned()),
            ..carol.clone()
        };
        assert!(ids(pool, None, &elsewhere, query.clone()).await.is_empty());

        assert_eq!(
            ids(pool, None, &carol, query.clone()).await,
            ["slack-file:SF2", "slack:C2:2024-01-02:000000"]
        );
        assert!(
            document(pool, &carol, "slack:C2:2024-01-02:000000")
                .await
                .unwrap()
                .is_some()
        );

        // Membership grants access only while a live credential observes C2.
        pool.execute(
            "UPDATE company_context_data.slack_broker_observations SET active = FALSE WHERE conversation_id = 'C2'",
        )
        .await
        .unwrap();
        assert!(ids(pool, None, &carol, query).await.is_empty());

        database.drop().await;
    }

    #[tokio::test]
    async fn slack_channels_are_visible_to_principals_permitted_their_history() {
        let Ok(database_url) = env::var("COMPANY_CONTEXT_TEST_DATABASE_URL") else {
            eprintln!("skipping: set COMPANY_CONTEXT_TEST_DATABASE_URL to a ParadeDB Postgres URL");
            return;
        };
        let database = TestDatabase::create(&database_url, "query_channel_history").await;
        let pool = &database.pool;
        seed(pool).await;
        // A channel's principal has no Slack user; it may read C2's history.
        let channel = PrincipalIdentity {
            slack_history_channel_ids: vec!["C2".to_owned()],
            ..Default::default()
        };
        let query = json!({ "query": "falcon" });

        assert!(
            ids(pool, None, &PrincipalIdentity::default(), query.clone())
                .await
                .is_empty()
        );
        assert_eq!(
            ids(pool, None, &channel, query.clone()).await,
            ["slack-file:SF2", "slack:C2:2024-01-02:000000"]
        );
        assert!(
            document(pool, &channel, "slack:C2:2024-01-02:000000")
                .await
                .unwrap()
                .is_some()
        );
        // SF2 is also shared in C3, which the channel cannot read.
        assert!(
            ids(
                pool,
                None,
                &channel,
                json!({ "query": "falcon", "filters": { "channel_ids": ["C3"] } })
            )
            .await
            .is_empty()
        );

        // A Slack user keeps their own conversations alongside the channels.
        let bob_in_c1 = PrincipalIdentity {
            slack_history_channel_ids: vec!["C1".to_owned()],
            ..bob()
        };
        let both = ids(pool, None, &bob_in_c1, query.clone()).await;
        assert!(both.contains(&"slack:C1:2024-01-02:000000".to_owned()));
        assert!(both.contains(&"slack:C2:2024-01-02:000000".to_owned()));

        // The permission grants access only while a live credential observes C2.
        pool.execute(
            "UPDATE company_context_data.slack_broker_observations SET active = FALSE WHERE conversation_id = 'C2'",
        )
        .await
        .unwrap();
        assert!(ids(pool, None, &channel, query).await.is_empty());

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
        let note = document(pool, &ada, "granola:N1:000000")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(note.data_type, DataType::GranolaNote);
        assert_eq!(note.metadata["note_id"], "N1");
        let event = document(pool, &ada, "google-calendar:1:CAL-ADA:E1")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(event.data_type, DataType::CalendarEvent);
        assert_eq!(event.metadata["calendar_id"], "CAL-ADA");

        for (identity, document_id) in [
            (&ada, "google-drive:F2"),
            (&ada, "google-drive:F3"),
            (&ada, "slack:C2:2024-01-02:000000"),
            (&ada, "granola:N2:000000"),
            (&ada, "granola:N3:000000"),
            (&ada, "google-calendar:5:CAL-TEAM:E3"),
            (&bob, "google-calendar:1:CAL-ADA:E2"),
            (&ada, "missing"),
            (&bob, "google-drive:F1"),
            (&bob, "slack-file:SF1"),
            (&bob, "granola:N1:000000"),
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
