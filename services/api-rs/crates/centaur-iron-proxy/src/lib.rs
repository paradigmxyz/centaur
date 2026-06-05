mod error;
mod fragment;
mod model;
mod source;

pub use error::{IronProxyConfigError, Result};
pub use fragment::{harness_auth_fragment, infra_fragment, load_fragment_str, placeholder_env};
pub use model::{
    PostgresClient, PostgresListener, PostgresUpstream, ProxyFragment, SandboxEnv, Secret,
    SecretReplace, Transform, TransformConfig, pg_env_var, pg_foreign_id, pg_sandbox_env_var,
};
pub use source::{SourceKind, SourcePolicy};

#[cfg(test)]
mod tests;
