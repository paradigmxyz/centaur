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
    HarnessFragmentFile, default_harness_fragment_dirs, discover_fragment_files,
    discover_harness_fragment_files, harness_broker_fragments, harness_broker_fragments_from_dirs,
    harness_fragment_from_dirs, infra_fragment, load_default_proxy_base_config, load_fragment_file,
    load_fragment_files, load_fragment_str, placeholder_env,
};
pub use model::{
    BrokerCredential, PgDsnEnv, PostgresClient, PostgresListener, PostgresUpstream, ProxyFragment,
    SandboxEnv, Secret, SecretReplace, Transform, TransformConfig,
};
pub use ports::{ListenPorts, listen_ports_from_yaml, pg_dsn_envs};
pub use render::{render_proxy_yaml, render_proxy_yaml_with_source_policy};
pub use source::{SourceKind, SourcePolicy};

pub(crate) use model::{
    ProxyConfig, listen_port, resolve_placeholder_source_values, value_field_str,
};

#[cfg(test)]
mod tests;
