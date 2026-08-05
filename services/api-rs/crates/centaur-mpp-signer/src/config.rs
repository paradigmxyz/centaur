use std::{net::SocketAddr, str::FromStr as _};

use clap::Parser;
use mpp::PrivateKeySigner;

use crate::model::PolicyRule;

const DEFAULT_RPC_URL: &str = "https://rpc.tempo.xyz";
const DEFAULT_CHAIN_ID: u64 = 4217;
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

    #[arg(long, env = "MPP_CHAIN_ID", default_value_t = DEFAULT_CHAIN_ID)]
    pub chain_id: u64,

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

    #[arg(long, env = "MPP_BUDGET_CURRENCY")]
    pub budget_currency: Option<String>,

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
        anyhow::ensure!(self.chain_id > 0, "MPP chain ID must be positive");
        for (name, value) in [
            ("MPP_MAX_PER_CHARGE_ATOMIC", self.max_per_charge_atomic),
            ("MPP_MAX_DAILY_ATOMIC", self.max_daily_atomic),
        ] {
            anyhow::ensure!(
                value.is_none_or(|amount| amount >= 0),
                "{name} cannot be negative"
            );
        }
        let budgets_configured =
            self.max_per_charge_atomic.is_some() || self.max_daily_atomic.is_some();
        anyhow::ensure!(
            !budgets_configured
                || self
                    .budget_currency
                    .as_deref()
                    .is_some_and(|currency| !currency.trim().is_empty()),
            "MPP_BUDGET_CURRENCY is required when a software budget is configured"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser as _;

    use super::Args;

    fn args(extra: &[&str]) -> Args {
        let mut values = vec![
            "centaur-mpp-signer",
            "--database-url",
            "postgres://example.invalid/test",
            "--private-key",
            "unused-in-validation",
            "--signer-token",
            "test-signer-token-that-is-at-least-32-bytes",
        ];
        values.extend_from_slice(extra);
        Args::try_parse_from(values).expect("arguments")
    }

    #[test]
    fn configured_budgets_require_one_currency() {
        let error = args(&["--max-daily-atomic", "100"])
            .validate()
            .expect_err("currency must be required");
        assert!(error.to_string().contains("MPP_BUDGET_CURRENCY"));

        args(&[
            "--max-per-charge-atomic",
            "10",
            "--budget-currency",
            "0xtest",
        ])
        .validate()
        .expect("single-currency budget");
    }

    #[test]
    fn missing_budget_values_keep_software_caps_disabled() {
        args(&[]).validate().expect("budgets are optional");
    }
}
