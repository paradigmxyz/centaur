//! Shared HTTP shapes for the Console API: Console's error envelope, the
//! not-found helpers, and the paginated label-search parameters.
//!
//! Errors render `{"error":{"message":...}}` (plus `details` when present);
//! keep that exact shape, which the SPA, connectors, and tools parse.

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap};

use axum::{
    Json,
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::Serialize;
use serde_json::{Map, Value, json};
use sqlx::PgPool;
use sqlx::error::BoxDynError;
use sqlx::postgres::{PgArguments, PgRow};
use sqlx::{Arguments, FromRow};

use centaur_console_db::FieldErrors;
use centaur_console_db::areas::principals::PrincipalError;
use centaur_console_db::areas::secrets::WriteError;

pub(crate) const DEFAULT_PAGE_LIMIT: i64 = 50;
pub(crate) const MAX_PAGE_LIMIT: i64 = 200;

/// A Console API failure with the wire status and envelope.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ConsoleError {
    pub(crate) status: StatusCode,
    pub(crate) message: String,
    pub(crate) details: Option<Value>,
}

impl ConsoleError {
    pub(crate) fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
            details: None,
        }
    }

    pub(crate) fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, message)
    }

    pub(crate) fn not_found(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, message)
    }

    pub(crate) fn unprocessable(message: impl Into<String>) -> Self {
        Self::new(StatusCode::UNPROCESSABLE_ENTITY, message)
    }

    /// `message: "validation failed"` with the field errors under `details`.
    pub(crate) fn validation_failed(details: Value) -> Self {
        Self::unprocessable("validation failed").with_details(details)
    }

    pub(crate) fn with_details(mut self, details: Value) -> Self {
        self.details = Some(details);
        self
    }

    pub(crate) fn with_reason(self, reason: &str) -> Self {
        self.with_details(json!({ "reason": reason }))
    }

    /// A server-side fault. The error is logged, never returned; the body is
    /// Console's envelope with a generic message, the same shape every other
    /// 5xx uses.
    pub(crate) fn internal(error: impl std::fmt::Display) -> Self {
        tracing::error!(error = %error, "console request failed");
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, "internal server error")
    }
}

/// The validation errors are Console's 422 `details`; a data-layer validator can
/// therefore return its `FieldErrors` with `?`.
impl From<FieldErrors> for ConsoleError {
    fn from(errors: FieldErrors) -> Self {
        Self::validation_failed(errors.into_value())
    }
}

/// A database failure a handler did not classify is an opaque 500.
impl From<sqlx::Error> for ConsoleError {
    fn from(error: sqlx::Error) -> Self {
        Self::internal(error)
    }
}

/// The data layer's own failures. A missing row is Console's plain
/// `"<Model> not found"` 404; anything else is an opaque 500.
impl From<centaur_console_db::Error> for ConsoleError {
    fn from(error: centaur_console_db::Error) -> Self {
        match error {
            centaur_console_db::Error::NotFound(message) => Self::not_found(message),
            error => Self::internal(error),
        }
    }
}

/// The secrets write/delete failures.
impl From<WriteError> for ConsoleError {
    fn from(error: WriteError) -> Self {
        match error {
            WriteError::NotFound(message) => Self::not_found(message),
            WriteError::Validation(details) => Self::validation_failed(details),
            WriteError::BadRequest(message) => Self::bad_request(message),
            WriteError::Internal(error) => Self::internal(error),
        }
    }
}

/// The principals area's failures. Internal/database/encryption details are
/// logged, never echoed in the 500 body.
impl From<PrincipalError> for ConsoleError {
    fn from(error: PrincipalError) -> Self {
        match error {
            PrincipalError::NotFound(message) => Self::not_found(message),
            PrincipalError::BadRequest(message) => Self::bad_request(message),
            PrincipalError::Validation(errors) => Self::validation_failed(errors.into_value()),
            PrincipalError::Internal(error) => Self::internal(error),
            PrincipalError::Database(error) => Self::internal(error),
            PrincipalError::Encryption(error) => Self::internal(error),
        }
    }
}

impl IntoResponse for ConsoleError {
    fn into_response(self) -> Response {
        render_error(self.status, &self.message, self.details)
    }
}

/// Console's JSON error envelope.
pub(crate) fn render_error(status: StatusCode, message: &str, details: Option<Value>) -> Response {
    let mut error = json!({ "message": message });
    if let Some(details) = details {
        error["details"] = details;
    }
    (status, Json(json!({ "error": error }))).into_response()
}

