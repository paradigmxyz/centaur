use std::collections::BTreeMap;

use crate::listen_port;
use crate::{IronProxyConfigError, PgDsnEnv, ProxyConfig, ProxyFragment, Result};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ListenPorts {
    pub proxy: u16,
    pub all: Vec<u16>,
}

pub fn listen_ports_from_yaml(config_yaml: &str) -> Result<ListenPorts> {
    let cfg: ProxyConfig =
        serde_yaml::from_str(config_yaml).map_err(IronProxyConfigError::ParseBase)?;
    let proxy = cfg
        .proxy
        .as_ref()
        .and_then(|proxy| proxy.tunnel_listen.as_deref())
        .and_then(listen_port)
        .unwrap_or(8080);
    let mut all = vec![proxy];
    all.extend(
        cfg.postgres
            .iter()
            .filter_map(|listener| listener.listen.as_deref().and_then(listen_port)),
    );
    all.sort_unstable();
    all.dedup();
    Ok(ListenPorts { proxy, all })
}

pub fn pg_dsn_envs(fragments: &[ProxyFragment]) -> Vec<PgDsnEnv> {
    let mut entries = BTreeMap::<String, PgDsnEnv>::new();
    for entry in fragments
        .iter()
        .flat_map(|fragment| fragment.postgres.iter())
        .filter_map(|listener| listener.pg_dsn_env())
    {
        entries.entry(entry.env_name.clone()).or_insert(entry);
    }
    entries.into_values().collect()
}
