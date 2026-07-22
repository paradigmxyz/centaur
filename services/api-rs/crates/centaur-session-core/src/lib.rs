//! Durable session control-plane types.
//!
//! A session is the public control-plane object for one ongoing agent
//! conversation. `thread_key` is the canonical identifier.

use std::{collections::BTreeMap, fmt, str::FromStr};

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use serde_json::Value;
use strum::{AsRefStr, Display, EnumString};
use thiserror::Error;
use time::OffsetDateTime;

pub const MAX_THREAD_KEY_BYTES: usize = 512;

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct ThreadKey(String);

impl ThreadKey {
    pub fn parse(value: impl Into<String>) -> Result<Self, ThreadKeyError> {
        let value = value.into();
        validate_thread_key(&value)?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

impl fmt::Display for ThreadKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ThreadKey {
    type Err = ThreadKeyError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

impl TryFrom<String> for ThreadKey {
    type Error = ThreadKeyError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(value)
    }
}

impl AsRef<str> for ThreadKey {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl Serialize for ThreadKey {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ThreadKey {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(value).map_err(de::Error::custom)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Error)]
pub enum ThreadKeyError {
    #[error("thread_key is required")]
    Empty,
    #[error("thread_key must be at most {MAX_THREAD_KEY_BYTES} bytes")]
    TooLong,
    #[error("thread_key must be namespaced as '<source>:<id>'")]
    MissingNamespace,
    #[error("thread_key must not contain ASCII control characters")]
    ControlCharacter,
    #[error("thread_key must not be raw JSON")]
    RawJson,
}

fn validate_thread_key(value: &str) -> Result<(), ThreadKeyError> {
    if value.is_empty() {
        return Err(ThreadKeyError::Empty);
    }
    if value.len() > MAX_THREAD_KEY_BYTES {
        return Err(ThreadKeyError::TooLong);
    }
    if value.starts_with('{') || value.starts_with('[') {
        return Err(ThreadKeyError::RawJson);
    }
    if value.chars().any(|ch| ch.is_ascii_control()) {
        return Err(ThreadKeyError::ControlCharacter);
    }
    let Some((namespace, rest)) = value.split_once(':') else {
        return Err(ThreadKeyError::MissingNamespace);
    };
    if namespace.is_empty() || rest.is_empty() {
        return Err(ThreadKeyError::MissingNamespace);
    }
    Ok(())
}

/// The chat surface a thread is delivered to, parsed from its thread key.
///
/// Slack, Discord, Linear, and GitHub all encode the destination — where a reply
/// (and, where the surface supports it, an uploaded file) lands — directly in the
/// key. Resolving it in one place lets the API session context, the per-turn
/// context line the agent reads, and any caller that needs a posting destination
/// share a single parser instead of each re-deriving the platform from the key
/// shape.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ChatDestination {
    Slack {
        channel_id: String,
        thread_ts: String,
    },
    Discord {
        guild_id: String,
        channel_id: String,
        thread_id: Option<String>,
    },
    /// A Linear issue thread. The reply lands as a comment on the issue (nested
    /// under `comment_id` when the turn came in on a comment thread). Unlike
    /// Slack/Discord, Linear has no file-upload surface — comments are markdown.
    Linear {
        issue_id: String,
        comment_id: Option<String>,
        agent_session_id: Option<String>,
    },
    /// A GitHub issue or pull-request thread. The reply lands as a comment on
    /// the issue/PR (pinned to `review_comment_id` when the turn came in on a
    /// PR review-comment thread). Like Linear, GitHub has no file-upload
    /// surface — comments are markdown.
    Github {
        owner: String,
        repo: String,
        number: u64,
        kind: GithubThreadKind,
        review_comment_id: Option<u64>,
    },
}

/// Whether a GitHub thread maps to an issue or a pull request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GithubThreadKind {
    Issue,
    Pr,
}

impl GithubThreadKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Issue => "issue",
            Self::Pr => "pr",
        }
    }
}

