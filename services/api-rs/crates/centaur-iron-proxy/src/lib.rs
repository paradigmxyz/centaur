mod error;
mod fragment;
mod model;
mod source;

pub use error::{IronProxyConfigError, Result};
pub use fragment::{
    bedrock_enabled, bedrock_region, bedrock_sandbox_env, harness_auth_fragment, infra_fragment,
    load_fragment_str, pg_sandbox_dsns, placeholder_env,
};
pub use model::{
    PgDsnSetting, PgDsnSettingValueFrom, PostgresClient, PostgresListener, PostgresUpstream,
    ProxyFragment, SandboxEnv, Secret, SecretReplace, Transform, TransformConfig, pg_foreign_id,
    pg_sandbox_env_var,
};
pub use source::{SourceKind, SourcePolicy};

#[cfg(test)]
mod tests;
