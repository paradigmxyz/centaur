use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_yaml::Value;

use super::values::{non_empty, resolve_placeholder_source_values, resolve_source_values};
use crate::{Result, SourcePolicy};

const MANAGED_TRANSFORMS: &[&str] = &["secrets", "gcp_auth", "oauth_token", "hmac_sign"];

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Transform {
    pub name: String,
    #[serde(default, skip_serializing_if = "TransformConfig::is_empty")]
    pub config: TransformConfig,
    #[serde(default, flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl Transform {
    pub(crate) fn is_managed(&self) -> bool {
        MANAGED_TRANSFORMS.contains(&self.name.as_str())
    }

    pub(crate) fn is_secrets(&self) -> bool {
        self.name == "secrets"
    }

    pub(crate) fn resolve_sources(&mut self, source_policy: &SourcePolicy) -> Result<()> {
        if self.is_secrets() {
            for secret in &mut self.config.secrets {
                secret.fill_missing_source(source_policy)?;
            }
        }
        self.config.resolve_sources(source_policy)?;
        resolve_source_values(self.extra.values_mut(), source_policy)
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

    fn resolve_sources(&mut self, source_policy: &SourcePolicy) -> Result<()> {
        for secret in &mut self.secrets {
            secret.resolve_sources(source_policy)?;
        }
        resolve_source_values(self.extra.values_mut(), source_policy)
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
    pub(crate) fn explicit_id(&self) -> Option<&str> {
        non_empty(self.id.as_deref())
    }

    pub(crate) fn proxy_value(&self) -> Option<&str> {
        self.replace
            .as_ref()
            .and_then(|replace| replace.proxy_value.as_deref())
    }

    fn fill_missing_source(&mut self, source_policy: &SourcePolicy) -> Result<()> {
        if self.source.is_some() {
            return Ok(());
        }
        if let Some(proxy_value) = self.proxy_value() {
            self.source = Some(source_policy.source_for(proxy_value, None)?);
        }
        Ok(())
    }

    fn resolve_sources(&mut self, source_policy: &SourcePolicy) -> Result<()> {
        if let Some(source) = &mut self.source {
            resolve_placeholder_source_values(source, source_policy)?;
        }
        if let Some(replace) = &mut self.replace {
            replace.resolve_sources(source_policy)?;
        }
        if let Some(inject) = &mut self.inject {
            resolve_placeholder_source_values(inject, source_policy)?;
        }
        resolve_source_values(&mut self.rules, source_policy)?;
        resolve_source_values(self.extra.values_mut(), source_policy)
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SecretReplace {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy_value: Option<String>,
    #[serde(default, flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl SecretReplace {
    fn resolve_sources(&mut self, source_policy: &SourcePolicy) -> Result<()> {
        resolve_source_values(self.extra.values_mut(), source_policy)
    }
}
