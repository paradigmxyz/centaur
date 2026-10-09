use std::collections::HashMap;

use active_record_encryption::ActiveRecordEncryption;
use chrono::{DateTime, Utc};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use crate::{
    ApiError,
    conflicts::suppress,
    database::load_credentials,
    identifiers::oid,
    models::{
        AppState, Config, Credential, CredentialData, CredentialKind, PostgresSetting, ProxyRecord,
        RequestRule, SecretSource,
    },
    tokens::{append_api_jwt, append_sandbox_jwt},
};

pub(crate) async fn build_config(
    state: &AppState,
    proxy: &ProxyRecord,
    principal_id: i64,
    now: i64,
) -> Result<Config, ApiError> {
    let loaded = load_credentials(&state.pool, principal_id, proxy.requester_principal_id).await?;
    let mut credentials = Vec::with_capacity(loaded.len());
    for credential in loaded {
        let deliverable = if credential.kind == CredentialKind::Static {
            match credential.sources.first() {
                Some(source) => source_value(source, &state.encryption)?.is_some(),
                None => false,
            }
        } else {
            true
        };
        if deliverable {
            credentials.push(credential);
        }
    }
    suppress(&mut credentials);

    let mut config = Config::default();
    for credential in credentials
        .iter()
        .filter(|credential| credential.kind == CredentialKind::Static)
    {
        let CredentialData::Static(data) = &credential.data else {
            return Err(mismatched_data(credential));
        };
        if let Some(source) = source_value(&credential.sources[0], &state.encryption)? {
            let mut entry = Map::new();
            entry.insert("source".to_owned(), source);
            entry.insert("rules".to_owned(), proxy_rules(&credential.rules));
            insert_present(&mut entry, "inject", data.inject_config.as_ref());
            insert_present(&mut entry, "replace", data.replace_config.as_ref());
            config.secrets.push(Value::Object(entry));
        }
    }
    append_api_jwt(state, proxy, &mut config, now).await?;
    append_sandbox_jwt(state, proxy, &mut config, now)?;

    for kind in [
        CredentialKind::GcpAuth,
        CredentialKind::GcpIdToken,
        CredentialKind::AwsAuth,
        CredentialKind::Hmac,
    ] {
        for credential in credentials
            .iter()
            .filter(|credential| credential.kind == kind)
        {
            config
                .transforms
                .push(transform(credential, &state.encryption)?);
        }
    }
    let oauth: Vec<Value> = credentials
        .iter()
        .filter(|credential| credential.kind == CredentialKind::OauthToken)
        .map(|credential| oauth_entry(credential, &state.encryption))
        .collect::<Result<_, _>>()?;
    if !oauth.is_empty() {
        config
            .transforms
            .push(json!({ "name": "oauth_token", "config": { "tokens": oauth } }));
    }
    append_postgres(proxy, &credentials, &state.encryption, &mut config)?;
    Ok(config)
}

fn source_value(
    source: &SecretSource,
    encryption: &ActiveRecordEncryption,
) -> Result<Option<Value>, ApiError> {
    if source.source_type == "token_broker" {
        let Some(raw) = source.broker_access_token.as_deref() else {
            return Ok(None);
        };
        let value = encryption.decrypt(raw)?;
        if value.is_empty() {
            return Ok(None);
        }
        return Ok(Some(json!({ "type": "control_plane", "value": value })));
    }

    let mut result = source.config.clone();
    result.insert("type".to_owned(), Value::String(source.source_type.clone()));
    if source.source_type == "control_plane" {
        let raw = source
            .secret
            .as_deref()
            .ok_or_else(|| ApiError::internal("control_plane source has no secret"))?;
        result.insert("value".to_owned(), Value::String(encryption.decrypt(raw)?));
    }
    Ok(Some(Value::Object(result)))
}

fn proxy_rules(rules: &[RequestRule]) -> Value {
    Value::Array(
        rules
            .iter()
            .map(|rule| {
                let mut output = Map::new();
                insert_nonempty_string(&mut output, "host", rule.host.as_deref());
                insert_nonempty_string(&mut output, "cidr", rule.cidr.as_deref());
                insert_nonempty_vec(&mut output, "methods", &rule.http_methods);
                insert_nonempty_vec(&mut output, "paths", &rule.paths);
                Value::Object(output)
            })
            .collect(),
    )
}

