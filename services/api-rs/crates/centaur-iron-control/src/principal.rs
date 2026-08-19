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
//! folded into a DM principal key so the same user id in two workspaces never
//! collides onto one principal. Slack DMs require both a team id and acting user
//! id; derivation fails rather than emitting a legacy teamless or
//! conversation-keyed DM principal.
//!
//! [`derive_principal`] is pure so the mapping is unit-tested directly; callers
//! upsert the returned [`PrincipalRef`] at session start.

use std::collections::BTreeMap;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use thiserror::Error;

use crate::models::PrincipalInput;
use crate::util::{managed_labels, slugify};

const SLACK_DM_KIND: &str = "slack_dm";
const SLACK_CHANNEL_KIND: &str = "slack_channel";
const DISCORD_CHANNEL_KIND: &str = "discord_channel";
const LINEAR_ISSUE_KIND: &str = "linear_issue";
const TEAMS_USER_KIND: &str = "teams_user";
const TEAMS_CONVERSATION_KIND: &str = "teams_conversation";

/// The principal a session resolves to, as a stable upsert key plus identity
/// fields and extensible labels.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrincipalRef {
    pub foreign_id: String,
    pub name: String,
    pub kind: Option<String>,
    pub slack_user_id: Option<String>,
    pub slack_channel_id: Option<String>,
    pub slack_team_id: Option<String>,
    pub labels: BTreeMap<String, String>,
}

/// A chat thread does not contain enough identity information to derive its
/// canonical principal.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum PrincipalDerivationError {
    #[error("Slack DM principal requires a Slack team ID")]
    MissingSlackTeamId,
    #[error("Slack DM principal requires an acting Slack user ID")]
    MissingSlackUserId,
}

impl PrincipalRef {
    /// Build the upsert body for this principal, tagging it as Centaur-managed.
    pub fn to_principal_input(&self) -> PrincipalInput {
        let mut labels = managed_labels();
        labels.extend(self.labels.clone());
        PrincipalInput {
            foreign_id: self.foreign_id.clone(),
            name: self.name.clone(),
            kind: self.kind.clone(),
            slack_user_id: self.slack_user_id.clone(),
            slack_channel_id: self.slack_channel_id.clone(),
            slack_team_id: self.slack_team_id.clone(),
            slack_email: None,
            labels,
        }
    }
}

/// Resolve the principal for a thread.
///
/// ``actor_user_id`` is the acting user, when known (carried in session
/// metadata). It is used to key 1:1 Slack DMs and Teams chats without a team.
/// Slack DMs additionally require a team id. Channel threads key on the
/// channel/conversation so everyone in the channel shares one principal. When
/// the thread key is not a recognizable chat conversation, the whole key is
/// slugged so every thread still maps to a deterministic, distinct principal.
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
) -> Result<PrincipalRef, PrincipalDerivationError> {
    derive_principal_with_slack_team(thread_key, actor_user_id, None, conversation_name)
}

