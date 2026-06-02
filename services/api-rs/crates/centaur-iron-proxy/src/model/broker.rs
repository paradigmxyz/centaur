use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_yaml::Value;

use super::values::{
    resolve_broker_store_source, resolve_placeholder_source_values, resolve_source_values,
};
use crate::{Result, SourcePolicy};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BrokerCredential {
    pub id: String,
    pub token_endpoint: String,
    pub client_id: Value,
    pub store: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_secret: Option<Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scopes: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub token_endpoint_headers: BTreeMap<String, Value>,
    #[serde(default, flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl BrokerCredential {
    pub(crate) fn resolve_sources(&mut self, source_policy: &SourcePolicy) -> Result<()> {
        resolve_placeholder_source_values(&mut self.client_id, source_policy)?;
        resolve_broker_store_source(&mut self.store, source_policy)?;
        if let Some(client_secret) = &mut self.client_secret {
            resolve_placeholder_source_values(client_secret, source_policy)?;
        }
        resolve_source_values(self.token_endpoint_headers.values_mut(), source_policy)?;
        resolve_source_values(self.extra.values_mut(), source_policy)
    }
}
