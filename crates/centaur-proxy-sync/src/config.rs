use centaur_console_db::{SecretValues, Subject, models::Config, oid};
use chrono::{DateTime, Utc};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::{
    ApiError,
    models::{AppState, ProxyRecord},
    tokens::{append_api_jwt, append_sandbox_jwt},
};

pub(crate) async fn build_config(
    state: &AppState,
    proxy: &ProxyRecord,
    principal_id: i64,
    now: i64,
) -> Result<Config, ApiError> {
    let subject = Subject {
        principal_id,
        principal: proxy.principal.as_ref(),
        console_user_id: proxy.console_user_id,
        console_user_email: proxy.console_user_email.as_deref(),
        slack_history_channel_ids: &proxy.slack_history_channel_ids,
        proxy_labels: &proxy.labels,
    };
    let mut config = centaur_console_db::build_config(
        &state.pool,
        SecretValues::Live(&state.encryption),
        &subject,
        proxy.requester_principal_id,
    )
    .await?;
    append_api_jwt(state, proxy, &mut config, now).await?;
    append_sandbox_jwt(state, proxy, &mut config, now)?;
    Ok(config)
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