impl ChatDestination {
    /// The platform identifier surfaced to the agent (`slack` / `discord` /
    /// `linear` / `github`).
    pub fn platform(&self) -> &'static str {
        match self {
            Self::Slack { .. } => "slack",
            Self::Discord { .. } => "discord",
            Self::Linear { .. } => "linear",
            Self::Github { .. } => "github",
        }
    }

    /// A terse, model-visible note describing the current chat surface. It is
    /// prepended to each user turn so the agent never has to infer which platform
    /// it is on — the static system prompt is platform-neutral, so this line is
    /// the agent's authoritative signal for where its reply and uploads go.
    pub fn context_line(&self) -> String {
        match self {
            Self::Slack {
                channel_id,
                thread_ts,
            } => format!(
                "[chat surface: Slack · channel {channel_id} · thread {thread_ts}. \
                 Centaur delivers your reply to this thread automatically — do not repost it with the slack tool. \
                 Send files here with `slack upload`.]"
            ),
            Self::Discord {
                guild_id,
                channel_id,
                thread_id,
            } => {
                let thread = thread_id
                    .as_deref()
                    .map(|id| format!(" · thread {id}"))
                    .unwrap_or_default();
                format!(
                    "[chat surface: Discord · channel {channel_id}{thread} (guild {guild_id}). \
                     Centaur delivers your reply to this thread automatically — do not repost it with the discord tool. \
                     Send files here with `discord upload`.]"
                )
            }
            Self::Linear {
                issue_id,
                comment_id,
                ..
            } => {
                let comment = comment_id
                    .as_deref()
                    .map(|id| format!(" · comment {id}"))
                    .unwrap_or_default();
                format!(
                    "[chat surface: Linear · issue {issue_id}{comment}. \
                     Centaur posts your reply as a comment on this Linear thread automatically — do not repost it with the linear tool. \
                     Linear replies are markdown comments with no file-upload surface; share artifacts inline or as a link.]"
                )
            }
            Self::Github {
                owner,
                repo,
                number,
                kind,
                review_comment_id,
            } => {
                let subject = match kind {
                    GithubThreadKind::Issue => "issue",
                    GithubThreadKind::Pr => "pull request",
                };
                let review_comment = review_comment_id
                    .map(|id| format!(" · review comment {id}"))
                    .unwrap_or_default();
                format!(
                    "[chat surface: GitHub · {subject} {owner}/{repo}#{number}{review_comment}. \
                     Centaur posts your reply as a comment on this GitHub thread automatically — do not repost it with `gh`. \
                     GitHub replies are markdown comments with no file-upload surface; share artifacts inline or as a link.]"
                )
            }
        }
    }
}