/// Console's 404 body for an unknown record or route.
pub(crate) fn not_found(message: &str) -> Response {
    render_error(StatusCode::NOT_FOUND, message, None)
}

/// The JSON fallback for routes outside the Console API. Unknown paths get the
/// same envelope instead of an empty body.
pub(crate) fn json_not_found() -> Response {
    not_found("not found")
}

/// A pagination value is a plain signed integer: no prefix parsing, no
/// whitespace/underscore separators, and no arbitrary-precision integers. A
/// value outside `i64` is invalid (a 400), not saturated. Defaults and the
/// existing clamps are unchanged.
fn parse_integer(raw: &str) -> Option<i64> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    raw.parse::<i64>().ok()
}

/// The page and limit of one paginated list response.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Pagination {
    pub(crate) page: i64,
    pub(crate) limit: i64,
}

impl Pagination {
    pub(crate) fn from_query(query: &HashMap<String, String>) -> Result<Self, ConsoleError> {
        Self::from_optional(
            query.get("page").map(String::as_str),
            query.get("limit").map(String::as_str),
        )
    }

    pub(crate) fn from_optional(
        page: Option<&str>,
        limit: Option<&str>,
    ) -> Result<Self, ConsoleError> {
        let limit = match limit.map(str::trim).filter(|value| !value.is_empty()) {
            None => DEFAULT_PAGE_LIMIT,
            Some(raw) => parse_integer(raw)
                .ok_or_else(|| ConsoleError::bad_request("limit must be an integer"))?
                .clamp(1, MAX_PAGE_LIMIT),
        };
        let page = match page.map(str::trim).filter(|value| !value.is_empty()) {
            None => 1,
            Some(raw) => parse_integer(raw)
                .ok_or_else(|| ConsoleError::bad_request("page must be an integer"))?
                .max(1),
        };
        Ok(Self { page, limit })
    }

    pub(crate) fn offset(self) -> i64 {
        (self.page - 1).saturating_mul(self.limit)
    }

    pub(crate) fn meta(self, total: i64) -> Value {
        let total_pages = if total == 0 {
            0
        } else {
            total.saturating_add(self.limit - 1) / self.limit
        };
        json!({
            "page": self.page,
            "limit": self.limit,
            "total": total,
            "total_pages": total_pages,
        })
    }
}

/// The query parameters of one paginated label search.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ListParams {
    pub(crate) pagination: Pagination,
    /// `labels[key]=value` filters, as Console's GIN containment query needs
    /// them. Values are scalar strings on the wire; missing brackets are not a
    /// label filter.
    pub(crate) labels: BTreeMap<String, String>,
}

impl ListParams {
    pub(crate) fn from_query(query: &HashMap<String, String>) -> Result<Self, ConsoleError> {
        Ok(Self {
            pagination: Pagination::from_query(query)?,
            labels: label_filters(query)?,
        })
    }
}

/// Extracts `labels[key]=value` pairs. A non-hash `labels` and non-scalar
/// values are a 400; urlencoded query strings only produce scalar
/// strings, so bracket-shaped keys are the whole contract here.
pub(crate) fn label_filters(
    query: &HashMap<String, String>,
) -> Result<BTreeMap<String, String>, ConsoleError> {
    let mut labels = BTreeMap::new();
    for (key, value) in query {
        if key == "labels" {
            // A blank `labels=` param is the same as no filter.
            if value.trim().is_empty() {
                continue;
            }
            return Err(ConsoleError::bad_request(
                "labels must be a hash of key=value pairs",
            ));
        }
        if let Some(label) = key
            .strip_prefix("labels[")
            .and_then(|key| key.strip_suffix(']'))
        {
            if label.is_empty() {
                return Err(ConsoleError::bad_request(
                    "labels must be a hash of key=value pairs",
                ));
            }
            // `labels[key][nested]=value` is a nested hash/array, which is
            // refused because label values must be scalars.
            if let Some((name, _)) = label.split_once(']') {
                return Err(ConsoleError::bad_request(format!(
                    "label value for {name} must be a scalar"
                )));
            }
            labels.insert(label.to_owned(), value.clone());
        }
    }
    Ok(labels)
}

/// The `labels` containment value a list query binds: the same scalar key/value
/// pairs, as a JSON object for `labels @> $n`. Because [`label_filters`] already
/// rejected non-scalar shapes, every value is a JSON string.
pub(crate) fn labels_value(labels: &BTreeMap<String, String>) -> Map<String, Value> {
    labels
        .iter()
        .map(|(key, value)| (key.clone(), Value::String(value.clone())))
        .collect()
}

