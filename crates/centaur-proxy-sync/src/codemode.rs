//! The tenancy iron-proxy's principal configuration endpoint.
//!
//! The proxy authenticates with the shared control-plane token and forwards the
//! turn token it verified as `X-Iron-Tenant-Token`. That token names the
//! principal and requester, so this route never trusts a caller-supplied id.
//! The response is the iron-proxy configuration for that tenant plus a windowed
//! api-rs token bound to `codemode`.

use axum::{Json, extract::State, http::HeaderMap};
use centaur_codemode_protocol::token::{TurnClaims, TurnTokenVerifier};
use centaur_console_db::decode_oid;
use chrono::Utc;
use serde_json::{Value, json};
use subtle::ConstantTimeEq;

use crate::{
    ApiError, SyncRequest,
    config::{build_codemode_config, config_hash},
    database::load_principal_record,
    models::AppState,
};

pub(crate) async fn tenant_config(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<SyncRequest>,
) -> Result<Json<Value>, ApiError> {
    authenticate_proxy(&state, &headers)?;
    let claims = verify_tenant_token(&state, &headers)?;
    let Some(principal_id) = decode_oid("prn", claims.principal()) else {
        return Err(ApiError::not_found("unknown principal"));
    };
    let requester_principal_id = match claims.requester.as_deref() {
        Some(requester) => Some(
            decode_oid("prn", requester)
                .ok_or_else(|| ApiError::not_found("unknown requester principal"))?,
        ),
        None => None,
    };
    let Some(record) =
        load_principal_record(&state.pool, principal_id, requester_principal_id).await?
    else {
        return Err(ApiError::not_found("unknown principal"));
    };
    let config = build_codemode_config(&state, &record, Utc::now().timestamp()).await?;
    let config_value = json!({
        "secrets": config.secrets,
        "transforms": config.transforms,
        "postgres": config.postgres,
    });
    let config_hash = config_hash(&record, &config_value)?;
    if request
        .config_hash
        .as_deref()
        .filter(|value| !value.is_empty())
        == Some(config_hash.as_str())
    {
        return Ok(Json(json!({ "config_hash": config_hash })));
    }
    Ok(Json(json!({
        "config_hash": config_hash,
        "secrets": config_value["secrets"],
        "transforms": config_value["transforms"],
    })))
}

/// The route exists only when the control-plane token is configured, and then
/// accepts exactly that bearer token.
fn authenticate_proxy(state: &AppState, headers: &HeaderMap) -> Result<(), ApiError> {
    let Some(expected) = state
        .codemode_sync_token
        .as_deref()
        .filter(|token| !token.trim().is_empty())
    else {
        // Off by default: without a configured control-plane token this route
        // has no credential it would accept.
        return Err(ApiError::not_found("route is not configured"));
    };
    let Some(presented) = crate::bearer_token(headers) else {
        return Err(ApiError::codemode_unauthorized());
    };
    if !bool::from(presented.as_bytes().ct_eq(expected.as_bytes())) {
        return Err(ApiError::codemode_unauthorized());
    }
    Ok(())
}

/// The tenant token only selects whose config comes back; the proxy credential
/// above proves the caller. A missing, tampered, or expired token is a refused
/// tenant.
fn verify_tenant_token(state: &AppState, headers: &HeaderMap) -> Result<TurnClaims, ApiError> {
    let Some(verifier) = state.codemode_token_verifier.as_ref() else {
        return Err(ApiError::codemode_forbidden(
            "turn token verification is not configured",
        ));
    };
    let Some(token) = headers
        .get("x-iron-tenant-token")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|token| !token.is_empty())
    else {
        return Err(ApiError::codemode_forbidden(
            "invalid or missing tenant token",
        ));
    };
    verifier
        .verify(token)
        .map_err(|_| ApiError::codemode_forbidden("invalid or missing tenant token"))
}

/// The turn token's public key, when configured. A deployment that enables the
/// route without a key refuses every tenant token rather than panicking.
pub(crate) fn token_verifier(pem: Option<&str>) -> Result<Option<TurnTokenVerifier>, String> {
    let Some(pem) = pem.filter(|pem| !pem.trim().is_empty()) else {
        return Ok(None);
    };
    TurnTokenVerifier::from_pem(pem.as_bytes())
        .map(Some)
        .map_err(|error| error.to_string())
}