impl ThreadKey {
    /// Resolve the chat surface this thread is delivered to, when the key encodes
    /// a recognized platform destination.
    ///
    /// Returns `None` for keys that are not platform-addressable (e.g. `api:`
    /// threads, or githubbot's synthetic `github-review:` sessions). The Slack
    /// arms are kept byte-for-byte compatible with the historical
    /// session-context parser so existing Slack behavior is preserved; Discord
    /// keys are `discord:<guild>:<channel>[:<thread>]`, Linear keys are
    /// `linear:<issue>[:c:<comment>][:s:<agent_session>]` (mirroring the
    /// linearbot chat-SDK `encodeThreadId` shape), and GitHub keys are
    /// `github:<owner>/<repo>:<pr>[:rc:<review_comment>]` or
    /// `github:<owner>/<repo>:issue:<issue>` (mirroring githubbot's
    /// `parseGithubThreadKey`).
    pub fn chat_destination(&self) -> Option<ChatDestination> {
        let key = self.as_str();
        if let Some(rest) = key.strip_prefix("github:") {
            let (repo_path, thread) = rest.split_once(':')?;
            let (owner, repo) = repo_path.split_once('/')?;
            if owner.is_empty() || repo.is_empty() || repo.contains('/') {
                return None;
            }
            let segments = thread.split(':').collect::<Vec<_>>();
            let (kind, number, review_comment_id) = match segments.as_slice() {
                ["issue", number] => (GithubThreadKind::Issue, *number, None),
                [number] => (GithubThreadKind::Pr, *number, None),
                [number, "rc", comment] => (GithubThreadKind::Pr, *number, Some(*comment)),
                _ => return None,
            };
            let number = number.parse::<u64>().ok()?;
            let review_comment_id = match review_comment_id {
                Some(comment) => Some(comment.parse::<u64>().ok()?),
                None => None,
            };
            return Some(ChatDestination::Github {
                owner: owner.to_owned(),
                repo: repo.to_owned(),
                number,
                kind,
                review_comment_id,
            });
        }
        if let Some(rest) = key.strip_prefix("discord:") {
            let mut segments = rest.split(':').map(str::trim);
            let guild_id = segments.next().filter(|s| !s.is_empty())?;
            let channel_id = segments.next().filter(|s| !s.is_empty())?;
            let thread_id = segments
                .next()
                .filter(|s| !s.is_empty())
                .map(ToOwned::to_owned);
            return Some(ChatDestination::Discord {
                guild_id: guild_id.to_owned(),
                channel_id: channel_id.to_owned(),
                thread_id,
            });
        }
        if let Some(rest) = key.strip_prefix("linear:") {
            let segments = rest.split(':').collect::<Vec<_>>();
            let (issue_id, comment_id, agent_session_id) = match segments.as_slice() {
                [issue, "c", comment, "s", session] => (*issue, Some(*comment), Some(*session)),
                [issue, "s", session] => (*issue, None, Some(*session)),
                [issue, "c", comment] => (*issue, Some(*comment), None),
                [issue] => (*issue, None, None),
                _ => return None,
            };
            if issue_id.is_empty() {
                return None;
            }
            return Some(ChatDestination::Linear {
                issue_id: issue_id.to_owned(),
                comment_id: comment_id.filter(|s| !s.is_empty()).map(ToOwned::to_owned),
                agent_session_id: agent_session_id
                    .filter(|s| !s.is_empty())
                    .map(ToOwned::to_owned),
            });
        }
        let parts = key.split(':').collect::<Vec<_>>();
        let (channel_id, thread_ts) = match parts.as_slice() {
            ["slack", channel_id, thread_ts] => (*channel_id, *thread_ts),
            ["slack", _team_id, channel_id, thread_ts] => (*channel_id, *thread_ts),
            [channel_id, thread_ts] if is_slack_conversation_id(channel_id) => {
                (*channel_id, *thread_ts)
            }
            _ => return None,
        };
        if channel_id.is_empty() || thread_ts.is_empty() {
            return None;
        }
        Some(ChatDestination::Slack {
            channel_id: channel_id.to_owned(),
            thread_ts: thread_ts.to_owned(),
        })
    }
}

/// Slack conversation ids start with `C` (channel), `D` (DM), or `G` (group).
fn is_slack_conversation_id(value: &str) -> bool {
    matches!(value.as_bytes().first(), Some(b'C' | b'D' | b'G'))
}

