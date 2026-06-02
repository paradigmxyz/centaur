use serde_yaml::Value;

use crate::{IronProxyConfigError, Result, SourcePolicy};

pub(crate) fn listen_port(value: &str) -> Option<u16> {
    value.rsplit_once(':')?.1.parse().ok()
}

pub(crate) fn non_empty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

pub(crate) fn value_field_str<'a>(value: Option<&'a Value>, key: &str) -> Option<&'a str> {
    value?
        .as_mapping()?
        .get(Value::String(key.to_owned()))?
        .as_str()
}

pub(crate) fn resolve_source_values<'a>(
    values: impl IntoIterator<Item = &'a mut Value>,
    source_policy: &SourcePolicy,
) -> Result<()> {
    for value in values {
        resolve_placeholder_source_values(value, source_policy)?;
    }
    Ok(())
}

pub(crate) fn resolve_placeholder_source_values(
    value: &mut Value,
    source_policy: &SourcePolicy,
) -> Result<()> {
    if let Some(placeholder) = value_field_str(Some(value), "placeholder").map(ToOwned::to_owned) {
        let json_key = value_field_str(Some(value), "json_key").map(ToOwned::to_owned);
        *value = source_policy.source_for(&placeholder, json_key.as_deref())?;
        return Ok(());
    }

    match value {
        Value::Mapping(map) => {
            if map.get(string_value("type")).and_then(Value::as_str) == Some("token_broker")
                && !map.contains_key(string_value("ttl"))
            {
                map.insert(
                    string_value("ttl"),
                    string_value(&source_policy.token_broker_ttl),
                );
            }
            for child in map.values_mut() {
                resolve_placeholder_source_values(child, source_policy)?;
            }
        }
        Value::Sequence(values) => {
            for child in values {
                resolve_placeholder_source_values(child, source_policy)?;
            }
        }
        _ => {}
    }
    Ok(())
}

pub(crate) fn resolve_broker_store_source(
    value: &mut Value,
    source_policy: &SourcePolicy,
) -> Result<()> {
    if let Some(placeholder) = value_field_str(Some(value), "placeholder").map(ToOwned::to_owned) {
        if value_has_field(value, "json_key") {
            return Err(IronProxyConfigError::BrokerStoreJsonKey { placeholder });
        }
        *value = source_policy.store_source_for(&placeholder)?;
        return Ok(());
    }
    if value_field_str(Some(value), "type") == Some("env") {
        let placeholder = value_field_str(Some(value), "var")
            .unwrap_or("store")
            .to_owned();
        return Err(IronProxyConfigError::BrokerStoreEnv { placeholder });
    }
    Ok(())
}

fn value_has_field(value: &Value, key: &str) -> bool {
    value
        .as_mapping()
        .is_some_and(|map| map.contains_key(Value::String(key.to_owned())))
}

fn string_value(value: impl AsRef<str>) -> Value {
    Value::String(value.as_ref().to_owned())
}
