//! Typed request bodies for the principals area.
//!
//! Each struct is what the matching endpoint deserializes, so every field is
//! either its declared JSON type or a validation error. There is no implicit
//! casting: a string is not a boolean, a number is not a string, and an unknown
//! field is ignored.
//!
//! `Option<Option<T>>` keeps the distinction between an omitted key (keep the
//! stored value) and an explicit `null` (clear it). Fields that were silently
//! dropped when they were not hashes (`labels`) stay a plain `Option`.
//!
//! The field rules are `garde` attributes here. The write paths validate a
//! request built from the merged draft, because a PATCH validates the record it
//! will store rather than the keys the body happened to carry.

use serde::{Deserialize, Deserializer};
use serde_json::{Map, Value};

use super::rules;
use super::{PRINCIPAL_RESERVED_PREFIX, ROLE_RESERVED_PREFIX};
use crate::areas::secrets::SecretKind;

/// Deserializes a present key as `Some(value)`, with `null` becoming
/// `Some(None)`, so an omitted key stays `None`.
pub fn present<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

/// One Slack channel permission row.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct PermissionWrite {
    #[serde(default)]
    pub channel_id: Option<String>,
    #[serde(default)]
    pub upload_enabled: Option<bool>,
    #[serde(default)]
    pub download_enabled: Option<bool>,
    #[serde(default)]
    pub history_enabled: Option<bool>,
}

impl PermissionWrite {
    /// The normalized channel id: trimmed and upper-cased.
    pub fn channel_id(&self) -> String {
        self.channel_id
            .as_deref()
            .unwrap_or_default()
            .trim()
            .to_uppercase()
    }

    /// The supplied flag, if the row carried it.
    pub fn flag(&self, attribute: &str) -> Option<bool> {
        match attribute {
            "upload_enabled" => self.upload_enabled,
            "download_enabled" => self.download_enabled,
            "history_enabled" => self.history_enabled,
            _ => None,
        }
    }
}

/// Body of `POST /principals` and `PUT|PATCH /principals/:id`.
#[derive(Clone, Debug, Default, Deserialize, garde::Validate)]
#[garde(allow_unvalidated)]
pub struct PrincipalWrite {
    #[serde(default)]
    #[garde(custom(rules::foreign_id(PRINCIPAL_RESERVED_PREFIX)))]
    pub foreign_id: Option<String>,
    #[serde(default, deserialize_with = "present")]
    pub name: Option<Option<String>>,
    #[serde(default, deserialize_with = "present")]
    #[garde(custom(rules::kind))]
    pub kind: Option<Option<String>>,
    #[serde(default)]
    pub labels: Option<Map<String, Value>>,
    #[serde(default, deserialize_with = "present")]
    #[garde(custom(rules::formatted(rules::SLACK_USER_ID_PATTERN)))]
    pub slack_user_id: Option<Option<String>>,
    #[serde(default, deserialize_with = "present")]
    #[garde(custom(rules::formatted(rules::SLACK_CHANNEL_ID_PATTERN)))]
    pub slack_channel_id: Option<Option<String>>,
    #[serde(default, deserialize_with = "present")]
    #[garde(custom(rules::formatted(rules::SLACK_TEAM_ID_PATTERN)))]
    pub slack_team_id: Option<Option<String>>,
    #[serde(default, deserialize_with = "present")]
    #[garde(custom(rules::formatted(rules::EMAIL_PATTERN)))]
    pub slack_email: Option<Option<String>>,
    #[serde(default, deserialize_with = "present")]
    pub console_user_id: Option<Option<i64>>,
    #[serde(default, deserialize_with = "present")]
    #[garde(custom(rules::repo_cache))]
    pub sandbox_repo_cache: Option<Option<String>>,
    #[serde(default, deserialize_with = "present")]
    pub sandbox_observability_enabled: Option<Option<bool>>,
    #[serde(default, deserialize_with = "present")]
    pub sandbox_sessions_read_enabled: Option<Option<bool>>,
    #[serde(default, deserialize_with = "present")]
    pub sandbox_workflows_read_enabled: Option<Option<bool>>,
    #[serde(default, deserialize_with = "present")]
    pub sandbox_workflows_write_enabled: Option<Option<bool>>,
    /// Present (even as `null`) replaces the permission set; absent leaves it.
    #[serde(default, deserialize_with = "present")]
    pub slack_channel_permissions: Option<Option<Vec<PermissionWrite>>>,
}

