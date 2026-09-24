use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use crate::{
    ApiError,
    active_record_encryption::ActiveRecordEncryption,
    conflicts::suppress,
    database::load_credentials,
    identifiers::oid,
    models::{AppState, Config, Credential, ProxyRecord},
    tokens::{append_api_jwt, append_sandbox_jwt},
};

const EMPTY_ARRAY: Value = Value::Array(Vec::new());

pub(crate) async fn build_config(
    state: &AppState,
    proxy: &ProxyRecord,
    principal_id: i64,
) -> Result<Config, ApiError> {
    let loaded = load_credentials(&state.pool, principal_id, proxy.requester_principal_id).await?;
    let mut credentials = Vec::with_capacity(loaded.len());
    for credential in loaded {
        let deliverable = if credential.kind == "static" {
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
    for credential in credentials.iter().filter(|c| c.kind == "static") {
        if let Some(source) = source_value(&credential.sources[0], &state.encryption)? {
            let mut entry = Map::new();
            entry.insert("source".to_owned(), source);
            entry.insert("rules".to_owned(), proxy_rules(&credential.rules));
            if present(credential.data.get("inject_config")) {
                entry.insert(
                    "inject".to_owned(),
                    credential.data["inject_config"].clone(),
                );
            }
            if present(credential.data.get("replace_config")) {
                entry.insert(
                    "replace".to_owned(),
                    credential.data["replace_config"].clone(),
                );
            }
            config.secrets.push(Value::Object(entry));
        }
    }
    append_api_jwt(state, proxy, &mut config).await?;
    append_sandbox_jwt(state, proxy, &mut config)?;

    for kind in ["gcp_auth", "gcp_id_token", "aws_auth", "hmac"] {
        for credential in credentials.iter().filter(|c| c.kind == kind) {
            config
                .transforms
                .push(transform(credential, &state.encryption)?);
        }
    }
    let oauth: Vec<Value> = credentials
        .iter()
        .filter(|c| c.kind == "oauth_token")
        .map(|c| oauth_entry(c, &state.encryption))
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
    source: &Value,
    encryption: &ActiveRecordEncryption,
) -> Result<Option<Value>, ApiError> {
    let source_type = string(source, "source_type");
    if source_type == "token_broker" {
        let Some(raw) = source.get("broker_access_token").and_then(Value::as_str) else {
            return Ok(None);
        };
        let value = encryption.decrypt(raw)?;
        if value.is_empty() {
            return Ok(None);
        }
        return Ok(Some(json!({ "type": "control_plane", "value": value })));
    }
    let mut result = source
        .get("config")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    result.insert("type".to_owned(), Value::String(source_type.to_owned()));
    if source_type == "control_plane" {
        let raw = source
            .get("secret")
            .and_then(Value::as_str)
            .ok_or_else(|| ApiError::internal("control_plane source has no secret"))?;
        result.insert("value".to_owned(), Value::String(encryption.decrypt(raw)?));
    }
    Ok(Some(Value::Object(result)))
}

fn proxy_rules(rules: &[Value]) -> Value {
    Value::Array(
        rules
            .iter()
            .map(|rule| {
                let mut output = Map::new();
                for key in ["host", "cidr"] {
                    if let Some(value) = rule.get(key).filter(|v| present(Some(v))) {
                        output.insert(key.to_owned(), value.clone());
                    }
                }
                if let Some(value) = rule.get("http_methods").filter(|v| present(Some(v))) {
                    output.insert("methods".to_owned(), value.clone());
                }
                if let Some(value) = rule.get("paths").filter(|v| present(Some(v))) {
                    output.insert("paths".to_owned(), value.clone());
                }
                Value::Object(output)
            })
            .collect(),
    )
}

fn transform(c: &Credential, encryption: &ActiveRecordEncryption) -> Result<Value, ApiError> {
    let rules = proxy_rules(&c.rules);
    match c.kind.as_str() {
        "gcp_auth" => {
            let mut config = Map::new();
            if let Some(source) = c.sources.first()
                && let Some(value) = source_value(source, encryption)?
            {
                config.insert("keyfile".to_owned(), value);
            }
            copy_present(
                &c.data,
                &mut config,
                "credentials_provider",
                "credentials_provider",
            );
            copy_present(&c.data, &mut config, "subject", "subject");
            config.insert(
                "scopes".to_owned(),
                c.data
                    .get("scopes")
                    .cloned()
                    .unwrap_or_else(|| EMPTY_ARRAY.clone()),
            );
            config.insert("rules".to_owned(), rules);
            Ok(json!({ "name": "gcp_auth", "config": config }))
        }
        "gcp_id_token" => {
            let source = c
                .sources
                .first()
                .ok_or_else(|| ApiError::internal("gcp_id_token source missing"))?;
            let mut config = Map::new();
            config.insert(
                "keyfile".to_owned(),
                source_value(source, encryption)?
                    .ok_or_else(|| ApiError::internal("gcp_id_token source unavailable"))?,
            );
            config.insert("audience".to_owned(), c.data["audience"].clone());
            config.insert("rules".to_owned(), rules);
            copy_present(&c.data, &mut config, "header", "header");
            Ok(json!({ "name": "gcp_id_token", "config": config }))
        }
        "aws_auth" => {
            let mut config = Map::new();
            for source in &c.sources {
                if let Some(role) = source.get("role").and_then(Value::as_str)
                    && let Some(value) = source_value(source, encryption)?
                {
                    config.insert(role.to_owned(), value);
                }
            }
            copy_present(&c.data, &mut config, "allowed_regions", "allowed_regions");
            copy_present(&c.data, &mut config, "allowed_services", "allowed_services");
            config.insert("rules".to_owned(), rules);
            Ok(json!({ "name": "aws_auth", "config": config }))
        }
        "hmac" => {
            let mut credentials = Map::new();
            for source in &c.sources {
                if let Some(role) = source.get("role").and_then(Value::as_str)
                    && let Some(value) = source_value(source, encryption)?
                {
                    credentials.insert(role.to_owned(), value);
                }
            }
            let mut config = json!({
                "credentials": credentials,
                "timestamp": { "format": c.data["timestamp_format"] },
                "signature": {
                    "algorithm": c.data["signature_algorithm"],
                    "key_encoding": c.data["signature_key_encoding"],
                    "output_encoding": c.data["signature_output_encoding"],
                    "message": c.data["signature_message"]
                },
                "headers": c.data["headers"],
                "rules": rules
            });
            if c.data.get("allow_chunked_body").and_then(Value::as_bool) == Some(true) {
                config["allow_chunked_body"] = Value::Bool(true);
            }
            Ok(json!({ "name": "hmac_sign", "config": config }))
        }
        _ => Err(ApiError::internal("unknown transform kind")),
    }
}

fn oauth_entry(c: &Credential, encryption: &ActiveRecordEncryption) -> Result<Value, ApiError> {
    let mut entry = Map::new();
    entry.insert("grant".to_owned(), c.data["grant"].clone());
    entry.insert(
        "token_endpoint".to_owned(),
        c.data["token_endpoint"].clone(),
    );
    let mut headers = Map::new();
    for source in &c.sources {
        let Some(role) = source.get("role").and_then(Value::as_str) else {
            continue;
        };
        let Some(value) = source_value(source, encryption)? else {
            continue;
        };
        if string(source, "role_kind") == "endpoint_header" {
            headers.insert(role.to_owned(), value);
        } else {
            entry.insert(role.to_owned(), value);
        }
    }
    for key in ["audience", "scopes", "header", "value_prefix"] {
        copy_present(&c.data, &mut entry, key, key);
    }
    if !headers.is_empty() {
        entry.insert("token_endpoint_headers".to_owned(), Value::Object(headers));
    }
    entry.insert("rules".to_owned(), proxy_rules(&c.rules));
    Ok(Value::Object(entry))
}

fn append_postgres(
    proxy: &ProxyRecord,
    credentials: &[Credential],
    encryption: &ActiveRecordEncryption,
    config: &mut Config,
) -> Result<(), ApiError> {
    let mut positions: HashMap<String, usize> = HashMap::new();
    for c in credentials.iter().filter(|c| c.kind == "pg_dsn") {
        let Some(source) = c.sources.first() else {
            continue;
        };
        let Some(dsn) = source_value(source, encryption)? else {
            continue;
        };
        let database = string(&c.data, "database").to_owned();
        let mut entry = Map::new();
        entry.insert("id".to_owned(), Value::String(oid("pgs", c.id)));
        entry.insert("foreign_id".to_owned(), c.data["foreign_id"].clone());
        entry.insert("database".to_owned(), Value::String(database.clone()));
        entry.insert("dsn".to_owned(), dsn);
        copy_present(&c.data, &mut entry, "role", "role");
        let settings = postgres_settings(proxy, c.data.get("settings"));
        if !settings.is_empty() {
            entry.insert("settings".to_owned(), Value::Array(settings));
        }
        if let Some(index) = positions.get(&database).copied() {
            config.postgres[index] = Value::Object(entry);
        } else {
            positions.insert(database, config.postgres.len());
            config.postgres.push(Value::Object(entry));
        }
    }
    Ok(())
}

fn postgres_settings(proxy: &ProxyRecord, value: Option<&Value>) -> Vec<Value> {
    value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|setting| {
            let name = setting.get("name")?.as_str()?.trim();
            if name.is_empty() {
                return None;
            }
            let value = if let Some(reference) =
                setting.get("value_from").and_then(Value::as_object)
            {
                if let Some(label) = reference.get("principal_label").and_then(Value::as_str) {
                    proxy
                        .principal_field("labels")
                        .get(label)
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_owned()
                } else if let Some(label) = reference.get("proxy_label").and_then(Value::as_str) {
                    proxy
                        .labels
                        .get(label)
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_owned()
                } else {
                    reference
                        .get("principal_field")
                        .and_then(Value::as_str)
                        .map(|field| principal_setting(proxy, field))
                        .unwrap_or_default()
                }
            } else {
                setting
                    .get("value")
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

fn string<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}

fn value_to_string(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(value) => value.clone(),
        other => other.to_string(),
    }
}

fn present(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::String(value)) => !value.is_empty(),
        Some(Value::Array(value)) => !value.is_empty(),
        Some(Value::Object(value)) => !value.is_empty(),
        Some(_) => true,
    }
}

fn copy_present(source: &Value, target: &mut Map<String, Value>, from: &str, to: &str) {
    if let Some(value) = source.get(from).filter(|value| present(Some(value))) {
        target.insert(to.to_owned(), value.clone());
    }
}

fn timestamp(value: Option<DateTime<Utc>>) -> Value {
    value
        .map(|value| Value::String(value.format("%Y-%m-%dT%H:%M:%SZ").to_string()))
        .unwrap_or(Value::Null)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::proxy_rules;

    #[test]
    fn rules_match_the_proxy_shape() {
        let rules = vec![
            json!({"host":"api.example.com","cidr":null,"http_methods":["POST"],"paths":["/v1/*"],"position":0}),
        ];
        assert_eq!(
            proxy_rules(&rules),
            json!([{"host":"api.example.com","methods":["POST"],"paths":["/v1/*"]}])
        );
    }
}
