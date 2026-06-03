use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum IronProxyConfigError {
    #[error("failed to parse iron-proxy fragment {path}: {source}")]
    ParseFragment {
        path: PathBuf,
        source: serde_yaml::Error,
    },
    #[error("failed to serialize iron-proxy yaml: {0}")]
    Serialize(serde_yaml::Error),
    #[error(
        "iron-token-broker store cannot use env source for {placeholder}; configure FIREWALL_MANAGER_SECRET_SOURCE=onepassword or onepassword-connect"
    )]
    BrokerStoreEnv { placeholder: String },
    #[error(
        "iron-token-broker store placeholder {placeholder} cannot use json_key because the broker writes the whole credential blob"
    )]
    BrokerStoreJsonKey { placeholder: String },
}

pub type Result<T> = std::result::Result<T, IronProxyConfigError>;