/// The rows one list page selects: the projection, the filter that both the
/// page and its `COUNT` use, and the ordering.
///
/// `filters` is the number of parameters `from_where` already binds ($1..=$n).
pub(crate) struct ListSource {
    select: Cow<'static, str>,
    from_where: &'static str,
    order_by: &'static str,
    filters: usize,
}

impl ListSource {
    pub(crate) const fn new(
        select: &'static str,
        from_where: &'static str,
        order_by: &'static str,
        filters: usize,
    ) -> Self {
        Self {
            select: Cow::Borrowed(select),
            from_where,
            order_by,
            filters,
        }
    }

    /// The same source with a projection built at runtime, for a list that
    /// selects through a shared column constant.
    pub(crate) fn owned(
        select: String,
        from_where: &'static str,
        order_by: &'static str,
        filters: usize,
    ) -> Self {
        Self {
            select: Cow::Owned(select),
            from_where,
            order_by,
            filters,
        }
    }
}

/// One paginated list page: the `COUNT` and the page query over the same
/// filter, rendered as Console's `{data, meta}`.
///
/// The two statements are built together from one [`ListSource`], so a route
/// cannot count one set of rows and return another. `bind` attaches the filter
/// values; the helper appends `LIMIT $n+1 OFFSET $n+2` for the page, and
/// `render` turns each row into its response payload.
///
/// ```ignore
/// const KEYS: ListSource = ListSource::new(
///     "SELECT id, name, created_at, updated_at",
///     "FROM api_keys WHERE user_id = $1 AND deleted_at IS NULL",
///     "ORDER BY created_at ASC, id ASC",
///     1,
/// );
///
/// let params = ListParams::from_query(&query)?;
/// let body = list::<ApiKeyRow, ApiKeyPayload, _, _>(
///     &pool,
///     &params,
///     KEYS,
///     |arguments| bind(arguments, user_id),
///     payload,
/// )
/// .await?;
/// ```
pub(crate) async fn list<Row, Payload, Bind, Render>(
    pool: &PgPool,
    params: &ListParams,
    source: ListSource,
    bind: Bind,
    render: Render,
) -> Result<Value, ConsoleError>
where
    Row: for<'r> FromRow<'r, PgRow> + Send + Unpin,
    Payload: Serialize,
    Bind: Fn(&mut PgArguments) -> std::result::Result<(), BoxDynError>,
    Render: Fn(&Row) -> Payload,
{
    let pagination = params.pagination;
    let (rows, total) = list_rows(pool, params, source, bind).await?;
    let data: Vec<Payload> = rows.iter().map(&render).collect();
    Ok(list_response(pagination, &data, total))
}

/// The `COUNT` and page query of one list, without rendering: an area whose
/// payload needs another async read hydrates the rows itself and calls
/// [`list_response`].
pub(crate) async fn list_rows<Row, Bind>(
    pool: &PgPool,
    params: &ListParams,
    source: ListSource,
    bind: Bind,
) -> Result<(Vec<Row>, i64), ConsoleError>
where
    Row: for<'r> FromRow<'r, PgRow> + Send + Unpin,
    Bind: Fn(&mut PgArguments) -> std::result::Result<(), BoxDynError>,
{
    let pagination = params.pagination;
    let ListSource {
        select,
        from_where,
        order_by,
        filters,
    } = source;
    let count_sql = format!("SELECT count(*) {from_where}");
    let page_sql = format!(
        "{select} {from_where} {order_by} LIMIT ${} OFFSET ${}",
        filters + 1,
        filters + 2
    );

    let mut count_args = PgArguments::default();
    bind(&mut count_args).map_err(ConsoleError::internal)?;
    let total: i64 = sqlx::query_scalar_with(&count_sql, count_args)
        .fetch_one(pool)
        .await?;

    let mut page_args = PgArguments::default();
    bind(&mut page_args).map_err(ConsoleError::internal)?;
    page_args
        .add(pagination.limit)
        .map_err(ConsoleError::internal)?;
    page_args
        .add(pagination.offset())
        .map_err(ConsoleError::internal)?;
    let rows: Vec<Row> = sqlx::query_as_with(&page_sql, page_args)
        .fetch_all(pool)
        .await?;

    Ok((rows, total))
}

