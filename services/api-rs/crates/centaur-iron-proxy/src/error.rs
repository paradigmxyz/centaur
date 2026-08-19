use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum IronProxyConfigError {
    #[error("failed to parse iron-proxy fragment {path}: {source}")]
    ParseFragment {
        path: PathBuf,
        source: serde_yaml::Error,
    },
    #[error("invalid OPENAI_BASE_URL {value:?}: {reason}")]
    InvalidOpenAiBaseUrl { value: String, reason: String },
}

pub type Result<T> = std::result::Result<T, IronProxyConfigError>;