fn transform(
    credential: &Credential,
    encryption: &ActiveRecordEncryption,
) -> Result<Value, ApiError> {
    let rules = proxy_rules(&credential.rules);
    match &credential.data {
        CredentialData::GcpAuth(data) => {
            let mut config = Map::new();
            if let Some(source) = credential.sources.first()
                && let Some(value) = source_value(source, encryption)?
            {
                config.insert("keyfile".to_owned(), value);
            }
            insert_present(
                &mut config,
                "credentials_provider",
                data.credentials_provider.as_ref(),
            );
            insert_nonempty_string(&mut config, "subject", data.subject.as_deref());
            config.insert("scopes".to_owned(), json!(data.scopes));
            config.insert("rules".to_owned(), rules);
            Ok(json!({ "name": "gcp_auth", "config": config }))
        }
        CredentialData::GcpIdToken(data) => {
            let source = credential
                .sources
                .first()
                .ok_or_else(|| ApiError::internal("gcp_id_token source missing"))?;
            let mut config = Map::new();
            config.insert(
                "keyfile".to_owned(),
                source_value(source, encryption)?
                    .ok_or_else(|| ApiError::internal("gcp_id_token source unavailable"))?,
            );
            config.insert("audience".to_owned(), Value::String(data.audience.clone()));
            config.insert("rules".to_owned(), rules);
            insert_nonempty_string(&mut config, "header", data.header.as_deref());
            Ok(json!({ "name": "gcp_id_token", "config": config }))
        }
        CredentialData::AwsAuth(data) => {
            let mut config = Map::new();
            for source in &credential.sources {
                if let Some(role) = source.role.as_deref()
                    && let Some(value) = source_value(source, encryption)?
                {
                    config.insert(role.to_owned(), value);
                }
            }
            insert_nonempty_vec(&mut config, "allowed_regions", &data.allowed_regions);
            insert_nonempty_vec(&mut config, "allowed_services", &data.allowed_services);
            config.insert("rules".to_owned(), rules);
            Ok(json!({ "name": "aws_auth", "config": config }))
        }
        CredentialData::Hmac(data) => {
            let mut credentials = Map::new();
            for source in &credential.sources {
                if let Some(role) = source.role.as_deref()
                    && let Some(value) = source_value(source, encryption)?
                {
                    credentials.insert(role.to_owned(), value);
                }
            }
            let mut config = json!({
                "credentials": credentials,
                "timestamp": { "format": data.timestamp_format },
                "signature": {
                    "algorithm": data.signature_algorithm,
                    "key_encoding": data.signature_key_encoding,
                    "output_encoding": data.signature_output_encoding,
                    "message": data.signature_message
                },
                "headers": data.headers,
                "rules": rules
            });
            if data.allow_chunked_body {
                config["allow_chunked_body"] = Value::Bool(true);
            }
            Ok(json!({ "name": "hmac_sign", "config": config }))
        }
        _ => Err(mismatched_data(credential)),
    }
}

fn oauth_entry(
    credential: &Credential,
    encryption: &ActiveRecordEncryption,
) -> Result<Value, ApiError> {
    let CredentialData::OauthToken(data) = &credential.data else {
        return Err(mismatched_data(credential));
    };
    let mut entry = Map::new();
    entry.insert("grant".to_owned(), Value::String(data.grant.clone()));
    entry.insert(
        "token_endpoint".to_owned(),
        Value::String(data.token_endpoint.clone()),
    );
    let mut headers = Map::new();
    for source in &credential.sources {
        let Some(role) = source.role.as_deref() else {
            continue;
        };
        let Some(value) = source_value(source, encryption)? else {
            continue;
        };
        if source.role_kind.as_deref() == Some("endpoint_header") {
            headers.insert(role.to_owned(), value);
        } else {
            entry.insert(role.to_owned(), value);
        }
    }
    insert_nonempty_string(&mut entry, "audience", data.audience.as_deref());
    insert_nonempty_vec(&mut entry, "scopes", &data.scopes);
    insert_nonempty_string(&mut entry, "header", data.header.as_deref());
    insert_nonempty_string(&mut entry, "value_prefix", data.value_prefix.as_deref());
    if !headers.is_empty() {
        entry.insert("token_endpoint_headers".to_owned(), Value::Object(headers));
    }
    entry.insert("rules".to_owned(), proxy_rules(&credential.rules));
    Ok(Value::Object(entry))
}

fn append_postgres(
    proxy: &ProxyRecord,
    credentials: &[Credential],
    encryption: &ActiveRecordEncryption,
    config: &mut Config,
) -> Result<(), ApiError> {
    let mut positions: HashMap<String, usize> = HashMap::new();
    for credential in credentials
        .iter()
        .filter(|credential| credential.kind == CredentialKind::PgDsn)
    {
        let CredentialData::PgDsn(data) = &credential.data else {
            return Err(mismatched_data(credential));
        };
        let Some(source) = credential.sources.first() else {
            continue;
        };
        let Some(dsn) = source_value(source, encryption)? else {
            continue;
        };
        let mut entry = Map::new();
        entry.insert("id".to_owned(), Value::String(oid("pgs", credential.id)));
        entry.insert(
            "foreign_id".to_owned(),
            Value::String(data.foreign_id.clone()),
        );
        entry.insert("database".to_owned(), Value::String(data.database.clone()));
        entry.insert("dsn".to_owned(), dsn);
        insert_nonempty_string(&mut entry, "role", data.role.as_deref());
        let settings = postgres_settings(proxy, &data.settings);
        if !settings.is_empty() {
            entry.insert("settings".to_owned(), Value::Array(settings));
        }
        if let Some(index) = positions.get(&data.database).copied() {
            config.postgres[index] = Value::Object(entry);
        } else {
            positions.insert(data.database.clone(), config.postgres.len());
            config.postgres.push(Value::Object(entry));
        }
    }
    Ok(())
}

