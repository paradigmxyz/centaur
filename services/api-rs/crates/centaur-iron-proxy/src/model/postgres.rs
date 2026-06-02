use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_yaml::Value;

use super::values::{
    listen_port, non_empty, resolve_placeholder_source_values, resolve_source_values,
};
use crate::{Result, SourcePolicy};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PostgresListener {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub listen: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream: Option<PostgresUpstream>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client: Option<PostgresClient>,
    #[serde(default, skip_serializing)]
    pub sandbox_env: Option<SandboxEnv>,
    #[serde(default, flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl PostgresListener {
    pub(crate) fn pg_dsn_env(&self) -> Option<PgDsnEnv> {
        let sandbox_env = self.sandbox_env.as_ref()?;
        let env_name = non_empty(sandbox_env.name.as_deref())?;
        let database = non_empty(sandbox_env.database.as_deref())?;
        let port = self.listen.as_deref().and_then(listen_port)?;
        let user = non_empty(
            self.client
                .as_ref()
                .and_then(|client| client.user.as_deref()),
        )?;
        let password_env = non_empty(
            self.client
                .as_ref()
                .and_then(|client| client.password_env.as_deref()),
        )?;
        Some(PgDsnEnv {
            env_name: env_name.to_owned(),
            database: database.to_owned(),
            port,
            user: user.to_owned(),
            password_env: password_env.to_owned(),
        })
    }

    pub(crate) fn resolve_sources(&mut self, source_policy: &SourcePolicy) -> Result<()> {
        if let Some(upstream) = &mut self.upstream {
            upstream.resolve_sources(source_policy)?;
        }
        if let Some(client) = &mut self.client {
            client.resolve_sources(source_policy)?;
        }
        resolve_source_values(self.extra.values_mut(), source_policy)
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PostgresUpstream {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dsn: Option<Value>,
    #[serde(default, flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl PostgresUpstream {
    fn resolve_sources(&mut self, source_policy: &SourcePolicy) -> Result<()> {
        if let Some(dsn) = &mut self.dsn {
            resolve_placeholder_source_values(dsn, source_policy)?;
        }
        resolve_source_values(self.extra.values_mut(), source_policy)
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PostgresClient {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password_env: Option<String>,
    #[serde(default, flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl PostgresClient {
    fn resolve_sources(&mut self, source_policy: &SourcePolicy) -> Result<()> {
        resolve_source_values(self.extra.values_mut(), source_policy)
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SandboxEnv {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub database: Option<String>,
    #[serde(default, flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PgDsnEnv {
    pub env_name: String,
    pub database: String,
    pub port: u16,
    pub user: String,
    pub password_env: String,
}

/// The iron-control `pg_dsn` secret foreign_id for a listener name. Shared so
/// the control-plane registration and the managed proxy's `IRON_PROXY_PG_*`
/// env derive the same key. foreign_id is restricted to `[A-Za-z0-9-._~]`.
pub fn pg_foreign_id(name: &str) -> String {
    let mut slug = String::new();
    let mut prev_dash = false;
    for ch in name.chars().flat_map(char::to_lowercase) {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch);
            prev_dash = false;
        } else if !prev_dash && !slug.is_empty() {
            slug.push('-');
            prev_dash = true;
        }
    }
    let slug = slug.trim_end_matches('-');
    format!("pg-{}", if slug.is_empty() { "pg" } else { slug })
}

/// The managed proxy reads each listener's local config from
/// `IRON_PROXY_PG_<FOREIGN_ID>_<SUFFIX>`, with the foreign_id normalized to
/// env-safe form: uppercase, with `- . ~` mapped to `_`.
pub fn pg_env_var(foreign_id: &str, suffix: &str) -> String {
    let normalized: String = foreign_id
        .chars()
        .map(|ch| match ch {
            '-' | '.' | '~' => '_',
            other => other.to_ascii_uppercase(),
        })
        .collect();
    format!("IRON_PROXY_PG_{normalized}_{suffix}")
}
