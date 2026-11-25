use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_yaml::Value;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Transform {
    pub name: String,
    #[serde(default, skip_serializing_if = "TransformConfig::is_empty")]
    pub config: TransformConfig,
    #[serde(default, flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl Transform {
    pub(crate) fn is_secrets(&self) -> bool {
        self.name == "secrets"
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct TransformConfig {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub secrets: Vec<Secret>,
    #[serde(default, flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl TransformConfig {
    fn is_empty(&self) -> bool {
        self.secrets.is_empty() && self.extra.is_empty()
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Secret {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replace: Option<SecretReplace>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inject: Option<Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rules: Vec<Value>,
    #[serde(default, flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl Secret {
    pub(crate) fn proxy_value(&self) -> Option<&str> {
        self.replace
            .as_ref()
            .and_then(|replace| replace.proxy_value.as_deref())
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SecretReplace {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy_value: Option<String>,
    #[serde(default, flatten)]
    pub extra: BTreeMap<String, Value>,
}