fn postgres_settings(proxy: &ProxyRecord, settings: &[PostgresSetting]) -> Vec<Value> {
    settings
        .iter()
        .filter_map(|setting| {
            let name = setting.name.trim();
            if name.is_empty() {
                return None;
            }
            let value = if let Some(reference) = &setting.value_from {
                if let Some(label) = nonempty(reference.principal_label.as_deref()) {
                    proxy
                        .principal_field("labels")
                        .get(label)
                        .map(value_to_string)
                        .unwrap_or_default()
                } else if let Some(label) = nonempty(reference.proxy_label.as_deref()) {
                    proxy
                        .labels
                        .get(label)
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_owned()
                } else {
                    reference
                        .principal_field
                        .as_deref()
                        .map(|field| principal_setting(proxy, field))
                        .unwrap_or_default()
                }
            } else {
                setting
                    .value
                    .as_ref()
                    .map(value_to_string)
                    .unwrap_or_default()
            };
            Some(json!({ "name": name, "value": value }))
        })
        .collect()
}

fn principal_setting(proxy: &ProxyRecord, field: &str) -> String {
    match field {
        "id" => proxy
            .principal_id
            .map(|id| oid("prn", id))
            .unwrap_or_default(),
        "console_user_id" => proxy
            .console_user_id
            .map(|id| oid("usr", id))
            .unwrap_or_default(),
        "console_user_email" => proxy.console_user_email.clone().unwrap_or_default(),
        "slack_history_channel_ids" => proxy.slack_history_channel_ids.to_string(),
        other => value_to_string(proxy.principal_field(other)),
    }
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

fn insert_present(target: &mut Map<String, Value>, key: &str, value: Option<&Value>) {
    if let Some(value) = value
        && present(value)
    {
        target.insert(key.to_owned(), value.clone());
    }
}

fn insert_nonempty_string(target: &mut Map<String, Value>, key: &str, value: Option<&str>) {
    if let Some(value) = nonempty(value) {
        target.insert(key.to_owned(), Value::String(value.to_owned()));
    }
}

fn insert_nonempty_vec<T: serde::Serialize>(
    target: &mut Map<String, Value>,
    key: &str,
    value: &[T],
) {
    if !value.is_empty() {
        target.insert(key.to_owned(), json!(value));
    }
}

fn nonempty(value: Option<&str>) -> Option<&str> {
    value.filter(|value| !value.is_empty())
}

fn present(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::String(value) => !value.is_empty(),
        Value::Array(value) => !value.is_empty(),
        Value::Object(value) => !value.is_empty(),
        _ => true,
    }
}

fn value_to_string(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(value) => value.clone(),
        other => other.to_string(),
    }
}

fn mismatched_data(credential: &Credential) -> ApiError {
    ApiError::internal(format!(
        "credential {} data does not match {:?}",
        credential.id, credential.kind
    ))
}

fn timestamp(value: Option<DateTime<Utc>>) -> Value {
    value
        .map(|value| Value::String(value.format("%Y-%m-%dT%H:%M:%SZ").to_string()))
        .unwrap_or(Value::Null)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{postgres_settings, proxy_rules};
    use crate::models::{PostgresSetting, ProxyRecord, RequestRule};

    #[test]
    fn postgres_principal_labels_stringify_scalars_and_missing_values() {
        let proxy = ProxyRecord {
            id: 1,
            name: "test".into(),
            labels: json!({}),
            principal_id: Some(1),
            requester_principal_id: None,
            principal_assigned_at: None,
            requester_principal_assigned_at: None,
            principal_cache_version: Some(0),
            principal: Some(json!({"labels": {"tenant": 123, "enabled": false, "empty": null}})),
            console_user_email: None,
            console_user_id: None,
            slack_history_channel_ids: json!([]),
        };
        let settings: Vec<PostgresSetting> = serde_json::from_value(json!([
            {"name": "app.tenant", "value_from": {"principal_label": "tenant"}},
            {"name": "app.enabled", "value_from": {"principal_label": "enabled"}},
            {"name": "app.empty", "value_from": {"principal_label": "empty"}},
            {"name": "app.missing", "value_from": {"principal_label": "missing"}}
        ]))
        .unwrap();
        assert_eq!(
            postgres_settings(&proxy, &settings),
            vec![
                json!({"name": "app.tenant", "value": "123"}),
                json!({"name": "app.enabled", "value": "false"}),
                json!({"name": "app.empty", "value": ""}),
                json!({"name": "app.missing", "value": ""}),
            ]
        );
    }

    #[test]
    fn rules_match_the_proxy_shape() {
        let rules = vec![RequestRule {
            host: Some("api.example.com".to_owned()),
            cidr: None,
            http_methods: vec!["POST".to_owned()],
            paths: vec!["/v1/*".to_owned()],
        }];
        assert_eq!(
            proxy_rules(&rules),
            json!([{"host":"api.example.com","methods":["POST"],"paths":["/v1/*"]}])
        );
    }
}
