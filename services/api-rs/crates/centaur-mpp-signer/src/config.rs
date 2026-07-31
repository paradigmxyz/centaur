use std::{net::SocketAddr, str::FromStr as _};

use clap::Parser;
use mpp::PrivateKeySigner;

use crate::model::PolicyRule;

const DEFAULT_RPC_URL: &str = "https://rpc.tempo.xyz";
const DEFAULT_REGISTRY_URL: &str = "https://mpp.dev/api/services";

#[derive(Parser)]
pub struct Args {
    #[arg(long, env = "MPP_SIGNER_BIND_ADDR", default_value = "0.0.0.0:8090")]
    pub bind_addr: SocketAddr,

    #[arg(long, env = "DATABASE_URL")]
    pub database_url: String,

    #[arg(long, env = "MPP_PRIVATE_KEY")]
    private_key: String,

    #[arg(long, env = "MPP_SIGNER_TOKEN")]
    pub signer_token: String,

    #[arg(long, env = "MPP_RPC_URL", default_value = DEFAULT_RPC_URL)]
    pub rpc_url: String,

    #[arg(
        long,
        env = "MPP_REGISTRY_URL",
        default_value = DEFAULT_REGISTRY_URL
    )]
    pub registry_url: String,

    #[arg(long, env = "MPP_REGISTRY_CACHE_TTL_SECONDS", default_value_t = 900)]
    pub registry_cache_ttl_seconds: i64,

    #[arg(long, env = "MPP_REGISTRY_MAX_STALE_SECONDS", default_value_t = 86_400)]
    pub registry_max_stale_seconds: i64,

    #[arg(
        long,
        env = "MPP_DEFAULT_METHODS",
        value_delimiter = ',',
        default_value = "GET"
    )]
    pub default_methods: Vec<String>,

    #[arg(long, env = "MPP_POLICY_RULES", default_value = "[]")]
    policy_rules: String,

    #[arg(long, env = "MPP_MAX_PER_CHARGE_ATOMIC")]
    pub max_per_charge_atomic: Option<i64>,

    #[arg(long, env = "MPP_MAX_DAILY_ATOMIC")]
    pub max_daily_atomic: Option<i64>,

    #[arg(long, env = "MPP_RUN_MIGRATIONS", default_value_t = true)]
    pub run_migrations: bool,
}

impl Args {
    pub fn signer(&self) -> anyhow::Result<PrivateKeySigner> {
        anyhow::ensure!(
            !self.private_key.trim().is_empty(),
            "MPP private key is empty"
        );
        PrivateKeySigner::from_str(self.private_key.trim())
            .map_err(|error| anyhow::anyhow!("invalid MPP private key: {error}"))
    }

    pub fn policy_rules(&self) -> anyhow::Result<Vec<PolicyRule>> {
        serde_json::from_str(&self.policy_rules)
            .map_err(|error| anyhow::anyhow!("invalid MPP policy rules: {error}"))
    }

    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.signer_token.len() >= 32,
            "MPP signer token must be at least 32 bytes"
        );
        anyhow::ensure!(
            self.registry_cache_ttl_seconds > 0
                && self.registry_max_stale_seconds >= self.registry_cache_ttl_seconds,
            "MPP registry cache durations are invalid"
        );
        for (name, value) in [
            ("MPP_MAX_PER_CHARGE_ATOMIC", self.max_per_charge_atomic),
            ("MPP_MAX_DAILY_ATOMIC", self.max_daily_atomic),
        ] {
            anyhow::ensure!(
                value.is_none_or(|amount| amount >= 0),
                "{name} cannot be negative"
            );
        }
        Ok(())
    }
}
