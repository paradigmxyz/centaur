//! Helpers for hand-written `Debug` impls on types that hold secrets
//! (digests, ciphertexts, tokens, passwords, session data). See the
//! "Secrets in `Debug`" rule in `centaur-console-api/CONVENTIONS.md`.
//!
//! ```
//! use std::fmt;
//! use centaur_console_db::redact::Redacted;
//!
//! struct Key { name: String, token_hash: String }
//!
//! impl fmt::Debug for Key {
//!     fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
//!         f.debug_struct("Key")
//!             .field("name", &self.name)
//!             .field("token_hash", &Redacted)
//!             .finish()
//!     }
//! }
//!
//! let key = Key { name: "ci".into(), token_hash: "sentinel".into() };
//! assert_eq!(format!("{key:?}"), r#"Key { name: "ci", token_hash: [REDACTED] }"#);
//! ```

use std::fmt;

/// Formats as `[REDACTED]`.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Redacted;

impl fmt::Debug for Redacted {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("[REDACTED]")
    }
}

/// An optional secret: `None`, or `Some([REDACTED])`.
pub fn option<T>(value: &Option<T>) -> Option<Redacted> {
    value.as_ref().map(|_| Redacted)
}

/// Formats the keys of a map whose values are secret, as
/// `{"a": [REDACTED], ...}`.
pub struct Keys<I>(pub I);

impl<I, K> fmt::Debug for Keys<I>
where
    I: Clone + IntoIterator<Item = K>,
    K: fmt::Debug,
{
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_map()
            .entries(self.0.clone().into_iter().map(|key| (key, Redacted)))
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_without_values() {
        assert_eq!(format!("{:?}", Redacted), "[REDACTED]");
        assert_eq!(
            format!("{:?}", option(&Some("sentinel"))),
            "Some([REDACTED])"
        );
        assert_eq!(format!("{:?}", option::<&str>(&None)), "None");
        let map = indexmap::IndexMap::from([("a", "sentinel"), ("b", "sentinel")]);
        assert_eq!(
            format!("{:?}", Keys(map.keys())),
            r#"{"a": [REDACTED], "b": [REDACTED]}"#
        );
    }

    const SENTINEL: &str = "SENTINEL-SECRET-7f3a";

    #[track_caller]
    fn assert_redacted(value: &impl fmt::Debug) {
        for rendered in [format!("{value:?}"), format!("{value:#?}")] {
            assert!(!rendered.contains(SENTINEL), "{rendered}");
        }
    }

    #[test]
    fn read_side_types_redact_secrets() {
        let source: crate::models::SecretSource = serde_json::from_value(serde_json::json!({
            "source_type": "env",
            "config": {"value": SENTINEL},
            "secret": SENTINEL,
            "broker_access_token": SENTINEL,
            "role": null,
            "role_kind": null,
        }))
        .unwrap();
        let config = crate::models::Config {
            secrets: vec![serde_json::json!({"value": SENTINEL})],
            ..Default::default()
        };
        let client = crate::ApiClient {
            id: 1,
            name: "runner".into(),
            secret_digest: SENTINEL.into(),
            scopes: vec![],
            disabled: false,
        };
        let key = crate::SshKey {
            id: 1,
            priority: 0,
            version: "v".into(),
            private_key: serde_json::json!({"value": SENTINEL}),
        };
        assert_redacted(&(Some(vec![source]), config, client, key));
    }
}
