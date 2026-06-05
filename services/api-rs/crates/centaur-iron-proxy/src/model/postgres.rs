use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_yaml::Value;

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

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PostgresUpstream {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dsn: Option<Value>,
    #[serde(default, flatten)]
    pub extra: BTreeMap<String, Value>,
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

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SandboxEnv {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub database: Option<String>,
    #[serde(default, flatten)]
    pub extra: BTreeMap<String, Value>,
}

/// The iron-control `pg_dsn` secret foreign_id for a listener name. Shared so
/// the control-plane registration and the sandbox DSN env var derive the same
/// key. foreign_id is restricted to `[A-Za-z0-9-._~]`.
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

/// Normalize a foreign_id to env-safe form: uppercase, with `- . ~` → `_`.
fn normalize_foreign_id(foreign_id: &str) -> String {
    foreign_id
        .chars()
        .map(|ch| match ch {
            '-' | '.' | '~' => '_',
            other => other.to_ascii_uppercase(),
        })
        .collect()
}

/// The sandbox env var that receives a listener's proxied DSN:
/// `<NORMALIZED_FOREIGN_ID>_DSN` (e.g. `pg-analytics` → `PG_ANALYTICS_DSN`).
pub fn pg_sandbox_env_var(foreign_id: &str) -> String {
    format!("{}_DSN", normalize_foreign_id(foreign_id))
}
