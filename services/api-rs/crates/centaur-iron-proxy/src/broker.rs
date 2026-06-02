use std::collections::BTreeMap;

use serde::Serialize;

use crate::{BrokerCredential, IronProxyConfigError, ProxyFragment, Result, SourcePolicy};

pub const DEFAULT_BROKER_LISTEN_PORT: u16 = 8181;
pub const DEFAULT_BROKER_METRICS_PORT: u16 = 9091;
pub const BROKER_BEARER_AUTH_ENV: &str = "IRON_BROKER_TOKEN";

pub fn render_token_broker_yaml(fragments: &[ProxyFragment]) -> Result<String> {
    render_token_broker_yaml_with_source_policy(fragments, &SourcePolicy::default())
}

pub fn render_token_broker_yaml_with_source_policy(
    fragments: &[ProxyFragment],
    source_policy: &SourcePolicy,
) -> Result<String> {
    let mut credentials = BTreeMap::<String, BrokerCredential>::new();
    for credential in fragments
        .iter()
        .flat_map(|fragment| fragment.broker_credentials.iter())
    {
        if credentials.contains_key(&credential.id) {
            continue;
        }
        let mut credential = credential.clone();
        credential.resolve_sources(source_policy)?;
        credentials.insert(credential.id.clone(), credential);
    }
    serde_yaml::to_string(&TokenBrokerConfig::new(credentials.into_values().collect()))
        .map_err(IronProxyConfigError::Serialize)
}

#[derive(Serialize)]
struct TokenBrokerConfig {
    listen: String,
    metrics_listen: String,
    bearer_auth_env: &'static str,
    log: TokenBrokerLogConfig,
    credentials: Vec<BrokerCredential>,
}

impl TokenBrokerConfig {
    fn new(credentials: Vec<BrokerCredential>) -> Self {
        Self {
            listen: format!(":{DEFAULT_BROKER_LISTEN_PORT}"),
            metrics_listen: format!(":{DEFAULT_BROKER_METRICS_PORT}"),
            bearer_auth_env: BROKER_BEARER_AUTH_ENV,
            log: TokenBrokerLogConfig {
                level: "info",
                format: "json",
            },
            credentials,
        }
    }
}

#[derive(Serialize)]
struct TokenBrokerLogConfig {
    level: &'static str,
    format: &'static str,
}
