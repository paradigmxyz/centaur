//! Derive the iron-control principal a session's proxy should act as.
//!
//! A principal is the identity that holds roles and owns proxies. For Centaur
//! the principal is the Slack conversation: a **user** for a 1:1 DM, or a
//! **channel** for a multi-party channel/group thread. The thread key is
//! ``<source>:[<team_id>:]<conversation_id>[:<thread_ts>]`` — segments are
//! identified by their Slack prefix rather than position, because the optional
//! team id shifts everything after it (``T`` = team, ``C``/``G`` = channel,
//! ``D`` = DM; a ``thread_ts`` is numeric). When a team id is present it is
//! folded into the principal key so the same channel/user id in two workspaces
//! never collides onto one principal.
//!
//! [`derive_principal`] is pure so the mapping is unit-tested directly; callers
//! upsert the returned [`PrincipalRef`] at session start.

use std::collections::BTreeMap;

use crate::models::IdentityInput;
use crate::util::{managed_labels, slugify};

/// The principal a session resolves to, as a stable upsert key plus a label.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrincipalRef {
    pub foreign_id: String,
    pub name: String,
    pub labels: BTreeMap<String, String>,
}

impl PrincipalRef {
    /// Build the upsert body for this principal in ``namespace``, tagging it as
    /// Centaur-managed.
    pub fn to_identity_input(&self, namespace: &str) -> IdentityInput {
        let mut labels = managed_labels();
        labels.extend(self.labels.clone());
        IdentityInput {
            namespace: namespace.to_owned(),
            foreign_id: self.foreign_id.clone(),
            name: self.name.clone(),
            labels,
        }
    }
}

/// Resolve the principal for a thread.
///
/// ``slack_user_id`` is the acting user, when known (carried in session
/// metadata). It is only used to key a DM principal; channel threads key on the
/// channel so everyone in the channel shares one principal. When the thread key
/// is not a recognizable Slack conversation, the whole key is slugged so every
/// thread still maps to a deterministic, distinct principal.
///
/// ``conversation_name`` is the human-readable channel name (or DM partner's
/// display name) the slackbot resolves and carries in session metadata. When
/// present and non-empty it is formatted into the principal's display ``name``
/// (``Slack DM @<name>`` for a DM, ``Slack Channel #<name>`` for a channel);
/// otherwise we fall back to a synthetic name built from the ids. The name is
/// cosmetic — ``foreign_id`` (the upsert key) is always derived from ids, so the
/// same conversation maps to one stable principal regardless of any later
/// rename.
pub fn derive_principal(
    thread_key: &str,
    slack_user_id: Option<&str>,
    conversation_name: Option<&str>,
) -> PrincipalRef {
    let (team_id, conversation_id) = parse_slack_segments(thread_key);
    let mut labels = BTreeMap::new();
    if let Some(team) = team_id {
        labels.insert("slack_team_id".to_owned(), team.to_owned());
    }
    let scope = team_id
        .map(|team| format!("{}-", slugify(team)))
        .unwrap_or_default();
    let team_suffix = team_id
        .map(|team| format!(" (team {team})"))
        .unwrap_or_default();
    let display_name = conversation_name
        .map(str::trim)
        .filter(|name| !name.is_empty());

    if is_direct_message(conversation_id)
        && let Some(user) = slack_user_id.map(str::trim).filter(|user| !user.is_empty())
    {
        labels.insert("slack_user_id".to_owned(), user.to_owned());
        return PrincipalRef {
            foreign_id: format!("slack-user-{scope}{}", slugify(user)),
            name: display_name
                .map(|name| format!("Slack DM @{name}"))
                .unwrap_or_else(|| format!("Slack User {user}{team_suffix}")),
            labels,
        };
    }

    if let Some(conversation_id) = conversation_id {
        labels.insert("slack_channel_id".to_owned(), conversation_id.to_owned());
        return PrincipalRef {
            foreign_id: format!("slack-channel-{scope}{}", slugify(conversation_id)),
            name: display_name
                .map(|name| format!("Slack Channel #{name}"))
                .unwrap_or_else(|| format!("Slack Channel {conversation_id}{team_suffix}")),
            labels,
        };
    }

    PrincipalRef {
        foreign_id: format!("thread-{}", slugify(thread_key)),
        name: display_name
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| thread_key.to_owned()),
        labels,
    }
}

