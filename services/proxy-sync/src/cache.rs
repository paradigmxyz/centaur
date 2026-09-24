use std::{
    collections::HashMap,
    sync::RwLock,
    time::{Duration, Instant},
};

use crate::{models::ProxyRecord, tokens::TokenWindows};

const ENTRY_TTL: Duration = Duration::from_secs(10 * 60);

#[derive(Clone, PartialEq)]
struct Generation {
    proxy_name: String,
    proxy_labels: serde_json::Value,
    principal_id: Option<i64>,
    principal_assigned_at: Option<chrono::DateTime<chrono::Utc>>,
    principal_cache_version: Option<i64>,
    principal: Option<serde_json::Value>,
    console_user_email: Option<String>,
    console_user_id: Option<i64>,
    slack_history_channel_ids: serde_json::Value,
    token_windows: TokenWindows,
}

impl Generation {
    fn new(proxy: &ProxyRecord, token_windows: TokenWindows) -> Self {
        Self {
            proxy_name: proxy.name.clone(),
            proxy_labels: proxy.labels.clone(),
            principal_id: proxy.principal_id,
            principal_assigned_at: proxy.principal_assigned_at,
            principal_cache_version: proxy.principal_cache_version,
            principal: proxy.principal.clone(),
            console_user_email: proxy.console_user_email.clone(),
            console_user_id: proxy.console_user_id,
            slack_history_channel_ids: proxy.slack_history_channel_ids.clone(),
            token_windows,
        }
    }
}

struct Entry {
    generation: Generation,
    config_hash: String,
    created_at: Instant,
}

#[derive(Default)]
pub(crate) struct SyncCache {
    entries: RwLock<HashMap<i64, Entry>>,
}

impl SyncCache {
    pub(crate) fn matching_hash(
        &self,
        proxy: &ProxyRecord,
        token_windows: TokenWindows,
        client_hash: &str,
    ) -> Option<String> {
        // Requester unions depend on OAuth-app eligibility that is intentionally
        // assembled live and is not covered by the principal cache version.
        if proxy.requester_principal_id.is_some() {
            return None;
        }
        let generation = Generation::new(proxy, token_windows);
        let entries = self.entries.read().unwrap_or_else(|lock| lock.into_inner());
        let entry = entries.get(&proxy.id)?;
        (entry.created_at.elapsed() < ENTRY_TTL
            && entry.generation == generation
            && entry.config_hash == client_hash)
            .then(|| entry.config_hash.clone())
    }

    pub(crate) fn store(
        &self,
        proxy: &ProxyRecord,
        token_windows: TokenWindows,
        config_hash: String,
    ) {
        if proxy.requester_principal_id.is_some() {
            return;
        }
        let mut entries = self
            .entries
            .write()
            .unwrap_or_else(|lock| lock.into_inner());
        entries.retain(|_, entry| entry.created_at.elapsed() < ENTRY_TTL);
        entries.insert(
            proxy.id,
            Entry {
                generation: Generation::new(proxy, token_windows),
                config_hash,
                created_at: Instant::now(),
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};
    use serde_json::json;

    use super::SyncCache;
    use crate::{models::ProxyRecord, tokens::TokenWindows};

    fn proxy() -> ProxyRecord {
        ProxyRecord {
            id: 7,
            name: "sandbox-7".to_owned(),
            labels: json!({"team": "infra"}),
            principal_id: Some(11),
            requester_principal_id: None,
            principal_assigned_at: Some(Utc.timestamp_opt(1_700_000_000, 0).unwrap()),
            requester_principal_assigned_at: None,
            principal_cache_version: Some(3),
            principal: Some(json!({"id": 11, "labels": {}})),
            console_user_email: Some("user@example.com".to_owned()),
            console_user_id: Some(13),
            slack_history_channel_ids: json!(["C12345678"]),
        }
    }

    fn windows() -> TokenWindows {
        TokenWindows {
            api: Some(1_700_000_000),
            sandbox: Some(1_699_920_000),
        }
    }

    #[test]
    fn matches_only_a_server_hash_for_the_same_generation() {
        let cache = SyncCache::default();
        let mut proxy = proxy();
        let windows = windows();

        assert_eq!(cache.matching_hash(&proxy, windows, "sha256:current"), None);
        cache.store(&proxy, windows, "sha256:current".to_owned());

        assert_eq!(
            cache.matching_hash(&proxy, windows, "sha256:current"),
            Some("sha256:current".to_owned())
        );
        assert_eq!(cache.matching_hash(&proxy, windows, "sha256:stale"), None);

        proxy.principal_cache_version = Some(4);
        assert_eq!(cache.matching_hash(&proxy, windows, "sha256:current"), None);
    }

    #[test]
    fn requester_unions_bypass_the_cache() {
        let cache = SyncCache::default();
        let mut proxy = proxy();
        proxy.requester_principal_id = Some(12);
        cache.store(&proxy, windows(), "sha256:current".to_owned());

        assert_eq!(
            cache.matching_hash(&proxy, windows(), "sha256:current"),
            None
        );
    }

    #[test]
    fn jwt_window_changes_invalidate_the_hash() {
        let cache = SyncCache::default();
        let proxy = proxy();
        let windows = windows();
        cache.store(&proxy, windows, "sha256:current".to_owned());

        let next_window = TokenWindows {
            api: windows.api.map(|window| window + 900),
            ..windows
        };
        assert_eq!(
            cache.matching_hash(&proxy, next_window, "sha256:current"),
            None
        );
    }
}
