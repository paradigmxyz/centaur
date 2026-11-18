//! One error type for every handler: `{"error":{"message":..,"details":..}}`
//! with the right status. Validation failures carry `details` as
//! `{field: [messages]}`.

use std::collections::BTreeMap;

use axum::{
    Json,
    response::{IntoResponse, Response},
};
use centaur_console_auth::Denial;
use http::StatusCode;
use serde_json::{Value, json};

/// `{field: [messages]}` for a 422.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FieldErrors(BTreeMap<String, Vec<String>>);

impl FieldErrors {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&mut self, field: &str, message: impl Into<String>) {
        self.0
            .entry(field.to_owned())
            .or_default()
            .push(message.into());
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// `Err(ApiError::Invalid)` when any error was recorded.
    pub fn finish(self) -> Result<(), ApiError> {
        if self.is_empty() {
            Ok(())
        } else {
            Err(ApiError::Invalid(self))
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    /// 400: a malformed request.
    #[error("{0}")]
    BadRequest(String),
    /// 401/403 from a credential check.
    #[error("{}", .0.message)]
    Denied(Denial),
    /// 403 for an authenticated caller that may not do this.
    #[error("{0}")]
    Forbidden(String),
    /// 404.
    #[error("{0}")]
    NotFound(String),
    /// 405.
    #[error("method not allowed")]
    MethodNotAllowed,
    /// 409.
    #[error("{0}")]
    Conflict(String),
    /// 422 "validation failed" with per-field messages.
    #[error("validation failed")]
    Invalid(FieldErrors),
    /// 422 for a rule that is not about one field. `reason` is a stable
    /// machine-readable code in `details.reason`.
    #[error("{message}")]
    Unprocessable {
        message: String,
        reason: Option<&'static str>,
    },
    /// An upstream or availability failure with its own status (502/503):
    /// the message is returned, `reason` is a stable code in `details.reason`.
    #[error("{message}")]
    Upstream {
        status: StatusCode,
        message: String,
        reason: Option<&'static str>,
    },
    /// 500. The message is logged, never returned.
    #[error("{0}")]
    Internal(String),
}

pub type ApiResult<T = Response> = Result<T, ApiError>;

impl ApiError {
    pub fn not_found() -> Self {
        Self::NotFound("not found".into())
    }

    pub fn upstream(
        status: StatusCode,
        message: impl Into<String>,
        reason: Option<&'static str>,
    ) -> Self {
        Self::Upstream {
            status,
            message: message.into(),
            reason,
        }
    }

    pub fn internal(context: impl std::fmt::Display) -> Self {
        Self::Internal(context.to_string())
    }

    /// A single-field 422.
    pub fn field(field: &str, message: impl Into<String>) -> Self {
        let mut errors = FieldErrors::new();
        errors.add(field, message);
        Self::Invalid(errors)
    }

    /// Maps a unique-constraint violation to a field error, anything else
    /// to a 500: `insert(...).await.map_err(|e| ApiError::unique(e, "email",
    /// "has already been taken"))`.
    pub fn unique(error: sqlx::Error, field: &str, message: &str) -> Self {
        if is_unique_violation(&error) {
            Self::field(field, message)
        } else {
            error.into()
        }
    }

    pub fn status(&self) -> StatusCode {
        match self {
            Self::BadRequest(_) => StatusCode::BAD_REQUEST,
            Self::Denied(denial) => {
                StatusCode::from_u16(denial.status).unwrap_or(StatusCode::FORBIDDEN)
            }
            Self::Forbidden(_) => StatusCode::FORBIDDEN,
            Self::NotFound(_) => StatusCode::NOT_FOUND,
            Self::MethodNotAllowed => StatusCode::METHOD_NOT_ALLOWED,
            Self::Conflict(_) => StatusCode::CONFLICT,
            Self::Invalid(_) | Self::Unprocessable { .. } => StatusCode::UNPROCESSABLE_ENTITY,
            Self::Upstream { status, .. } => *status,
            Self::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    fn body(&self) -> Value {
        let (message, details) = match self {
            Self::Denied(denial) => (
                denial.message.to_owned(),
                denial.reason.map(|reason| json!({ "reason": reason })),
            ),
            Self::Invalid(errors) => ("validation failed".to_owned(), Some(json!(errors.0))),
            Self::Unprocessable { message, reason }
            | Self::Upstream {
                message, reason, ..
            } => (
                message.clone(),
                reason.map(|reason| json!({ "reason": reason })),
            ),
            Self::Internal(_) => ("internal server error".to_owned(), None),
            other => (other.to_string(), None),
        };
        let mut error = json!({ "message": message });
        if let Some(details) = details {
            error["details"] = details;
        }
        json!({ "error": error })
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        if let Self::Internal(context) = &self {
            tracing::error!(%context, "console internal error");
        }
        (self.status(), Json(self.body())).into_response()
    }
}

impl From<Denial> for ApiError {
    fn from(denial: Denial) -> Self {
        Self::Denied(denial)
    }
}

impl From<sqlx::Error> for ApiError {
    fn from(error: sqlx::Error) -> Self {
        Self::Internal(format!("database error: {error}"))
    }
}

impl From<centaur_console_db::Error> for ApiError {
    fn from(error: centaur_console_db::Error) -> Self {
        Self::Internal(format!("console database error: {error}"))
    }
}

/// Whether `error` is a Postgres unique-constraint violation.
pub fn is_unique_violation(error: &sqlx::Error) -> bool {
    error
        .as_database_error()
        .is_some_and(|error| error.is_unique_violation())
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn render(error: ApiError) -> (StatusCode, Value) {
        let response = error.into_response();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    #[tokio::test]
    async fn renders_the_error_envelope() {
        let (status, body) = render(ApiError::BadRequest("bad".into())).await;
        assert_eq!(status, 400);
        assert_eq!(body, json!({"error": {"message": "bad"}}));
        let (status, body) = render(ApiError::Denied(Denial::CSRF)).await;
        assert_eq!(status, 403);
        assert_eq!(body["error"]["details"]["reason"], "csrf");
        let mut errors = FieldErrors::new();
        errors.add("email", "has already been taken");
        errors.add("email", "is invalid");
        let (status, body) = render(ApiError::Invalid(errors)).await;
        assert_eq!(status, 422);
        assert_eq!(body["error"]["message"], "validation failed");
        assert_eq!(
            body["error"]["details"]["email"],
            json!(["has already been taken", "is invalid"])
        );
        let (status, body) = render(ApiError::internal("secret detail")).await;
        assert_eq!(status, 500);
        assert!(!body.to_string().contains("secret detail"));
    }
}