/// Body of `POST /roles` and `PUT|PATCH /roles/:id`.
#[derive(Clone, Debug, Default, Deserialize, garde::Validate)]
#[garde(allow_unvalidated)]
pub struct RoleWrite {
    #[serde(default)]
    #[garde(custom(rules::foreign_id(ROLE_RESERVED_PREFIX)))]
    pub foreign_id: Option<String>,
    #[serde(default, deserialize_with = "present")]
    pub name: Option<Option<String>>,
    #[serde(default)]
    pub labels: Option<Map<String, Value>>,
    #[serde(default, deserialize_with = "present")]
    pub slack_channel_permissions: Option<Option<Vec<PermissionWrite>>>,
}

/// Body of `PUT /principals|roles/:id/slack_channel_permissions`.
///
/// An absent or null list is an empty replacement.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct PermissionSetWrite {
    #[serde(default)]
    pub slack_channel_permissions: Option<Vec<PermissionWrite>>,
}

impl PermissionSetWrite {
    pub fn rows(&self) -> &[PermissionWrite] {
        self.slack_channel_permissions
            .as_deref()
            .unwrap_or_default()
    }
}

/// Body of `POST /principals/:principal_id/roles`.
#[derive(Clone, Debug, Deserialize)]
pub struct AssignRole {
    pub role_id: String,
}

/// Body of `POST /grants`.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct GrantWrite {
    #[serde(default)]
    pub principal_id: Option<String>,
    #[serde(default)]
    pub role_id: Option<String>,
    #[serde(default)]
    pub static_secret_id: Option<String>,
    #[serde(default)]
    pub gcp_auth_secret_id: Option<String>,
    #[serde(default)]
    pub gcp_id_token_secret_id: Option<String>,
    #[serde(default)]
    pub aws_auth_secret_id: Option<String>,
    #[serde(default)]
    pub oauth_token_secret_id: Option<String>,
    #[serde(default)]
    pub pg_dsn_secret_id: Option<String>,
    #[serde(default)]
    pub hmac_secret_id: Option<String>,
    #[serde(default)]
    pub ssh_key_secret_id: Option<String>,
}

impl GrantWrite {
    /// A trimmed, non-blank value.
    fn present(value: &Option<String>) -> Option<&str> {
        value
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
    }

    /// The grantee reference: its `grants` column and the oid it names.
    /// Principals are resolved before roles.
    pub fn grantee(&self) -> Option<(&'static str, &str)> {
        if let Some(oid) = Self::present(&self.principal_id) {
            return Some(("principal_id", oid));
        }
        Self::present(&self.role_id).map(|oid| ("role_id", oid))
    }

    /// The non-blank reference of one grantable kind.
    pub fn grantable(&self, kind: SecretKind) -> Option<&str> {
        Self::present(match kind {
            SecretKind::Static => &self.static_secret_id,
            SecretKind::GcpAuth => &self.gcp_auth_secret_id,
            SecretKind::GcpIdToken => &self.gcp_id_token_secret_id,
            SecretKind::AwsAuth => &self.aws_auth_secret_id,
            SecretKind::OauthToken => &self.oauth_token_secret_id,
            SecretKind::PgDsn => &self.pg_dsn_secret_id,
            SecretKind::Hmac => &self.hmac_secret_id,
            SecretKind::SshKey => &self.ssh_key_secret_id,
        })
    }
}
