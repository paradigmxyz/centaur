//! Resolve the CLI `--principal` argument into an iron-control identity.

use std::collections::BTreeMap;

use centaur_iron_control::{IdentityInput, derive_principal};

/// Turn a `--principal` value (plus optional `--slack-user`) into the identity
/// to upsert/look up.
///
/// A value containing `:` is treated as a Slack thread key and run through the
/// canonical [`derive_principal`], so the resulting `foreign_id` matches exactly
/// what api-rs writes at session start. Any other value is used verbatim as a
/// principal `foreign_id` (e.g. `slack-channel-t1-c9`), so an operator can name
/// an already-registered principal directly.
pub fn resolve_principal(principal: &str, slack_user: Option<&str>, namespace: &str) -> IdentityInput {
    if principal.contains(':') {
        derive_principal(principal, slack_user).to_identity_input(namespace)
    } else {
        IdentityInput {
            namespace: namespace.to_owned(),
            foreign_id: principal.to_owned(),
            name: principal.to_owned(),
            labels: BTreeMap::from([("managed-by".to_owned(), "centaur".to_owned())]),
        }
    }
}

/// Resolve a `--principal` selector to the value to *look up* (read-only paths).
///
/// A value containing `:` is a Slack thread key and is derived to its canonical
/// `foreign_id`; anything else — a `foreign_id` or a `prn_` OID — is returned
/// verbatim, both of which `GET /principals/:id` accepts.
pub fn resolve_lookup(principal: &str, slack_user: Option<&str>) -> String {
    if principal.contains(':') {
        derive_principal(principal, slack_user).foreign_id
    } else {
        principal.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_derives_thread_key_but_passes_oid_verbatim() {
        assert_eq!(resolve_lookup("slack:T1:C9:ts", None), "slack-channel-t1-c9");
        assert_eq!(resolve_lookup("prn_abc123", None), "prn_abc123");
        assert_eq!(resolve_lookup("slack-channel-t1-c9", None), "slack-channel-t1-c9");
    }

    #[test]
    fn thread_key_is_derived() {
        let id = resolve_principal("slack:T123:C456:1780000000.0001", Some("U1"), "default");
        assert_eq!(id.foreign_id, "slack-channel-t123-c456");
    }

    #[test]
    fn dm_thread_key_keys_on_user() {
        let id = resolve_principal("slack:D9:ts", Some("U07ABC"), "default");
        assert_eq!(id.foreign_id, "slack-user-u07abc");
    }

    #[test]
    fn raw_foreign_id_is_verbatim() {
        let id = resolve_principal("slack-channel-t1-c9", None, "default");
        assert_eq!(id.foreign_id, "slack-channel-t1-c9");
        assert_eq!(id.name, "slack-channel-t1-c9");
    }
}
