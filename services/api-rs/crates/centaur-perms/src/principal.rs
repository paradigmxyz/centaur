//! Resolve the CLI `--principal` argument into an iron-control identity.

use std::collections::BTreeMap;

use centaur_iron_control::{PrincipalDerivationError, PrincipalInput, derive_principal};

/// Turn a `--principal` value (plus optional `--slack-user`) into the identity
/// to upsert/look up.
///
/// A value containing `:` is treated as a chat thread key and run through the
/// canonical [`derive_principal`], so the resulting `foreign_id` matches exactly
/// what api-rs writes at session start. Any other value is used verbatim as a
/// principal `foreign_id` (e.g. `slack-channel-t1-c9`), so an operator can name
/// an already-registered principal directly.
pub fn resolve_principal(
    principal: &str,
    slack_user: Option<&str>,
) -> Result<PrincipalInput, PrincipalDerivationError> {
    if principal.contains(':') {
        // The CLI has no resolved conversation name; the synthetic display name
        // is fine for operator-driven lookups.
        Ok(derive_principal(principal, slack_user, None)?.to_principal_input())
    } else {
        Ok(PrincipalInput {
            foreign_id: principal.to_owned(),
            name: principal.to_owned(),
            labels: BTreeMap::from([("managed-by".to_owned(), "centaur".to_owned())]),
            kind: None,
            slack_user_id: None,
            slack_channel_id: None,
            slack_team_id: None,
            slack_email: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thread_key_is_derived() {
        let id = resolve_principal("slack:T123:C456:1780000000.0001", Some("U1")).unwrap();
        assert_eq!(id.foreign_id, "slack-channel-t123-c456");
    }

    #[test]
    fn raw_foreign_id_is_verbatim() {
        let id = resolve_principal("External-System-AbC123", None).unwrap();
        assert_eq!(id.foreign_id, "External-System-AbC123");
        assert_eq!(id.name, "External-System-AbC123");
    }
}