/// Binds one value for [`list`]'s filter closure. `PgArguments::add` is private
/// in sqlx, so a list route goes through this helper instead of calling it.
///
/// ```ignore
/// |arguments| {
///     bind(arguments, user_id)?;
///     bind(arguments, &status)
/// }
/// ```
pub(crate) fn bind<T>(arguments: &mut PgArguments, value: T) -> std::result::Result<(), BoxDynError>
where
    T: for<'q> sqlx::Encode<'q, sqlx::Postgres> + sqlx::Type<sqlx::Postgres>,
{
    Arguments::add(arguments, value)
}

/// Console's paginated list body: the page under `data`, the same `meta` every
/// paginated endpoint renders.
pub(crate) fn list_response<T: Serialize>(pagination: Pagination, rows: &[T], total: i64) -> Value {
    json!({
        "data": rows,
        "meta": pagination.meta(total),
    })
}

/// Console's `{"data": ...}` envelope for a fixed-shape payload. Areas used to
/// each own a copy of this struct; use this one.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct DataPayload<T> {
    pub(crate) data: T,
}

/// A `200` document response: the payload under `data`.
pub(crate) fn json_data<T: Serialize>(value: T) -> Response {
    Json(DataPayload { data: value }).into_response()
}

/// A `201` document response, Console's status for a write that inserted.
pub(crate) fn created_data<T: Serialize>(value: T) -> Response {
    (StatusCode::CREATED, Json(DataPayload { data: value })).into_response()
}

/// Console's empty success body (`204`), used by every delete.
pub(crate) fn no_content() -> Response {
    StatusCode::NO_CONTENT.into_response()
}

/// The request origin: the `Origin` header only, with no `Referer` fallback.
/// Every unsafe console request, JSON or form, must carry exactly
/// the configured public origin; a missing or `null` Origin is refused.
pub(crate) fn request_origin(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pagination_parses_plain_integers_and_keeps_the_clamps() {
        // A plain i64 is required: no `_` separators and no unbounded
        // integers, with the same 400 messages.
        assert_eq!(
            Pagination::from_optional(Some("2"), Some("1")).unwrap(),
            Pagination { page: 2, limit: 1 }
        );
        assert_eq!(
            Pagination::from_optional(None, None).unwrap(),
            Pagination { page: 1, limit: 50 }
        );
        // A blank value keeps the default.
        assert_eq!(
            Pagination::from_optional(Some(" "), Some(" ")).unwrap(),
            Pagination { page: 1, limit: 50 }
        );
        assert_eq!(
            Pagination::from_optional(Some("0"), Some("0")).unwrap(),
            Pagination { page: 1, limit: 1 }
        );
        assert_eq!(
            Pagination::from_optional(Some("-3"), Some("999")).unwrap(),
            Pagination {
                page: 1,
                limit: 200
            }
        );
        for raw in ["1_0", "99999999999999999999", "1.5", "two"] {
            let error = Pagination::from_optional(Some(raw), None).unwrap_err();
            assert_eq!(error.status, StatusCode::BAD_REQUEST, "{raw}");
            assert_eq!(error.message, "page must be an integer", "{raw}");
            let error = Pagination::from_optional(None, Some(raw)).unwrap_err();
            assert_eq!(error.status, StatusCode::BAD_REQUEST, "{raw}");
            assert_eq!(error.message, "limit must be an integer", "{raw}");
        }
    }

    #[test]
    fn list_response_is_data_and_page_meta() {
        let pagination = Pagination { page: 2, limit: 2 };
        assert_eq!(
            list_response(pagination, &["a", "b"], 3),
            json!({
                "data": ["a", "b"],
                "meta": {"page": 2, "limit": 2, "total": 3, "total_pages": 2},
            })
        );
    }

    #[test]
    fn labels_value_re_encodes_scalar_strings() {
        let labels = BTreeMap::from([
            ("env".to_owned(), "prod".to_owned()),
            ("team".to_owned(), "core".to_owned()),
        ]);
        assert_eq!(
            Value::Object(labels_value(&labels)),
            json!({"env": "prod", "team": "core"})
        );
    }

    #[test]
    fn request_origin_reads_only_the_origin_header() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::REFERER,
            "https://console.example/x/y?q=1".parse().unwrap(),
        );
        assert_eq!(request_origin(&headers), None);
        headers.insert(header::ORIGIN, "https://other.example".parse().unwrap());
        assert_eq!(request_origin(&headers), Some("https://other.example"));
    }
}
