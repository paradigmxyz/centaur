//! Turn-token-authenticated lease API for nanocode brain hands.

use std::sync::Arc;

use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode, header},
};
use centaur_codemode_protocol::token::{
    HANDS_AUDIENCE, TurnClaims, TurnTokenSigner, TurnTokenVerifier, bearer,
};
use centaur_hands::{HandLease, HandPool, HandSize, LeaseRequest};
use serde::Deserialize;

use crate::{ApiError, AppState};

/// The hand pool plus the verifier for the hands tokens that authorize leases.
/// api-rs derives the verifier from its own signing key and binds it to the
/// `centaur-hands` audience, so a Code Mode turn token cannot lease a hand.
#[derive(Clone)]
pub struct HandsRuntime {
    pool: Arc<HandPool>,
    verifier: TurnTokenVerifier,
}

impl HandsRuntime {
    /// Build from api-rs's turn-token signer. The lease routes accept only
    /// tokens with the hands audience, never the Code Mode turn token.
    #[must_use]
    pub fn new(pool: Arc<HandPool>, signer: &TurnTokenSigner) -> Self {
        Self {
            pool,
            verifier: signer.verifier_for(HANDS_AUDIENCE),
        }
    }

    pub fn spawn_background(&self) {
        self.pool.clone().spawn_background();
    }

    fn claims(&self, headers: &HeaderMap) -> Result<TurnClaims, ApiError> {
        let value = headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .ok_or_else(|| ApiError::Unauthorized("a turn token is required".to_owned()))?;
        let token = bearer(value)
            .ok_or_else(|| ApiError::Unauthorized("a bearer turn token is required".to_owned()))?;
        self.verifier
            .verify(token)
            .map_err(|_| ApiError::Unauthorized("invalid turn token".to_owned()))
    }
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct CreateHandLeaseRequest {
    #[serde(default)]
    size: Option<HandSize>,
}

pub(crate) async fn create_hand_lease(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Option<Json<CreateHandLeaseRequest>>,
) -> Result<Json<HandLease>, ApiError> {
    let hands = hands(&state)?;
    let claims = hands.claims(&headers)?;
    let request = body.map(|Json(body)| body).unwrap_or_default();
    let lease = hands
        .pool
        .lease(LeaseRequest {
            thread_key: &claims.thread_key,
            principal: claims.principal(),
            requester: claims.requester.as_deref(),
            size: request.size,
        })
        .await?;
    Ok(Json(lease))
}

pub(crate) async fn current_hand_lease(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<HandLease>, ApiError> {
    let hands = hands(&state)?;
    let claims = hands.claims(&headers)?;
    hands
        .pool
        .current(&claims.thread_key)
        .await?
        .map(Json)
        .ok_or_else(|| ApiError::NotFound("this thread has no live hand lease".to_owned()))
}

pub(crate) async fn renew_hand_lease(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(lease_id): Path<String>,
) -> Result<Json<HandLease>, ApiError> {
    let hands = hands(&state)?;
    let claims = hands.claims(&headers)?;
    let lease = hands
        .pool
        .renew(
            &lease_id,
            &claims.thread_key,
            claims.principal(),
            claims.requester.as_deref(),
        )
        .await?;
    Ok(Json(lease))
}

pub(crate) async fn release_hand_lease(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(lease_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let hands = hands(&state)?;
    let claims = hands.claims(&headers)?;
    hands.pool.release(&lease_id, &claims.thread_key).await?;
    Ok(StatusCode::NO_CONTENT)
}

fn hands(state: &AppState) -> Result<HandsRuntime, ApiError> {
    state
        .hands()
        .ok_or_else(|| ApiError::NotFound("hands are not enabled".to_owned()))
}
