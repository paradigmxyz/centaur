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
