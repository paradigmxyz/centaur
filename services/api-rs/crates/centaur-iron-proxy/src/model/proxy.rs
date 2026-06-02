use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_yaml::Value;

use super::{BrokerCredential, PostgresListener, Transform};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ProxyFragment {
    #[serde(default)]
    pub transforms: Vec<Transform>,
    #[serde(default)]
    pub postgres: Vec<PostgresListener>,
    #[serde(default)]
    pub broker_credentials: Vec<BrokerCredential>,
    #[serde(default, flatten)]
    pub top_level: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(crate) struct ProxyConfig {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) transforms: Vec<Transform>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) postgres: Vec<PostgresListener>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) proxy: Option<ProxySection>,
    #[serde(default, flatten)]
    pub(crate) top_level: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(crate) struct ProxySection {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) tunnel_listen: Option<String>,
    #[serde(default, flatten)]
    pub(crate) extra: BTreeMap<String, Value>,
}
