use centaur_console_db::{SecretValues, Subject, models::Config, oid};
use chrono::{DateTime, Utc};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::{
    ApiError,
    models::{AppState, ProxyRecord},
    tokens::{append_codemode_sandbox_token, append_sandbox_token},
};

pub(crate) async fn build_config(
    state: &AppState,
    proxy: &ProxyRecord,
    now: i64,
) -> Result<Config, ApiError> {
    let mut config = assemble_config(state, proxy).await?;
    append_sandbox_token(state, proxy, &mut config, now).await?;
    Ok(config)
}

/// The configuration the Code Mode tenant route receives: the same credentials
/// and requester wrappers, with the windowed Code Mode token appended.
pub(crate) async fn build_codemode_config(
    state: &AppState,
    proxy: &ProxyRecord,
    now: i64,
) -> Result<Config, ApiError> {
    let mut config = assemble_config(state, proxy).await?;
    append_codemode_sandbox_token(state, proxy, &mut config, now).await?;
    Ok(config)
}

/// The stored credentials for the record's principal and requester, without any
/// token the caller mints.
async fn assemble_config(state: &AppState, proxy: &ProxyRecord) -> Result<Config, ApiError> {
    let Some(principal_id) = proxy.principal_id else {
        return Ok(Config::default());
    };
    let subject = Subject {
        principal_id,
        principal: proxy.principal.as_ref(),
        console_user_id: proxy.console_user_id,
        console_user_email: proxy.console_user_email.as_deref(),
        slack_history_channel_ids: &proxy.slack_history_channel_ids,
        proxy_labels: &proxy.labels,
    };
    Ok(centaur_console_db::build_config(
        &state.pool,
        SecretValues::Live(&state.encryption),
        &subject,
        proxy.requester_principal_id,
    )
    .await?)
}

pub(crate) fn config_hash(proxy: &ProxyRecord, config: &Value) -> Result<String, ApiError> {
    let mut payload = config
        .as_object()
        .cloned()
        .ok_or_else(|| ApiError::internal("config was not an object"))?;
    payload.insert(
        "principal".to_owned(),
        proxy
            .principal_id
            .map(|id| Value::String(oid("prn", id)))
            .unwrap_or(Value::Null),
    );
    payload.insert(
        "principal_assigned_at".to_owned(),
        timestamp(proxy.principal_assigned_at),
    );
    payload.insert("proxy_labels".to_owned(), proxy.labels.clone());
    if proxy.requester_principal_id.is_some() {
        payload.insert(
            "requester_principal".to_owned(),
            proxy
                .requester_principal_id
                .map(|id| Value::String(oid("prn", id)))
                .unwrap_or(Value::Null),
        );
        payload.insert(
            "requester_principal_assigned_at".to_owned(),
            timestamp(proxy.requester_principal_assigned_at),
        );
    }
    let canonical = serde_json::to_vec(&Value::Object(payload))
        .map_err(|_| ApiError::internal("config serialization failed"))?;
    Ok(format!("sha256:{}", hex::encode(Sha256::digest(canonical))))
}

fn timestamp(value: Option<DateTime<Utc>>) -> Value {
    value
        .map(|value| Value::String(value.format("%Y-%m-%dT%H:%M:%SZ").to_string()))
        .unwrap_or(Value::Null)
}