#[derive(
    Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize, AsRefStr, Display, EnumString,
)]
#[serde(rename_all = "lowercase")]
#[strum(serialize_all = "lowercase")]
pub enum HarnessType {
    Codex,
    Amp,
    ClaudeCode,
    Nanocodex,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, AsRefStr, Display, EnumString)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum SessionStatus {
    Active,
    Idle,
    Executing,
    Failed,
    Archived,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxRepoCacheAccess {
    None,
    Public,
    #[default]
    All,
}

impl SandboxRepoCacheAccess {
    pub const fn enabled(&self) -> bool {
        !matches!(self, Self::None)
    }

    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Public => "public",
            Self::All => "all",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "none" => Some(Self::None),
            "public" => Some(Self::Public),
            "all" => Some(Self::All),
            _ => None,
        }
    }

    pub const fn from_legacy_enabled(enabled: bool) -> Self {
        if enabled { Self::All } else { Self::None }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SandboxCapabilities {
    #[serde(default)]
    pub repo_cache: SandboxRepoCacheAccess,
    pub observability_enabled: bool,
    pub api_server_enabled: bool,
}

impl SandboxCapabilities {
    pub const fn default_enabled() -> Self {
        Self {
            repo_cache: SandboxRepoCacheAccess::All,
            observability_enabled: true,
            api_server_enabled: true,
        }
    }

    pub const fn is_default_enabled(&self) -> bool {
        matches!(self.repo_cache, SandboxRepoCacheAccess::All)
            && self.observability_enabled
            && self.api_server_enabled
    }

    pub const fn repo_cache_enabled(&self) -> bool {
        self.repo_cache.enabled()
    }
}

impl Default for SandboxCapabilities {
    fn default() -> Self {
        Self::default_enabled()
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Session {
    pub thread_key: ThreadKey,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub sandbox_id: Option<String>,
    /// Capabilities applied to the currently assigned sandbox. `None` means the
    /// sandbox predates capability tracking; callers may treat it as compatible
    /// only with the default-enabled profile.
    #[serde(default)]
    pub sandbox_capabilities: Option<SandboxCapabilities>,
    pub harness_type: HarnessType,
    pub harness_thread_id: Option<String>,
    pub persona_id: Option<String>,
    pub status: SessionStatus,
    /// iron-control principal OID this session's egress proxy binds to,
    /// captured at registration so a resumed session can recreate its sandbox.
    pub iron_control_principal: Option<String>,
    /// Per-proxy labels captured at session creation and applied whenever this
    /// session's egress proxy is created, repaired, or rebound.
    #[serde(default)]
    pub proxy_labels: BTreeMap<String, String>,
    /// Last meaningful activity for the currently assigned sandbox. This is
    /// the eviction signal for capacity pressure and intentionally separate
    /// from `updated_at`, which also changes for metadata/status writes.
    pub sandbox_last_active_at: Option<OffsetDateTime>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, AsRefStr, Display, EnumString)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum MessageRole {
    User,
    Assistant,
    System,
    Tool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SessionMessageInput {
    #[serde(default)]
    pub client_message_id: Option<String>,
    pub role: MessageRole,
    pub parts: Vec<Value>,
    #[serde(default = "empty_object")]
    pub metadata: Value,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SessionMessage {
    pub message_id: String,
    pub client_message_id: Option<String>,
    pub thread_key: ThreadKey,
    pub role: MessageRole,
    pub parts: Vec<Value>,
    pub metadata: Value,
    pub created_at: OffsetDateTime,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, AsRefStr, Display, EnumString)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum ExecutionStatus {
    Queued,
    Running,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SessionExecution {
    pub execution_id: String,
    pub idempotency_key: Option<String>,
    pub thread_key: ThreadKey,
    pub status: ExecutionStatus,
    pub metadata: Value,
    pub error: Option<String>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
    pub started_at: Option<OffsetDateTime>,
    pub completed_at: Option<OffsetDateTime>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SessionEvent {
    pub event_id: i64,
    pub thread_key: ThreadKey,
    pub execution_id: Option<String>,
    pub event_type: String,
    pub payload: Value,
    pub created_at: OffsetDateTime,
}

pub fn empty_object() -> Value {
    Value::Object(serde_json::Map::new())
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::{ChatDestination, GithubThreadKind, HarnessType, ThreadKey};

    #[test]
    fn chat_destination_resolves_slack_keys() {
        let dest = ThreadKey::parse("slack:C123:123.456")
            .unwrap()
            .chat_destination()
            .unwrap();
        assert_eq!(
            dest,
            ChatDestination::Slack {
                channel_id: "C123".to_owned(),
                thread_ts: "123.456".to_owned(),
            }
        );
        assert_eq!(dest.platform(), "slack");

        // The team-id variant shifts the channel/ts one segment to the right.
        let team = ThreadKey::parse("slack:T999:C123:123.456")
            .unwrap()
            .chat_destination()
            .unwrap();
        assert_eq!(
            team,
            ChatDestination::Slack {
                channel_id: "C123".to_owned(),
                thread_ts: "123.456".to_owned(),
            }
        );

        // A bare conversation id (C/D/G prefix) plus a timestamp also resolves.
        let bare = ThreadKey::parse("D42:123.456")
            .unwrap()
            .chat_destination()
            .unwrap();
        assert_eq!(
            bare,
            ChatDestination::Slack {
                channel_id: "D42".to_owned(),
                thread_ts: "123.456".to_owned(),
            }
        );
    }

    #[test]
    fn chat_destination_resolves_discord_keys() {
        let with_thread = ThreadKey::parse("discord:111:222:333")
            .unwrap()
            .chat_destination()
            .unwrap();
        assert_eq!(
            with_thread,
            ChatDestination::Discord {
                guild_id: "111".to_owned(),
                channel_id: "222".to_owned(),
                thread_id: Some("333".to_owned()),
            }
        );
        assert_eq!(with_thread.platform(), "discord");

        // The thread segment is optional (a channel-root message).
        let no_thread = ThreadKey::parse("discord:111:222")
            .unwrap()
            .chat_destination()
            .unwrap();
        assert_eq!(
            no_thread,
            ChatDestination::Discord {
                guild_id: "111".to_owned(),
                channel_id: "222".to_owned(),
                thread_id: None,
            }
        );
    }

    #[test]
    fn chat_destination_resolves_linear_keys() {
        // An agent session anchored to a comment carries both ids.
        let comment_session = ThreadKey::parse("linear:ISSUE:c:CMT:s:SESS")
            .unwrap()
            .chat_destination()
            .unwrap();
        assert_eq!(
            comment_session,
            ChatDestination::Linear {
                issue_id: "ISSUE".to_owned(),
                comment_id: Some("CMT".to_owned()),
                agent_session_id: Some("SESS".to_owned()),
            }
        );
        assert_eq!(comment_session.platform(), "linear");

        // An issue-level agent session has a session id but no comment.
        let issue_session = ThreadKey::parse("linear:ISSUE:s:SESS")
            .unwrap()
            .chat_destination()
            .unwrap();
        assert_eq!(
            issue_session,
            ChatDestination::Linear {
                issue_id: "ISSUE".to_owned(),
                comment_id: None,
                agent_session_id: Some("SESS".to_owned()),
            }
        );

        // A plain comment thread, and a bare issue, both resolve.
        let comment = ThreadKey::parse("linear:ISSUE:c:CMT")
            .unwrap()
            .chat_destination()
            .unwrap();
        assert_eq!(
            comment,
            ChatDestination::Linear {
                issue_id: "ISSUE".to_owned(),
                comment_id: Some("CMT".to_owned()),
                agent_session_id: None,
            }
        );
        let issue = ThreadKey::parse("linear:ISSUE")
            .unwrap()
            .chat_destination()
            .unwrap();
        assert_eq!(
            issue,
            ChatDestination::Linear {
                issue_id: "ISSUE".to_owned(),
                comment_id: None,
                agent_session_id: None,
            }
        );
    }

    #[test]
    fn chat_destination_resolves_github_keys() {
        // A bare number is a PR conversation thread.
        let pr = ThreadKey::parse("github:0xSplits/centaur:704")
            .unwrap()
            .chat_destination()
            .unwrap();
        assert_eq!(
            pr,
            ChatDestination::Github {
                owner: "0xSplits".to_owned(),
                repo: "centaur".to_owned(),
                number: 704,
                kind: GithubThreadKind::Pr,
                review_comment_id: None,
            }
        );
        assert_eq!(pr.platform(), "github");

        let issue = ThreadKey::parse("github:0xSplits/centaur:issue:12")
            .unwrap()
            .chat_destination()
            .unwrap();
        assert_eq!(
            issue,
            ChatDestination::Github {
                owner: "0xSplits".to_owned(),
                repo: "centaur".to_owned(),
                number: 12,
                kind: GithubThreadKind::Issue,
                review_comment_id: None,
            }
        );

        let review_comment = ThreadKey::parse("github:0xSplits/centaur:704:rc:99")
            .unwrap()
            .chat_destination()
            .unwrap();
        assert_eq!(
            review_comment,
            ChatDestination::Github {
                owner: "0xSplits".to_owned(),
                repo: "centaur".to_owned(),
                number: 704,
                kind: GithubThreadKind::Pr,
                review_comment_id: Some(99),
            }
        );
    }

    #[test]
    fn chat_destination_is_none_for_unaddressable_keys() {
        // No channel id → not a postable Discord destination.
        assert!(
            ThreadKey::parse("discord:111")
                .unwrap()
                .chat_destination()
                .is_none()
        );
        // A Linear key with an empty issue id, or an unrecognized shape, is not
        // addressable.
        assert!(
            ThreadKey::parse("linear::c:CMT")
                .unwrap()
                .chat_destination()
                .is_none()
        );
        assert!(
            ThreadKey::parse("linear:ISSUE:x:Y")
                .unwrap()
                .chat_destination()
                .is_none()
        );
        // A GitHub key without a numeric thread number, or with an unrecognized
        // shape, is not addressable — and githubbot's synthetic review sessions
        // deliberately stay unaddressable, matching its own parser.
        assert!(
            ThreadKey::parse("github:0xSplits/centaur:abc")
                .unwrap()
                .chat_destination()
                .is_none()
        );
        assert!(
            ThreadKey::parse("github:no-repo-part:704")
                .unwrap()
                .chat_destination()
                .is_none()
        );
        assert!(
            ThreadKey::parse("github-review:0xSplits/centaur:704")
                .unwrap()
                .chat_destination()
                .is_none()
        );
        // Non-platform namespaces resolve to nothing.
        assert!(
            ThreadKey::parse("api:abc123")
                .unwrap()
                .chat_destination()
                .is_none()
        );
    }

    #[test]
    fn chat_destination_renders_a_platform_context_line() {
        let slack = ThreadKey::parse("slack:C123:123.456")
            .unwrap()
            .chat_destination()
            .unwrap()
            .context_line();
        assert!(slack.contains("Slack"));
        assert!(slack.contains("C123"));
        assert!(slack.contains("slack upload"));

        let discord = ThreadKey::parse("discord:111:222:333")
            .unwrap()
            .chat_destination()
            .unwrap()
            .context_line();
        assert!(discord.contains("Discord"));
        assert!(discord.contains("222"));
        assert!(discord.contains("discord upload"));

        let linear = ThreadKey::parse("linear:ISSUE:c:CMT")
            .unwrap()
            .chat_destination()
            .unwrap()
            .context_line();
        assert!(linear.contains("Linear"));
        assert!(linear.contains("ISSUE"));
        assert!(linear.contains("comment CMT"));
        // Linear has no upload command, so the line must not promise a
        // `linear upload` analog of the Slack/Discord upload tools.
        assert!(!linear.contains("linear upload"));

        let github = ThreadKey::parse("github:0xSplits/centaur:704:rc:99")
            .unwrap()
            .chat_destination()
            .unwrap()
            .context_line();
        assert!(github.contains("GitHub"));
        assert!(github.contains("pull request 0xSplits/centaur#704"));
        assert!(github.contains("review comment 99"));
        // GitHub has no upload command either.
        assert!(!github.contains("github upload"));

        let github_issue = ThreadKey::parse("github:0xSplits/centaur:issue:12")
            .unwrap()
            .chat_destination()
            .unwrap()
            .context_line();
        assert!(github_issue.contains("issue 0xSplits/centaur#12"));
    }

    #[test]
    fn thread_key_accepts_namespaced_values() {
        let key = ThreadKey::parse("chat:C123:1780000000.000000").unwrap();
        assert_eq!(key.as_str(), "chat:C123:1780000000.000000");
    }

    #[test]
    fn thread_key_rejects_missing_namespace() {
        let err = ThreadKey::parse("not-namespaced").unwrap_err();
        assert_eq!(
            err.to_string(),
            "thread_key must be namespaced as '<source>:<id>'"
        );
    }

    #[test]
    fn thread_key_rejects_unbounded_payload_shape() {
        let err = ThreadKey::parse("{\"thread\":\"x\"}").unwrap_err();
        assert_eq!(err.to_string(), "thread_key must not be raw JSON");
    }

    #[test]
    fn harness_type_accepts_supported_values() {
        assert_eq!(HarnessType::from_str("codex").unwrap(), HarnessType::Codex);
        assert_eq!(HarnessType::from_str("amp").unwrap(), HarnessType::Amp);
        assert_eq!(
            HarnessType::from_str("nanocodex").unwrap(),
            HarnessType::Nanocodex
        );
        assert_eq!(
            HarnessType::from_str("claudecode").unwrap(),
            HarnessType::ClaudeCode
        );
    }

    #[test]
    fn harness_type_serializes_as_wire_value() {
        assert_eq!(
            serde_json::to_value(HarnessType::ClaudeCode).unwrap(),
            serde_json::json!("claudecode")
        );
        assert_eq!(
            serde_json::from_value::<HarnessType>(serde_json::json!("codex")).unwrap(),
            HarnessType::Codex
        );
    }

    #[test]
    fn harness_type_rejects_unsupported_values() {
        assert!(HarnessType::from_str("claude-code").is_err());
    }
}
