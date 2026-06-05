use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use centaur_session_core::ThreadKeyError;
use centaur_session_runtime::SessionRuntimeError;
use centaur_session_sqlx::SessionStoreError;
use serde_json::json;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ApiError {
    #[error("{0}")]
    BadRequest(String),
    #[error(transparent)]
    Runtime(#[from] SessionRuntimeError),
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
            Self::Runtime(_) | Self::Serialize(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        let body = Json(json!({
            "ok": false,
            "error": self.to_string(),
        }));
        (status, body).into_response()
    }
}
