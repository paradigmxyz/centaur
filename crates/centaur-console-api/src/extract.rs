//! Typed request extractors that fail with [`ApiError`] (so a bad request is
//! always the JSON error envelope):
//!
//! - [`Json<T>`] / [`Query<T>`] / [`Path<T>`] / [`Form<T>`];
//! - [`Data<T>`]: the bearer API's `{ "data": {...} }` request wrapper.

use axum::{
    extract::{FromRequest, FromRequestParts, Request},
    response::{IntoResponse, Response},
};
use serde::{Serialize, de::DeserializeOwned};

use super::{error::ApiError, state::AppState};

/// A JSON body (request) or response.
pub struct Json<T>(pub T);

impl<T: DeserializeOwned> FromRequest<AppState> for Json<T> {
    type Rejection = ApiError;

    async fn from_request(request: Request, state: &AppState) -> Result<Self, ApiError> {
        axum::Json::<T>::from_request(request, state)
            .await
            .map(|axum::Json(value)| Self(value))
            .map_err(|rejection| ApiError::BadRequest(format!("invalid JSON body: {rejection}")))
    }
}

impl<T: Serialize> IntoResponse for Json<T> {
    fn into_response(self) -> Response {
        axum::Json(self.0).into_response()
    }
}

/// `{ "data": T }` as a request body.
pub struct Data<T>(pub T);

#[derive(serde::Deserialize)]
struct Envelope<T> {
    data: T,
}

impl<T: DeserializeOwned> FromRequest<AppState> for Data<T> {
    type Rejection = ApiError;

    async fn from_request(request: Request, state: &AppState) -> Result<Self, ApiError> {
        let Json(Envelope { data }) = Json::<Envelope<T>>::from_request(request, state).await?;
        Ok(Self(data))
    }
}

/// A `{ "data": T }` response.
pub fn data<T: Serialize>(value: T) -> Json<serde_json::Value> {
    Json(serde_json::json!({ "data": value }))
}

/// The query string.
pub struct Query<T>(pub T);

impl<T: DeserializeOwned> FromRequestParts<AppState> for Query<T> {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut http::request::Parts,
        state: &AppState,
    ) -> Result<Self, ApiError> {
        axum::extract::Query::<T>::from_request_parts(parts, state)
            .await
            .map(|axum::extract::Query(value)| Self(value))
            .map_err(|rejection| ApiError::BadRequest(rejection.body_text()))
    }
}

/// Path parameters; one that does not parse is a 404 (an unknown id).
pub struct Path<T>(pub T);

impl<T: DeserializeOwned + Send> FromRequestParts<AppState> for Path<T> {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut http::request::Parts,
        state: &AppState,
    ) -> Result<Self, ApiError> {
        axum::extract::Path::<T>::from_request_parts(parts, state)
            .await
            .map(|axum::extract::Path(value)| Self(value))
            .map_err(|_| ApiError::not_found())
    }
}

/// A `application/x-www-form-urlencoded` body (server-rendered pages).
pub struct Form<T>(pub T);

impl<T: DeserializeOwned> FromRequest<AppState> for Form<T> {
    type Rejection = ApiError;

    async fn from_request(request: Request, state: &AppState) -> Result<Self, ApiError> {
        axum::Form::<T>::from_request(request, state)
            .await
            .map(|axum::Form(value)| Self(value))
            .map_err(|rejection| ApiError::BadRequest(rejection.body_text()))
    }
}
