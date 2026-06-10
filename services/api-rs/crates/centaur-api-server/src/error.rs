use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use centaur_session_core::ThreadKeyError;
use centaur_session_runtime::SessionRuntimeError;
use centaur_session_sqlx::SessionStoreError;
use centaur_workflows::WorkflowRuntimeError;
use serde_json::json;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ApiError {
    #[error("{0}")]
    BadRequest(String),
    #[error("{0}")]
    Unauthorized(String),
    #[error("{0}")]
    NotFound(String),
    #[error("{0}")]
    MethodNotAllowed(String),
    #[error("{0}")]
    PayloadTooLarge(String),
    #[error(transparent)]
    Runtime(#[from] SessionRuntimeError),
    #[error(transparent)]
    Workflow(#[from] WorkflowRuntimeError),
    #[error(transparent)]
    Serialize(#[from] serde_json::Error),
}

impl From<ThreadKeyError> for ApiError {
    fn from(error: ThreadKeyError) -> Self {
        Self::BadRequest(error.to_string())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = match &self {
            Self::BadRequest(_) => StatusCode::BAD_REQUEST,
            Self::Unauthorized(_) => StatusCode::UNAUTHORIZED,
            Self::NotFound(_) => StatusCode::NOT_FOUND,
            Self::MethodNotAllowed(_) => StatusCode::METHOD_NOT_ALLOWED,
            Self::PayloadTooLarge(_) => StatusCode::PAYLOAD_TOO_LARGE,
            Self::Runtime(SessionRuntimeError::BadRequest(_)) => StatusCode::BAD_REQUEST,
            Self::Runtime(SessionRuntimeError::Store(SessionStoreError::NotFound { .. })) => {
                StatusCode::NOT_FOUND
            }
            Self::Runtime(SessionRuntimeError::Store(SessionStoreError::HarnessConflict {
                ..
            })) => StatusCode::CONFLICT,
            Self::Runtime(SessionRuntimeError::Store(SessionStoreError::PersonaConflict {
                ..
            })) => StatusCode::CONFLICT,
            Self::Workflow(WorkflowRuntimeError::BadRequest(_)) => StatusCode::BAD_REQUEST,
            Self::Workflow(WorkflowRuntimeError::NotFound(_)) => StatusCode::NOT_FOUND,
            Self::Runtime(_) | Self::Workflow(_) | Self::Serialize(_) => {
                StatusCode::INTERNAL_SERVER_ERROR
            }
        };
        let body = Json(json!({
            "ok": false,
            "error": self.to_string(),
        }));
        (status, body).into_response()
    }
}
