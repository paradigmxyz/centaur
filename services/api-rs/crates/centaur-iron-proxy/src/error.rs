use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum IronProxyConfigError {
    #[error("failed to parse iron-proxy fragment {path}: {source}")]
    ParseFragment {
        path: PathBuf,
        source: serde_yaml::Error,
    },
    #[error("failed to parse CODEX_CUSTOM_PROVIDERS: {source}")]
    ParseCustomProviders { source: serde_json::Error },
    #[error("invalid custom Codex provider {provider:?}: {reason}")]
    InvalidCustomProvider {
        provider: String,
        reason: &'static str,
    },
}

pub type Result<T> = std::result::Result<T, IronProxyConfigError>;
