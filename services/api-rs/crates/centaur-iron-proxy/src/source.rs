use serde::Serialize;
use serde_yaml::Value;
use strum::EnumString;

use crate::{IronProxyConfigError, Result};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourcePolicy {
    pub kind: SourceKind,
    pub op_vault: String,
    pub ttl: String,
    pub token_broker_ttl: String,
}

impl SourcePolicy {
    pub fn env() -> Self {
        Self::new(SourceKind::Env, "ai-agents", "10m")
    }

    pub fn onepassword(op_vault: impl Into<String>, ttl: impl Into<String>) -> Self {
        Self::new(SourceKind::OnePassword, op_vault, ttl)
    }

    pub fn onepassword_connect(op_vault: impl Into<String>, ttl: impl Into<String>) -> Self {
        Self::new(SourceKind::OnePasswordConnect, op_vault, ttl)
    }

    fn new(kind: SourceKind, op_vault: impl Into<String>, ttl: impl Into<String>) -> Self {
        Self {
            kind,
            op_vault: op_vault.into(),
            ttl: ttl.into(),
            token_broker_ttl: "1m".to_owned(),
        }
    }

    pub(crate) fn source_for(&self, placeholder: &str, json_key: Option<&str>) -> Result<Value> {
        let json_key = json_key.map(ToOwned::to_owned);
        let source = match self.kind {
            SourceKind::Env => SecretSource::Env {
                var: placeholder.to_owned(),
                json_key,
            },
            SourceKind::OnePassword => SecretSource::OnePassword {
                secret_ref: self.secret_ref(placeholder),
                ttl: self.ttl.clone(),
                json_key,
            },
            SourceKind::OnePasswordConnect => SecretSource::OnePasswordConnect {
                secret_ref: self.secret_ref(placeholder),
                ttl: self.ttl.clone(),
                json_key,
            },
        };
        serde_yaml::to_value(source).map_err(IronProxyConfigError::Serialize)
    }

    pub(crate) fn store_source_for(&self, placeholder: &str) -> Result<Value> {
        match self.kind {
            SourceKind::Env => Err(IronProxyConfigError::BrokerStoreEnv {
                placeholder: placeholder.to_owned(),
            }),
            SourceKind::OnePassword | SourceKind::OnePasswordConnect => {
                self.source_for(placeholder, None)
            }
        }
    }

    fn secret_ref(&self, placeholder: &str) -> String {
        format!("op://{}/{placeholder}/credential", self.op_vault)
    }
}

impl Default for SourcePolicy {
    fn default() -> Self {
        Self::env()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, EnumString)]
pub enum SourceKind {
    #[strum(serialize = "env")]
    Env,
    #[strum(serialize = "onepassword")]
    OnePassword,
    #[strum(serialize = "onepassword-connect")]
    OnePasswordConnect,
}

#[derive(Serialize)]
#[serde(tag = "type")]
enum SecretSource {
    #[serde(rename = "env")]
    Env {
        var: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        json_key: Option<String>,
    },
    #[serde(rename = "1password")]
    OnePassword {
        secret_ref: String,
        ttl: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        json_key: Option<String>,
    },
    #[serde(rename = "1password_connect")]
    OnePasswordConnect {
        secret_ref: String,
        ttl: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        json_key: Option<String>,
    },
}