/// Identify the team and conversation segments by their Slack prefix, ignoring
/// the leading source namespace and any numeric ``thread_ts``. Returns the
/// first team (``T…``) and first conversation (``C``/``D``/``G``) found.
fn parse_slack_segments(thread_key: &str) -> (Option<&str>, Option<&str>) {
    let mut team = None;
    let mut conversation = None;
    // Slack object ids are always uppercase, so match case-sensitively: a
    // numeric thread_ts never matches, and a lowercase placeholder like "ts"
    // is correctly ignored rather than mistaken for a team.
    for segment in thread_key.split(':').skip(1).map(str::trim) {
        match segment.chars().next() {
            Some('T') if team.is_none() => team = Some(segment),
            Some('C' | 'D' | 'G') if conversation.is_none() => conversation = Some(segment),
            _ => {}
        }
    }
    (team, conversation)
}

/// Slack direct-message conversation ids start with ``D``.
fn is_direct_message(conversation_id: Option<&str>) -> bool {
    conversation_id
        .and_then(|id| id.chars().next())
        .is_some_and(|first| first.eq_ignore_ascii_case(&'d'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dm_with_user_keys_on_the_user() {
        let principal = derive_principal("slack:D0420:1780000000.0001", Some("U07ABC"), None);
        assert_eq!(principal.foreign_id, "slack-user-u07abc");
        assert_eq!(principal.name, "Slack User U07ABC");
        assert_eq!(
            principal.labels.get("slack_user_id").map(String::as_str),
            Some("U07ABC")
        );
    }

    #[test]
    fn dm_without_user_falls_back_to_the_conversation() {
        let principal = derive_principal("slack:D0420:1780000000.0001", None, None);
        assert_eq!(principal.foreign_id, "slack-channel-d0420");
    }

    #[test]
    fn channel_keys_on_the_channel_even_with_a_user() {
        let principal = derive_principal("chat:C123:1780000000.000000", Some("U07ABC"), None);
        assert_eq!(principal.foreign_id, "slack-channel-c123");
        assert_eq!(principal.name, "Slack Channel C123");
        assert_eq!(
            principal.labels.get("slack_channel_id").map(String::as_str),
            Some("C123")
        );
    }

    #[test]
    fn private_group_keys_on_the_channel() {
        let principal = derive_principal("slack:G99:ts", Some("U1"), None);
        assert_eq!(principal.foreign_id, "slack-channel-g99");
    }

    #[test]
    fn team_id_is_folded_into_the_channel_key() {
        let principal = derive_principal("slack:T123:C456:1780000000.0001", Some("U1"), None);
        assert_eq!(principal.foreign_id, "slack-channel-t123-c456");
        assert_eq!(principal.name, "Slack Channel C456 (team T123)");
        assert_eq!(
            principal.labels.get("slack_team_id").map(String::as_str),
            Some("T123")
        );
        assert_eq!(
            principal.labels.get("slack_channel_id").map(String::as_str),
            Some("C456")
        );
    }

    #[test]
    fn team_id_is_folded_into_the_dm_user_key() {
        let principal = derive_principal("slack:T123:D9:ts", Some("U07ABC"), None);
        assert_eq!(principal.foreign_id, "slack-user-t123-u07abc");
        assert_eq!(principal.name, "Slack User U07ABC (team T123)");
    }

    #[test]
    fn non_slack_thread_keys_slug_the_whole_key() {
        let principal = derive_principal("api", None, None);
        assert_eq!(principal.foreign_id, "thread-api");
        assert_eq!(principal.name, "api");
    }

    #[test]
    fn conversation_name_overrides_the_channel_display_name_but_not_the_key() {
        let principal = derive_principal("slack:T123:C456:ts", Some("U1"), Some("eng-oncall"));
        // Key stays derived from ids so renames never split the principal.
        assert_eq!(principal.foreign_id, "slack-channel-t123-c456");
        assert_eq!(principal.name, "Slack Channel #eng-oncall");
    }

    #[test]
    fn conversation_name_overrides_the_dm_display_name() {
        let principal = derive_principal("slack:D0420:ts", Some("U07ABC"), Some("Ada Lovelace"));
        assert_eq!(principal.foreign_id, "slack-user-u07abc");
        assert_eq!(principal.name, "Slack DM @Ada Lovelace");
    }

    #[test]
    fn blank_conversation_name_falls_back_to_the_synthetic_name() {
        let principal = derive_principal("chat:C123:ts", None, Some("   "));
        assert_eq!(principal.name, "Slack Channel C123");
    }

    #[test]
    fn identity_input_carries_namespace_and_managed_label() {
        let input = derive_principal("chat:C1:ts", None, None).to_identity_input("default");
        assert_eq!(input.namespace, "default");
        assert_eq!(input.foreign_id, "slack-channel-c1");
        assert_eq!(
            input.labels.get("managed-by").map(String::as_str),
            Some("centaur")
        );
        assert_eq!(
            input.labels.get("slack_channel_id").map(String::as_str),
            Some("C1")
        );
    }
}
