//! Disposable identities for paired SSH proxies.
use centaur_sandbox_core::{SandboxError, SandboxResult};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use ssh_key::{
    LineEnding, PrivateKey,
    private::{Ed25519Keypair, KeypairData},
};

/// Shortest accepted identity seed, in bytes.
const MIN_SEED_BYTES: usize = 32;

/// Deployment SSH settings for paired proxies. Present only when SSH is on.
#[derive(Clone)]
pub struct SshProxyConfig {
    seed: String,
    workspace: String,
}

impl SshProxyConfig {
    /// `seed` derives every sandbox's disposable keys, so it must stay stable
    /// for retained sandboxes; `workspace` is where remote commands run.
    pub fn new(seed: impl Into<String>, workspace: impl Into<String>) -> Result<Self, String> {
        let seed = seed.into();
        if seed.len() < MIN_SEED_BYTES {
            return Err(format!(
                "SSH seed must contain at least {MIN_SEED_BYTES} bytes"
            ));
        }
        Ok(Self {
            seed,
            workspace: workspace.into(),
        })
    }
}

impl std::fmt::Debug for SshProxyConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SshProxyConfig")
            .field("seed", &"[redacted]")
            .field("workspace", &self.workspace)
            .finish()
    }
}

#[derive(Clone, Eq, PartialEq)]
pub(crate) struct Identity {
    client_private: String,
    client_public: String,
    host_private: String,
    host_public: String,
    workspace: String,
}

impl std::fmt::Debug for Identity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Identity([redacted])")
    }
}

impl Identity {
    pub(crate) fn derive(config: &SshProxyConfig, scope: &str) -> SandboxResult<Self> {
        Self::from_seed(&config.seed, scope, &config.workspace)
    }
    fn from_seed(seed: &str, scope: &str, workspace: &str) -> SandboxResult<Self> {
        if seed.len() < MIN_SEED_BYTES {
            return Err(SandboxError::InvalidSpec(format!(
                "SSH seed must contain at least {MIN_SEED_BYTES} bytes"
            )));
        }
        let pair = |purpose: &str| -> SandboxResult<(String, String)> {
            let mut hash = Sha256::new();
            // Changing the tag changes every derived identity; keep it stable
            // once sandboxes exist.
            for value in ["centaur-ssh-v1", seed, scope, purpose] {
                hash.update((value.len() as u64).to_be_bytes());
                hash.update(value.as_bytes());
            }
            let bytes: [u8; 32] = hash.finalize().into();
            let key = PrivateKey::new(KeypairData::Ed25519(Ed25519Keypair::from_seed(&bytes)), "")
                .map_err(|_| SandboxError::InvalidSpec("SSH key generation failed".into()))?;
            let private = key
                .to_openssh(LineEnding::LF)
                .map_err(|_| SandboxError::InvalidSpec("SSH key encoding failed".into()))?
                .to_string();
            let public = key
                .public_key()
                .to_openssh()
                .map_err(|_| SandboxError::InvalidSpec("SSH public key encoding failed".into()))?;
            Ok((private, public))
        };
        let (client_private, client_public) = pair("client")?;
        let (host_private, host_public) = pair("host")?;
        Ok(Self {
            client_private,
            client_public,
            host_private,
            host_public,
            workspace: workspace.into(),
        })
    }
    #[cfg(test)]
    pub(crate) fn fixture(scope: &str) -> Self {
        Self::from_seed("test-only-seed-with-at-least-32-bytes", scope, "/workspace").unwrap()
    }
    pub(crate) fn sandbox(&self, host: &str) -> Value {
        json!({"host":host,"port":2222,"private_key":self.client_private,"host_key":self.host_public,"workspace":self.workspace})
    }
    /// The listener's `IRON_SSH_CONFIG_JSON`. Upstream dials honor the proxy's
    /// own `IRON_PROXY_UPSTREAM_DENY_CIDRS`, so no deny list is repeated here.
    pub(crate) fn proxy(&self, control_url: &str) -> Value {
        json!({"listen":"0.0.0.0:2222","host_key":{"type":"control_plane","value":self.host_private},"client_public_key":self.client_public,"authorize_url":format!("{}/api/v1/proxy/ssh/authorize",control_url.trim_end_matches('/')),"token_env":"IRON_PROXY_TOKEN"})
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn identities_are_stable_and_isolated() {
        let seed = "test-only-seed-with-at-least-32-bytes";
        let a = Identity::from_seed(seed, "sandbox-a", "/workspace").unwrap();
        let again = Identity::from_seed(seed, "sandbox-a", "/workspace").unwrap();
        let b = Identity::from_seed(seed, "sandbox-b", "/workspace").unwrap();
        assert_eq!(a, again);
        assert_ne!(a.client_public, b.client_public);
        assert_ne!(a.client_public, a.host_public);
        let client = a.sandbox("proxy").to_string();
        assert!(!client.contains(&a.host_private));
        assert!(!client.contains(seed));
        assert!(!format!("{a:?}").contains(&a.client_private));
        assert!(Identity::from_seed("short", "a", "/").is_err());
        assert!(SshProxyConfig::new("short", "/workspace").is_err());
        let config = SshProxyConfig::new(seed, "/workspace").unwrap();
        assert_eq!(Identity::derive(&config, "sandbox-a").unwrap(), a);
        assert!(!format!("{config:?}").contains(seed));
    }
}
