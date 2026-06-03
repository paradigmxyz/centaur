mod broker;
mod error;
mod fragment;
mod model;
mod ports;
mod render;
mod source;

pub use broker::{
    BROKER_BEARER_AUTH_ENV, DEFAULT_BROKER_LISTEN_PORT, DEFAULT_BROKER_METRICS_PORT,
    render_token_broker_yaml, render_token_broker_yaml_with_source_policy,
};
pub use error::{IronProxyConfigError, Result};
pub use fragment::{
    harness_auth_fragment, infra_fragment, load_default_proxy_base_config, load_fragment_file,
    load_fragment_str, placeholder_env,
};
pub use model::{
    BrokerCredential, PgDsnEnv, PostgresClient, PostgresListener, PostgresUpstream, ProxyFragment,
    SandboxEnv, Secret, SecretReplace, Transform, TransformConfig, pg_env_var, pg_foreign_id, pg_sandbox_env_var,
};
pub use ports::{ListenPorts, listen_ports_from_yaml, pg_dsn_envs};
pub use render::{render_proxy_yaml, render_proxy_yaml_with_source_policy};
pub use source::{SourceKind, SourcePolicy};

pub(crate) use model::{
    ProxyConfig, listen_port, resolve_placeholder_source_values, value_field_str,
};

#[cfg(test)]
mod tests;
