//! Derive the iron-control principal a session's proxy should act as.
//!
//! A principal is the identity that holds roles and owns proxies. For Centaur
//! the principal is the conversation: a Discord **channel** (every thread in it
//! shares one principal), a Linear **issue** (every agent session on it shares
//! one principal), a Microsoft Teams **channel/conversation** (or **user** for
//! a personal/user-scoped run when the acting user is known), or — for Slack —
//! a **user** for a 1:1 DM and a **channel** for a multi-party channel/group
//! thread. The Slack thread key is
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

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;

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
/// ``actor_user_id`` is the acting user, when known (carried in session
/// metadata). It is used to key 1:1 Slack DMs and Teams chats without a team;
/// channel threads key on the channel/conversation so everyone in the channel
/// shares one principal. When the thread key is not a recognizable chat
/// conversation, the whole key is slugged so every thread still maps to a
/// deterministic, distinct principal.
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
    actor_user_id: Option<&str>,
    conversation_name: Option<&str>,
) -> PrincipalRef {
    let display_name = conversation_name
        .map(str::trim)
        .filter(|name| !name.is_empty());

    // Discord sessions key on the channel so every thread in a channel shares
    // one principal (mirrors the Slack channel model). The thread key is
    // ``discord:<guild_id>:<channel_id>[:<thread_id>]``; the guild id is folded
    // into the key so the same channel id in two guilds never collides.
    if let Some((guild_id, channel_id)) = parse_discord_segments(thread_key) {
        let mut labels = BTreeMap::new();
        labels.insert("discord_guild_id".to_owned(), guild_id.to_owned());
        let scope = format!("{}-", slugify(guild_id));
        let key_id = channel_id.unwrap_or(guild_id);
        if let Some(channel) = channel_id {
            labels.insert("discord_channel_id".to_owned(), channel.to_owned());
        }
        return PrincipalRef {
            foreign_id: format!("discord-channel-{scope}{}", slugify(key_id)),
            name: display_name
                .map(|name| format!("Discord Channel #{name}"))
                .unwrap_or_else(|| format!("Discord Channel {key_id} (guild {guild_id})")),
            labels,
        };
    }

    // Linear sessions key on the issue so every agent session on an issue shares
    // one principal (mirrors the Slack channel model). The thread key is
    // ``linear:<issue_id>[:…]``; ``display_name`` is the issue identifier the
    // linearbot resolves — cosmetic, since the key stays derived from the id.
    if let Some(issue_id) = parse_linear_issue(thread_key) {
        let mut labels = BTreeMap::new();
        labels.insert("linear_issue_id".to_owned(), issue_id.to_owned());
        return PrincipalRef {
            foreign_id: format!("linear-issue-{}", slugify(issue_id)),
            name: display_name
                .map(|name| format!("Linear Issue #{name}"))
                .unwrap_or_else(|| format!("Linear Issue {issue_id}")),
            labels,
        };
    }

    if let Some((conversation_id, service_url, thread_id)) =
        parse_teams_adapter_segments(thread_key)
    {
        let mut labels = BTreeMap::new();
        labels.insert(
            "teams_conversation_id".to_owned(),
            conversation_id.to_owned(),
        );
        labels.insert("teams_service_url".to_owned(), service_url.to_owned());
        if let Some(thread) = thread_id {
            labels.insert("teams_thread_id".to_owned(), thread.to_owned());
        }
        if let Some(user) = actor_user_id.map(str::trim).filter(|user| !user.is_empty())
            && !conversation_id.starts_with("19:")
        {
            labels.insert("teams_user_id".to_owned(), user.to_owned());
            return PrincipalRef {
                foreign_id: format!("teams-user-{}", slugify(user)),
                name: display_name
                    .map(|name| format!("Teams User @{name}"))
                    .unwrap_or_else(|| format!("Teams User {user}")),
                labels,
            };
        }
        return PrincipalRef {
            foreign_id: format!("teams-conversation-{}", slugify(&conversation_id)),
            name: display_name
                .map(|name| format!("Teams Conversation {name}"))
                .unwrap_or_else(|| format!("Teams Conversation {conversation_id}")),
            labels,
        };
    }

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

    if is_direct_message(conversation_id)
        && let Some(user) = actor_user_id.map(str::trim).filter(|user| !user.is_empty())
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

/// The guild and (optional) channel segments of a ``discord:<guild>:<channel>``
/// thread key, or ``None`` when the key is not a Discord thread. The discordbot
/// encodes session threads as ``discord:<guild_id>:<channel_id>[:<thread_id>]``,
/// so keying on the channel groups every thread in a channel onto one principal.
fn parse_discord_segments(thread_key: &str) -> Option<(&str, Option<&str>)> {
    let rest = thread_key.strip_prefix("discord:")?;
    let mut segments = rest.split(':').map(str::trim);
    let guild = segments.next().filter(|guild| !guild.is_empty())?;
    let channel = segments.next().filter(|channel| !channel.is_empty());
    Some((guild, channel))
}

/// The Linear issue id from a ``linear:<issue_id>[:…]`` thread key, or ``None``
/// when the key is not a Linear thread. The linearbot encodes every agent
/// session on an issue with the same ``linear:<issue_id>:s:<session_id>`` key,
/// so keying on the issue id groups one issue's sessions onto one principal.
fn parse_linear_issue(thread_key: &str) -> Option<&str> {
    let rest = thread_key.strip_prefix("linear:")?;
    rest.split(':')
        .next()
        .map(str::trim)
        .filter(|issue| !issue.is_empty())
}

/// Parse the official Chat SDK Teams adapter key:
/// ``teams:<base64url conversation id>:<base64url service url>``.
fn parse_teams_adapter_segments(thread_key: &str) -> Option<(String, String, Option<String>)> {
    let rest = thread_key.strip_prefix("teams:")?;
    let mut segments = rest.split(':');
    let conversation = segments.next().filter(|value| !value.is_empty())?;
    let service_url = segments.next().filter(|value| !value.is_empty())?;
    if segments.next().is_some() {
        return None;
    }
    let conversation_id = String::from_utf8(URL_SAFE_NO_PAD.decode(conversation).ok()?).ok()?;
    let service_url = String::from_utf8(URL_SAFE_NO_PAD.decode(service_url).ok()?).ok()?;
    if conversation_id.is_empty() || service_url.is_empty() {
        return None;
    }
    let (conversation_id, thread_id) = conversation_id
        .split_once(";messageid=")
        .map(|(conversation, thread)| {
            (
                conversation.to_owned(),
                (!thread.is_empty()).then(|| thread.to_owned()),
            )
        })
        .unwrap_or((conversation_id, None));
    Some((conversation_id, service_url, thread_id))
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
    fn discord_sessions_key_on_the_channel() {
        // Two threads in the same channel resolve to one principal.
        let thread_a = derive_principal("discord:111:222:333", None, None);
        let thread_b = derive_principal("discord:111:222:444", None, None);
        assert_eq!(thread_a.foreign_id, "discord-channel-111-222");
        assert_eq!(thread_a.foreign_id, thread_b.foreign_id);
        assert_eq!(thread_a.name, "Discord Channel 222 (guild 111)");
        assert_eq!(
            thread_a
                .labels
                .get("discord_channel_id")
                .map(String::as_str),
            Some("222")
        );
        assert_eq!(
            thread_a.labels.get("discord_guild_id").map(String::as_str),
            Some("111")
        );
    }

    #[test]
    fn linear_sessions_key_on_the_issue() {
        // Two agent sessions on the same issue resolve to one principal.
        let session_a = derive_principal("linear:issue-1:s:sess-a", None, None);
        let session_b = derive_principal("linear:issue-1:s:sess-b", None, None);
        assert_eq!(session_a.foreign_id, "linear-issue-issue-1");
        assert_eq!(session_a.foreign_id, session_b.foreign_id);
        assert_eq!(session_a.name, "Linear Issue issue-1");
        assert_eq!(
            session_a.labels.get("linear_issue_id").map(String::as_str),
            Some("issue-1")
        );
    }

    #[test]
    fn discord_conversation_name_overrides_the_display_name_but_not_the_key() {
        let principal = derive_principal("discord:111:222:333", None, Some("general"));
        // Key stays derived from the ids so a channel rename never splits it.
        assert_eq!(principal.foreign_id, "discord-channel-111-222");
        assert_eq!(principal.name, "Discord Channel #general");
    }

    #[test]
    fn linear_conversation_name_overrides_the_display_name_but_not_the_key() {
        let principal = derive_principal("linear:issue-1:s:sess-a", None, Some("ENG-123"));
        // Key stays derived from the issue id so a rename never splits it.
        assert_eq!(principal.foreign_id, "linear-issue-issue-1");
        assert_eq!(principal.name, "Linear Issue #ENG-123");
    }

    #[test]
    fn linear_issue_level_thread_keys_on_the_issue() {
        let principal = derive_principal("linear:issue-1", None, None);
        assert_eq!(principal.foreign_id, "linear-issue-issue-1");
    }

    #[test]
    fn teams_adapter_conversation_keys_on_the_conversation() {
        let conversation = URL_SAFE_NO_PAD.encode("19:abc123@thread.tacv2");
        let service_url = URL_SAFE_NO_PAD.encode("https://smba.trafficmanager.net/amer/");
        let principal = derive_principal(
            &format!("teams:{conversation}:{service_url}"),
            Some("aad-user-1"),
            Some("general"),
        );
        assert_eq!(
            principal.foreign_id,
            "teams-conversation-19-abc123-thread-tacv2"
        );
        assert_eq!(principal.name, "Teams Conversation general");
        assert_eq!(
            principal
                .labels
                .get("teams_conversation_id")
                .map(String::as_str),
            Some("19:abc123@thread.tacv2")
        );
        assert_eq!(
            principal
                .labels
                .get("teams_service_url")
                .map(String::as_str),
            Some("https://smba.trafficmanager.net/amer/")
        );
    }

    #[test]
    fn teams_adapter_channel_thread_suffix_does_not_change_the_conversation_principal() {
        let conversation =
            URL_SAFE_NO_PAD.encode("19:abc123@thread.tacv2;messageid=root-message-1");
        let service_url = URL_SAFE_NO_PAD.encode("https://smba.trafficmanager.net/amer/");
        let principal = derive_principal(
            &format!("teams:{conversation}:{service_url}"),
            Some("aad-user-1"),
            Some("general"),
        );
        assert_eq!(
            principal.foreign_id,
            "teams-conversation-19-abc123-thread-tacv2"
        );
        assert_eq!(
            principal
                .labels
                .get("teams_conversation_id")
                .map(String::as_str),
            Some("19:abc123@thread.tacv2")
        );
        assert_eq!(
            principal.labels.get("teams_thread_id").map(String::as_str),
            Some("root-message-1")
        );
    }

    #[test]
    fn teams_adapter_dm_keys_on_the_actor_user() {
        let conversation = URL_SAFE_NO_PAD.encode("a:personal-conversation");
        let service_url = URL_SAFE_NO_PAD.encode("https://smba.trafficmanager.net/amer/");
        let principal = derive_principal(
            &format!("teams:{conversation}:{service_url}"),
            Some("aad-user-1"),
            Some("Casey"),
        );
        assert_eq!(principal.foreign_id, "teams-user-aad-user-1");
        assert_eq!(principal.name, "Teams User @Casey");
        assert_eq!(
            principal.labels.get("teams_user_id").map(String::as_str),
            Some("aad-user-1")
        );
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