/// Resolve the principal for a thread, allowing ingress metadata to supply the
/// Slack team id when legacy DM thread keys omit it.
pub fn derive_principal_with_slack_team(
    thread_key: &str,
    actor_user_id: Option<&str>,
    slack_team_id: Option<&str>,
    conversation_name: Option<&str>,
) -> Result<PrincipalRef, PrincipalDerivationError> {
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
        return Ok(PrincipalRef {
            foreign_id: format!("discord-channel-{scope}{}", slugify(key_id)),
            name: display_name
                .map(|name| format!("Discord Channel #{name}"))
                .unwrap_or_else(|| format!("Discord Channel {key_id} (guild {guild_id})")),
            kind: Some(DISCORD_CHANNEL_KIND.to_owned()),
            slack_user_id: None,
            slack_channel_id: None,
            slack_team_id: None,
            labels,
        });
    }

    // Linear sessions key on the issue so every agent session on an issue shares
    // one principal (mirrors the Slack channel model). The thread key is
    // ``linear:<issue_id>[:…]``; ``display_name`` is the issue identifier the
    // linearbot resolves — cosmetic, since the key stays derived from the id.
    if let Some(issue_id) = parse_linear_issue(thread_key) {
        let mut labels = BTreeMap::new();
        labels.insert("linear_issue_id".to_owned(), issue_id.to_owned());
        return Ok(PrincipalRef {
            foreign_id: format!("linear-issue-{}", slugify(issue_id)),
            name: display_name
                .map(|name| format!("Linear Issue #{name}"))
                .unwrap_or_else(|| format!("Linear Issue {issue_id}")),
            kind: Some(LINEAR_ISSUE_KIND.to_owned()),
            slack_user_id: None,
            slack_channel_id: None,
            slack_team_id: None,
            labels,
        });
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
            return Ok(PrincipalRef {
                foreign_id: format!("teams-user-{}", slugify(user)),
                name: display_name
                    .map(|name| format!("Teams User @{name}"))
                    .unwrap_or_else(|| format!("Teams User {user}")),
                kind: Some(TEAMS_USER_KIND.to_owned()),
                slack_user_id: None,
                slack_channel_id: None,
                slack_team_id: None,
                labels,
            });
        }
        return Ok(PrincipalRef {
            foreign_id: format!("teams-conversation-{}", slugify(&conversation_id)),
            name: display_name
                .map(|name| format!("Teams Conversation {name}"))
                .unwrap_or_else(|| format!("Teams Conversation {conversation_id}")),
            kind: Some(TEAMS_CONVERSATION_KIND.to_owned()),
            slack_user_id: None,
            slack_channel_id: None,
            slack_team_id: None,
            labels,
        });
    }

    let (thread_team_id, conversation_id) = parse_slack_segments(thread_key);
    let metadata_team_id = slack_team_id.map(str::trim).filter(|team| !team.is_empty());
    if is_direct_message(conversation_id) {
        let team = thread_team_id
            .or(metadata_team_id)
            .ok_or(PrincipalDerivationError::MissingSlackTeamId)?;
        let user = actor_user_id
            .map(str::trim)
            .filter(|user| !user.is_empty())
            .ok_or(PrincipalDerivationError::MissingSlackUserId)?;
        return Ok(slack_user_principal(user, team, display_name));
    }

    // Channel principals must never be scoped by the message-derived metadata
    // team: in Slack Connect shared channels that team can identify an external
    // requester's workspace, which would fork a separate principal from the
    // host channel's. Thread-key team ids remain part of the established
    // channel identity format.
    let team_id = thread_team_id;
    let scope = team_id
        .map(|team| format!("{}-", slugify(team)))
        .unwrap_or_default();
    let team_suffix = team_id
        .map(|team| format!(" (team {team})"))
        .unwrap_or_default();

    if let Some(conversation_id) = conversation_id {
        return Ok(PrincipalRef {
            foreign_id: format!("slack-channel-{scope}{}", slugify(conversation_id)),
            name: display_name
                .map(|name| format!("Slack Channel #{name}"))
                .unwrap_or_else(|| format!("Slack Channel {conversation_id}{team_suffix}")),
            kind: Some(SLACK_CHANNEL_KIND.to_owned()),
            slack_user_id: None,
            slack_channel_id: Some(conversation_id.to_owned()),
            slack_team_id: team_id.map(ToOwned::to_owned),
            labels: BTreeMap::new(),
        });
    }

    Ok(PrincipalRef {
        foreign_id: format!("thread-{}", slugify(thread_key)),
        name: display_name
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| thread_key.to_owned()),
        kind: None,
        slack_user_id: None,
        slack_channel_id: None,
        slack_team_id: None,
        labels: BTreeMap::new(),
    })
}

