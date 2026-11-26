use std::{collections::BTreeMap, fmt, str::FromStr};

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

/// The hand sizes the chart maps to pod resources.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HandSize {
    Default,
    Large,
}

impl HandSize {
    pub const ALL: [Self; 2] = [Self::Default, Self::Large];

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Large => "large",
        }
    }
}

impl fmt::Display for HandSize {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for HandSize {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|size| size.as_str() == value)
            .ok_or_else(|| format!("unknown hand size {value:?}; expected default or large"))
    }
}

/// One `hand_boxes` row.
#[derive(Clone, Debug, sqlx::FromRow)]
pub struct HandBox {
    pub id: String,
    pub size: String,
    pub state: String,
    pub pod_name: String,
    pub pod_ip: Option<String>,
    pub iron_proxy_id: Option<String>,
    pub lease_id: Option<String>,
    pub thread_key: Option<String>,
    pub principal: Option<String>,
    pub requester: Option<String>,
    pub leased_at: Option<OffsetDateTime>,
    pub lease_expires_at: Option<OffsetDateTime>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

impl HandBox {
    #[must_use]
    pub fn size(&self) -> HandSize {
        self.size.parse().unwrap_or(HandSize::Default)
    }

    #[must_use]
    pub fn is_ready(&self) -> bool {
        self.state == "ready"
    }

    #[must_use]
    pub fn is_warming(&self) -> bool {
        self.state == "warming"
    }

    #[must_use]
    pub fn is_leased(&self) -> bool {
        self.state == "leased"
    }

    #[must_use]
    pub fn is_live_lease(&self, now: OffsetDateTime) -> bool {
        self.is_leased()
            && self
                .lease_expires_at
                .is_some_and(|expires_at| expires_at > now)
    }
}

/// What a caller asks for when it checks out a hand. Identity always comes
/// from the turn token, never from the request body.
#[derive(Clone, Copy, Debug)]
pub struct LeaseRequest<'a> {
    pub thread_key: &'a str,
    pub principal: &'a str,
    pub requester: Option<&'a str>,
    pub size: Option<HandSize>,
}

/// The transition that turns a ready or freshly-created hand into a lease.
#[derive(Clone, Debug)]
pub struct LeaseClaim {
    pub lease_id: String,
    pub thread_key: String,
    pub principal: String,
    pub requester: Option<String>,
    pub expires_at: OffsetDateTime,
}

/// The lease API response.
#[derive(Clone, Serialize)]
pub struct HandLease {
    pub lease_id: String,
    pub hand_id: String,
    pub size: HandSize,
    pub url: String,
    pub token: String,
    pub env: BTreeMap<String, String>,
    #[serde(with = "time::serde::rfc3339")]
    pub expires_at: OffsetDateTime,
}

impl fmt::Debug for HandLease {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The token is the lease credential and env values are credentials'
        // placeholders. Log only names, never either value.
        formatter
            .debug_struct("HandLease")
            .field("lease_id", &self.lease_id)
            .field("hand_id", &self.hand_id)
            .field("size", &self.size)
            .field("url", &self.url)
            .field("env_keys", &self.env.keys().collect::<Vec<_>>())
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

/// A new hand id (`hnd_...`). DNS-label safe once embedded in a pod name.
#[must_use]
pub fn new_hand_id() -> String {
    let uuid = Uuid::new_v4().simple().to_string();
    format!("hnd_{}", &uuid[..12])
}

/// A new lease id (`hls_<uuid simple>`).
#[must_use]
pub fn new_lease_id() -> String {
    format!("hls_{}", Uuid::new_v4().simple())
}

/// The pod name for a hand. Hand ids use an underscore, which is legal in a
/// label value but not in a DNS label, so the pod name replaces it.
#[must_use]
pub fn pod_name_for_hand(id: &str) -> String {
    format!("hand-{}", id.replace('_', "-"))
}

/// The per-hand Secret name. Owned by the pod after creation so a pod delete
/// also removes the token and proxy material.
#[must_use]
pub fn secret_name_for_hand(id: &str) -> String {
    format!("{}-secret", pod_name_for_hand(id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lease_debug_omits_the_token_and_env_values() {
        let lease = HandLease {
            lease_id: "hls_test".to_owned(),
            hand_id: "hnd_test".to_owned(),
            size: HandSize::Default,
            url: "ws://127.0.0.1:7070".to_owned(),
            token: "hand-secret-token".to_owned(),
            env: BTreeMap::from([(
                "GITHUB_TOKEN".to_owned(),
                "placeholder-secret-value".to_owned(),
            )]),
            expires_at: OffsetDateTime::now_utc(),
        };
        let debug = format!("{lease:?}");
        assert!(debug.contains("hls_test"));
        assert!(debug.contains("GITHUB_TOKEN"));
        assert!(!debug.contains("hand-secret-token"));
        assert!(!debug.contains("placeholder-secret-value"));
    }
}
