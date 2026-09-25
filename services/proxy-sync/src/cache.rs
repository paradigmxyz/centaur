use std::{
    collections::HashMap,
    sync::RwLock,
    time::{Duration, Instant},
};

use sha2::{Digest, Sha256};

use crate::{models::ProxyRecord, tokens::TokenWindows};

const ENTRY_TTL: Duration = Duration::from_secs(10 * 60);

fn generation_fingerprint(proxy: &ProxyRecord, token_windows: TokenWindows) -> Option<[u8; 32]> {
    // Structured serialization preserves field boundaries. serde_json's default
    // map representation sorts object keys independently of insertion order.
    let bytes = serde_json::to_vec(&(
        &proxy.name,
        &proxy.labels,
        proxy.principal_id,
        proxy.principal_assigned_at,
        proxy.principal_cache_version,
        &proxy.principal,
        &proxy.console_user_email,
        proxy.console_user_id,
        &proxy.slack_history_channel_ids,
        token_windows.api,
        token_windows.sandbox,
    ))
    .ok()?;
    Some(Sha256::digest(bytes).into())
}

struct Entry {
    generation: [u8; 32],
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
        let generation = generation_fingerprint(proxy, token_windows)?;
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
        let Some(generation) = generation_fingerprint(proxy, token_windows) else {
            return;
        };
        let mut entries = self
            .entries
            .write()
            .unwrap_or_else(|lock| lock.into_inner());
        entries.retain(|_, entry| entry.created_at.elapsed() < ENTRY_TTL);
        entries.insert(
            proxy.id,
            Entry {
                generation,
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
    fn proxy_specific_changes_invalidate_the_hash() {
        let cache = SyncCache::default();
        cache.store(&proxy(), windows(), "sha256:current".to_owned());

        let changes: &[fn(&mut ProxyRecord)] = &[
            |p| p.name.push_str("-renamed"),
            |p| p.labels["team"] = json!("other"),
            |p| p.principal_id = Some(22),
            |p| p.principal_assigned_at = None,
            |p| p.principal.as_mut().unwrap()["labels"] = json!({"tenant": "other"}),
            |p| p.console_user_email = Some("other@example.com".to_owned()),
            |p| p.console_user_id = Some(23),
            |p| p.slack_history_channel_ids = json!([]),
        ];
        for change in changes {
            let mut changed = proxy();
            change(&mut changed);
            assert_eq!(
                cache.matching_hash(&changed, windows(), "sha256:current"),
                None
            );
        }

        let mut reordered = proxy();
        reordered.principal = Some(json!({"labels": {}, "id": 11}));
        assert_eq!(
            cache.matching_hash(&reordered, windows(), "sha256:current"),
            Some("sha256:current".to_owned())
        );
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
        let next_window = TokenWindows {
            sandbox: windows.sandbox.map(|window| window + 86_400),
            ..windows
        };
        assert_eq!(
            cache.matching_hash(&proxy, next_window, "sha256:current"),
            None
        );
    }
}