/// Resolve the requesting user's principal for a Slack channel thread. This is
/// the same per-user principal a 1:1 DM with that user resolves to (both key on
/// the user's own workspace), so a user first seen in a channel and later in a
/// DM (or vice versa) maps to one identity. Returns `None` for DM threads (the
/// conversation principal already is the user's) and for non-Slack thread keys,
/// which have no Slack requester.
///
/// ``slack_team_id`` must be a workspace the user is proven to belong to —
/// callers pass the home-team-gate-validated metadata team. The thread key's
/// team segment is deliberately never used: it names where the conversation
/// lives, not where the user belongs, and Slack user ids are only unique
/// within a workspace, so keying a user on the thread's team could mint (or
/// collide with) an identity in a workspace the user is not a member of.
pub fn derive_slack_requester_principal(
    thread_key: &str,
    slack_user_id: &str,
    slack_team_id: &str,
    display_name: Option<&str>,
) -> Option<PrincipalRef> {
    let (_, conversation_id) = parse_slack_segments(thread_key);
    let conversation_id = conversation_id?;
    if is_direct_message(Some(conversation_id)) {
        return None;
    }
    let user = slack_user_id.trim();
    let team = slack_team_id.trim();
    if user.is_empty() || team.is_empty() {
        return None;
    }
    Some(slack_user_principal(
        user,
        team,
        display_name.map(str::trim).filter(|name| !name.is_empty()),
    ))
}

/// The per-user Slack principal shared by the DM branch of
/// [`derive_principal_with_slack_team`] and
/// [`derive_slack_requester_principal`], so the two can never mint diverging
/// foreign ids or labels for the same user. ``team`` must be a workspace the
/// user is proven to belong to, never one inferred from where the message
/// happened to land.
fn slack_user_principal(user: &str, team: &str, display_name: Option<&str>) -> PrincipalRef {
    PrincipalRef {
        foreign_id: format!("slack-user-{}-{}", slugify(team), slugify(user)),
        name: display_name
            .map(|name| format!("Slack DM @{name}"))
            .unwrap_or_else(|| format!("Slack User {user} (team {team})")),
        kind: Some(SLACK_DM_KIND.to_owned()),
        slack_user_id: Some(user.to_owned()),
        slack_channel_id: None,
        slack_team_id: Some(team.to_owned()),
        labels: BTreeMap::new(),
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

/// The first Slack conversation id (``C``/``D``/``G``) in a thread key.
pub(crate) fn slack_conversation_id(thread_key: &str) -> Option<&str> {
    parse_slack_segments(thread_key).1
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
pub(crate) fn is_direct_message(conversation_id: Option<&str>) -> bool {
    conversation_id
        .and_then(|id| id.chars().next())
        .is_some_and(|first| first.eq_ignore_ascii_case(&'d'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn derive_principal(
        thread_key: &str,
        actor_user_id: Option<&str>,
        conversation_name: Option<&str>,
    ) -> PrincipalRef {
        super::derive_principal(thread_key, actor_user_id, conversation_name)
            .expect("principal should be derivable")
    }

    fn derive_principal_with_slack_team(
        thread_key: &str,
        actor_user_id: Option<&str>,
        slack_team_id: Option<&str>,
        conversation_name: Option<&str>,
    ) -> PrincipalRef {
        super::derive_principal_with_slack_team(
            thread_key,
            actor_user_id,
            slack_team_id,
            conversation_name,
        )
        .expect("principal should be derivable")
    }

    #[test]
    fn dm_with_user_keys_on_the_user() {
        let principal = derive_principal("slack:T123:D0420:1780000000.0001", Some("U07ABC"), None);
        assert_eq!(principal.foreign_id, "slack-user-t123-u07abc");
        assert_eq!(principal.name, "Slack User U07ABC (team T123)");
        assert_eq!(principal.slack_user_id.as_deref(), Some("U07ABC"));
        assert_eq!(principal.slack_team_id.as_deref(), Some("T123"));
        assert_eq!(principal.kind.as_deref(), Some("slack_dm"));
        assert!(principal.labels.is_empty());
    }

    #[test]
    fn dm_without_team_id_is_rejected() {
        assert_eq!(
            super::derive_principal("slack:D0420:1780000000.0001", Some("U07ABC"), None),
            Err(PrincipalDerivationError::MissingSlackTeamId)
        );
    }

    #[test]
    fn dm_without_user_id_is_rejected() {
        assert_eq!(
            super::derive_principal("slack:T123:D0420:1780000000.0001", None, None),
            Err(PrincipalDerivationError::MissingSlackUserId)
        );
    }

    #[test]
    fn channel_keys_on_the_channel_even_with_a_user() {
        let principal = derive_principal("chat:C123:1780000000.000000", Some("U07ABC"), None);
        assert_eq!(principal.foreign_id, "slack-channel-c123");
        assert_eq!(principal.name, "Slack Channel C123");
        assert_eq!(principal.slack_channel_id.as_deref(), Some("C123"));
        assert_eq!(principal.kind.as_deref(), Some("slack_channel"));
        assert!(principal.labels.is_empty());
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
        assert_eq!(principal.slack_team_id.as_deref(), Some("T123"));
        assert_eq!(principal.slack_channel_id.as_deref(), Some("C456"));
    }

    #[test]
    fn team_id_is_folded_into_the_dm_user_key() {
        let principal = derive_principal("slack:T123:D9:ts", Some("U07ABC"), None);
        assert_eq!(principal.foreign_id, "slack-user-t123-u07abc");
        assert_eq!(principal.name, "Slack User U07ABC (team T123)");
    }

    #[test]
    fn teamless_dm_thread_key_uses_metadata_team_id() {
        let principal = derive_principal_with_slack_team(
            "slack:D9:ts",
            Some("U07ABC"),
            Some("T123"),
            Some("Ada Lovelace"),
        );
        assert_eq!(principal.foreign_id, "slack-user-t123-u07abc");
        assert_eq!(principal.name, "Slack DM @Ada Lovelace");
        assert_eq!(principal.slack_team_id.as_deref(), Some("T123"));
        assert_eq!(principal.slack_user_id.as_deref(), Some("U07ABC"));
    }

    #[test]
    fn metadata_team_id_is_not_folded_into_channel_key() {
        // A Slack Connect requester's team must not scope a channel principal:
        // the channel is globally unique and belongs to the host workspace.
        let principal = derive_principal_with_slack_team(
            "slack:C456:1780000000.0001",
            Some("U07ABC"),
            Some("T_EXTERNAL"),
            None,
        );
        assert_eq!(principal.foreign_id, "slack-channel-c456");
        assert_eq!(principal.slack_team_id, None);
        assert_eq!(principal.slack_channel_id.as_deref(), Some("C456"));
    }

    #[test]
    fn thread_key_team_id_wins_over_metadata_team_id() {
        let principal = derive_principal_with_slack_team(
            "slack:T_FROM_KEY:D9:ts",
            Some("U07ABC"),
            Some("T_FROM_METADATA"),
            None,
        );
        assert_eq!(principal.foreign_id, "slack-user-t-from-key-u07abc");
        assert_eq!(principal.slack_team_id.as_deref(), Some("T_FROM_KEY"));
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
        let principal =
            derive_principal("slack:T123:D0420:ts", Some("U07ABC"), Some("Ada Lovelace"));
        assert_eq!(principal.foreign_id, "slack-user-t123-u07abc");
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
        assert_eq!(thread_a.kind.as_deref(), Some("discord_channel"));
        assert!(!thread_a.labels.contains_key("kind"));
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
        assert_eq!(session_a.kind.as_deref(), Some("linear_issue"));
        assert!(!session_a.labels.contains_key("kind"));
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
        assert_eq!(principal.kind.as_deref(), Some("teams_conversation"));
        assert!(!principal.labels.contains_key("kind"));
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
        assert_eq!(principal.kind.as_deref(), Some("teams_user"));
        assert!(!principal.labels.contains_key("kind"));
    }

    #[test]
    fn requester_channel_thread_keys_on_the_user() {
        let principal = derive_slack_requester_principal(
            "slack:T123:C456:1780000000.0001",
            "U07ABC",
            "T123",
            Some("Ada Lovelace"),
        )
        .unwrap();
        assert_eq!(principal.foreign_id, "slack-user-t123-u07abc");
        assert_eq!(principal.name, "Slack DM @Ada Lovelace");
        assert_eq!(principal.kind.as_deref(), Some("slack_dm"));
        assert_eq!(principal.slack_user_id.as_deref(), Some("U07ABC"));
        assert_eq!(principal.slack_team_id.as_deref(), Some("T123"));
        assert!(principal.labels.is_empty());
    }

    #[test]
    fn requester_matches_the_dm_principal_for_the_same_user() {
        let requester = derive_slack_requester_principal(
            "slack:T123:C456:1780000000.0001",
            "U07ABC",
            "T123",
            None,
        )
        .unwrap();
        let dm = derive_principal("slack:T123:D9:ts", Some("U07ABC"), None);
        assert_eq!(requester.foreign_id, dm.foreign_id);
        assert_eq!(requester.kind, dm.kind);
        assert_eq!(requester.slack_user_id, dm.slack_user_id);
        assert_eq!(requester.slack_team_id, dm.slack_team_id);
    }

    #[test]
    fn requester_dm_thread_resolves_none() {
        assert_eq!(
            derive_slack_requester_principal("slack:T123:D9:ts", "U07ABC", "T123", None),
            None
        );
    }

    #[test]
    fn requester_non_slack_threads_resolve_none() {
        for thread_key in [
            "discord:111:222:333",
            "linear:issue-1:s:sess-a",
            "teams:abc:def",
            "mcp:prn_x",
            "api",
        ] {
            assert_eq!(
                derive_slack_requester_principal(thread_key, "U07ABC", "T123", None),
                None,
                "expected no requester for {thread_key}"
            );
        }
    }

    #[test]
    fn requester_teamless_thread_key_uses_verified_team() {
        let principal = derive_slack_requester_principal("chat:C123:ts", "U1", "T9", None).unwrap();
        assert_eq!(principal.foreign_id, "slack-user-t9-u1");
        assert_eq!(principal.slack_team_id.as_deref(), Some("T9"));
    }

    #[test]
    fn requester_ignores_the_thread_key_team() {
        let principal = derive_slack_requester_principal(
            "slack:T_FROM_KEY:C456:ts",
            "U1",
            "T_FROM_METADATA",
            None,
        )
        .unwrap();
        assert_eq!(principal.foreign_id, "slack-user-t-from-metadata-u1");
        assert_eq!(principal.slack_team_id.as_deref(), Some("T_FROM_METADATA"));
    }

    // The thread key's team must never substitute for a missing verified team:
    // it names where the conversation lives, not where the user belongs.
    #[test]
    fn requester_blank_team_resolves_none() {
        assert_eq!(
            derive_slack_requester_principal("slack:T123:C456:ts", "U07ABC", "  ", None),
            None
        );
    }

    #[test]
    fn requester_blank_user_resolves_none() {
        assert_eq!(
            derive_slack_requester_principal("slack:T123:C456:ts", "  ", "T123", None),
            None
        );
    }

    #[test]
    fn requester_synthetic_name_without_display_name() {
        let principal =
            derive_slack_requester_principal("slack:T123:C456:ts", "U07ABC", "T123", None).unwrap();
        assert_eq!(principal.name, "Slack User U07ABC (team T123)");
    }

    #[test]
    fn principal_input_uses_first_class_slack_identity_fields() {
        let input = derive_principal("chat:C1:ts", None, None).to_principal_input();
        assert_eq!(input.foreign_id, "slack-channel-c1");
        assert_eq!(
            input.labels.get("managed-by").map(String::as_str),
            Some("centaur")
        );
        assert_eq!(input.slack_channel_id.as_deref(), Some("C1"));
        assert_eq!(input.kind.as_deref(), Some("slack_channel"));
        assert_eq!(input.labels.len(), 1);

        let payload = serde_json::to_value(&input).unwrap();
        assert_eq!(payload["kind"], "slack_channel");
        assert_eq!(payload["slack_channel_id"], "C1");
        assert_eq!(payload["labels"]["managed-by"], "centaur");
        assert!(payload["labels"].get("kind").is_none());
        assert!(payload["labels"].get("slack_channel_id").is_none());
    }
}
