//! Per-session principal registration.
//!
//! Roles are registered once at startup (see [`crate::register_role`]); a
//! When a session starts, [`SessionRegistrar`] upserts the session's principal.
//! Iron-control owns default role assignment for brand-new principals, while
//! existing principals keep their current assignments so operator revocations
//! in console or ``centaur-perms`` remain sticky. The principal is derived from
//! the thread key (see [`crate::derive_principal`]).

use serde_json::Value;

use crate::IronControlClient;
use crate::error::{IronControlError, Result};
use crate::models::{Principal, PrincipalInput, SlackChannelPermissionInput};
use crate::principal::{
    PrincipalRef, derive_github_requester_principal, derive_principal_with_slack_team,
    derive_slack_requester_principal, is_direct_message, slack_conversation_id,
};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct SessionPrincipalMetadata<'a> {
    actor_user_id: Option<&'a str>,
    slack_team_id: Option<&'a str>,
    slack_user_email: Option<&'a str>,
    conversation_name: Option<&'a str>,
}

impl<'a> SessionPrincipalMetadata<'a> {
    fn from_session_metadata(metadata: Option<&'a Value>) -> Self {
        let Some(metadata) = metadata else {
            return Self::default();
        };
        Self {
            actor_user_id: metadata
                .get("slack_user_id")
                .or_else(|| metadata.get("aad_object_id"))
                .or_else(|| metadata.get("user_id"))
                .and_then(Value::as_str),
            slack_team_id: metadata.get("slack_team_id").and_then(Value::as_str),
            slack_user_email: metadata.get("slack_user_email").and_then(Value::as_str),
            conversation_name: metadata
                .get("slack_conversation_name")
                .or_else(|| metadata.get("discord_conversation_name"))
                .or_else(|| metadata.get("linear_conversation_name"))
                .or_else(|| metadata.get("teams_conversation_name"))
                .and_then(Value::as_str),
        }
    }
}

/// Registers a session's principal against iron-control at session start.
///
/// Cheap to clone (the inner [`IronControlClient`] shares a connection pool),
/// so it can live on a shared runtime handle.
#[derive(Clone, Debug)]
pub struct SessionRegistrar {
    client: IronControlClient,
}

impl SessionRegistrar {
    pub fn new(client: IronControlClient) -> Self {
        Self { client }
    }

    /// Upsert the principal for ``thread_key`` using the session metadata the
    /// ingress supplied. Returns the upserted principal record (its ``id`` is
    /// the OID) so callers can bind the session's egress proxy to the same
    /// identity.
    ///
    /// Re-registering an existing channel/user refreshes identity metadata but
    /// leaves its role assignments to iron-control.
    pub async fn register_session(
        &self,
        thread_key: &str,
        metadata: Option<&Value>,
    ) -> Result<Principal> {
        let metadata = SessionPrincipalMetadata::from_session_metadata(metadata);
        let principal = derive_principal_with_slack_team(
            thread_key,
            metadata.actor_user_id,
            metadata.slack_team_id,
            metadata.conversation_name,
        )?;
        let mut input = principal.to_principal_input();
        apply_slack_dm_email(thread_key, metadata.slack_user_email, &mut input);
        let exists = self.merge_existing_labels(&mut input).await?;
        let slack_permission = slack_permission_for_thread(
            thread_key,
            input.slack_channel_id.as_deref(),
            input.slack_user_id.as_deref(),
        );
        let should_upsert_slack_permission = !exists
            || slack_permission
                .as_ref()
                .is_some_and(|permission| is_direct_message(Some(&permission.channel_id)));
        let record = self.client.upsert_principal(&input).await?;
        if should_upsert_slack_permission && let Some(permission) = slack_permission {
            self.client
                .upsert_slack_channel_permission(&record.id, &permission)
                .await?;
        }
        Ok(record)
    }

    /// Bind the principal of the human requesting this turn, resolved from the
    /// execute metadata (see [`requester_plan`]): fetched for authenticated
    /// Console executions and upserted for Slack channel and GitHub turns.
    /// Returns ``Ok(None)`` when the metadata carries no eligible requester.
    /// For Slack that includes DM threads (the conversation principal already
    /// is the user's) and requesters not proven to belong to the Slack app's
    /// home team, which prevents Slack Connect users from supplying requester
    /// credentials to a shared channel turn.
    ///
    /// Unlike [`Self::register_session`], this never writes Slack channel
    /// permissions: the requester principal only scopes proxy credentials, and
    /// the conversation principal already owns the thread's Slack permission.
    /// Roles are left to iron-control's default assignment, as for sessions.
    pub async fn register_requester(
        &self,
        thread_key: &str,
        metadata: Option<&Value>,
    ) -> Result<Option<Principal>> {
        let Some(metadata) = metadata else {
            return Ok(None);
        };
        match requester_plan(thread_key, metadata) {
            None => Ok(None),
            // The console owns console-user principals: fetch, never upsert.
            Some(RequesterPlan::FetchExisting(foreign_id)) => {
                self.client.get_principal(&foreign_id).await.map(Some)
            }
            Some(RequesterPlan::UpsertDerived(principal)) => {
                let mut input = principal.to_principal_input();
                set_slack_email(
                    &mut input,
                    metadata.get("slack_user_email").and_then(Value::as_str),
                );
                self.merge_existing_labels(&mut input).await?;
                Ok(Some(self.client.upsert_principal(&input).await?))
            }
        }
    }

    pub async fn get_principal(&self, principal: &str) -> Result<Principal> {
        self.client.get_principal(principal).await
    }

    /// Fold an existing principal's labels under the freshly derived ones so
    /// labels an operator or the console added survive re-registration.
    /// Returns whether the principal already existed.
    async fn merge_existing_labels(&self, input: &mut PrincipalInput) -> Result<bool> {
        let existing = match self.client.get_principal(&input.foreign_id).await {
            Ok(existing) => Some(existing),
            Err(error) if is_status(&error, 404) => None,
            Err(error) => return Err(error),
        };
        let Some(existing) = existing else {
            return Ok(false);
        };
        let mut labels = existing.labels;
        labels.extend(std::mem::take(&mut input.labels));
        input.labels = labels;
        Ok(true)
    }
}

/// How a turn's requester principal is resolved from the execute metadata.
/// Adding a source is one arm here, not another branch in the registrar.
#[derive(Debug)]
enum RequesterPlan {
    /// Fetch a principal the console service provisioned for its
    /// authenticated user. The API server strips this metadata field from
    /// every caller except the authenticated Console service, so checking the
    /// field also covers Console replies to non-Console threads.
    FetchExisting(String),
    /// Upsert the api-rs-owned per-user principal derived from the ingress's
    /// verified actor identity (Slack channel turns, GitHub turns).
    UpsertDerived(PrincipalRef),
}

fn requester_plan(thread_key: &str, metadata: &Value) -> Option<RequesterPlan> {
    if metadata.get("requester_principal_foreign_id").is_some() {
        let foreign_id = metadata
            .get("requester_principal_foreign_id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|foreign_id| !foreign_id.is_empty())?;
        return Some(RequesterPlan::FetchExisting(foreign_id.to_owned()));
    }
    // githubbot forwards the comment author (`user_id`, `user_name`) from the
    // signature-verified webhook payload. Let the derivation helper recognize
    // every GitHub-owned session family, including work sessions.
    if let Some(principal) = metadata
        .get("user_id")
        .and_then(Value::as_str)
        .and_then(|user_id| {
            derive_github_requester_principal(
                thread_key,
                user_id,
                metadata.get("user_name").and_then(Value::as_str),
            )
        })
    {
        return Some(RequesterPlan::UpsertDerived(principal));
    }
    let slack_team_id = eligible_slack_requester_team(metadata)?;
    let slack_user_id = metadata.get("slack_user_id").and_then(Value::as_str)?;
    derive_slack_requester_principal(
        thread_key,
        slack_user_id,
        slack_team_id,
        metadata.get("slack_display_name").and_then(Value::as_str),
    )
    .map(RequesterPlan::UpsertDerived)
}

fn eligible_slack_requester_team(metadata: &Value) -> Option<&str> {
    let requester_team = metadata
        .get("slack_team_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|team| !team.is_empty())?;
    let home_team = metadata
        .get("slack_home_team_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|team| !team.is_empty())?;
    (requester_team == home_team).then_some(requester_team)
}

fn slack_permission_for_thread(
    thread_key: &str,
    slack_channel_id: Option<&str>,
    slack_user_id: Option<&str>,
) -> Option<SlackChannelPermissionInput> {
    if let Some(channel_id) = slack_channel_id {
        let channel_id = channel_id.trim();
        return (!is_direct_message(Some(channel_id)))
            .then(|| slack_permission(channel_id.to_owned()));
    }

    slack_user_id?;
    let conversation_id = slack_conversation_id(thread_key)?;
    is_direct_message(Some(conversation_id)).then(|| slack_permission(conversation_id.to_owned()))
}

fn apply_slack_dm_email(
    thread_key: &str,
    slack_user_email: Option<&str>,
    input: &mut PrincipalInput,
) {
    let Some(conversation_id) = slack_conversation_id(thread_key) else {
        return;
    };
    if is_direct_message(Some(conversation_id)) {
        set_slack_email(input, slack_user_email);
    }
}

/// Stamp ``slack_email`` on a user principal, skipping blank emails. Shared by
/// the DM session path and the channel requester path so a user carries the
/// same identity either way.
fn set_slack_email(input: &mut PrincipalInput, slack_user_email: Option<&str>) {
    let Some(email) = slack_user_email
        .map(str::trim)
        .filter(|email| !email.is_empty())
    else {
        return;
    };
    if input.slack_user_id.is_some() {
        input.slack_email = Some(email.to_owned());
    }
}

fn slack_permission(channel_id: String) -> SlackChannelPermissionInput {
    SlackChannelPermissionInput {
        channel_id,
        upload_enabled: true,
        download_enabled: true,
        history_enabled: true,
    }
}

fn is_status(err: &IronControlError, code: u16) -> bool {
    matches!(err, IronControlError::Status { status, .. } if *status == code)
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use crate::{PrincipalDerivationError, derive_principal};
    use serde_json::json;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;

    #[test]
    fn session_principal_metadata_prefers_slack_user_then_teams_ids() {
        assert_eq!(
            SessionPrincipalMetadata::from_session_metadata(Some(&json!({
                "slack_user_id": "U1",
                "aad_object_id": "aad-user-1",
                "user_id": "teams-user-1"
            })))
            .actor_user_id,
            Some("U1")
        );
        assert_eq!(
            SessionPrincipalMetadata::from_session_metadata(Some(&json!({
                "aad_object_id": "aad-user-1",
                "user_id": "teams-user-1"
            })))
            .actor_user_id,
            Some("aad-user-1")
        );
        assert_eq!(
            SessionPrincipalMetadata::from_session_metadata(Some(&json!({
                "user_id": "teams-user-1"
            })))
            .actor_user_id,
            Some("teams-user-1")
        );
    }

    #[test]
    fn session_principal_metadata_accepts_teams_name() {
        assert_eq!(
            SessionPrincipalMetadata::from_session_metadata(Some(&json!({
                "teams_conversation_name": "Casey Harper"
            })))
            .conversation_name,
            Some("Casey Harper")
        );
    }

    #[test]
    fn session_principal_metadata_carries_slack_team_id() {
        assert_eq!(
            SessionPrincipalMetadata::from_session_metadata(Some(&json!({
                "slack_team_id": "T123"
            })))
            .slack_team_id,
            Some("T123")
        );
    }

    #[test]
    fn session_principal_metadata_carries_slack_user_email() {
        assert_eq!(
            SessionPrincipalMetadata::from_session_metadata(Some(&json!({
                "slack_user_email": "ada@example.com"
            })))
            .slack_user_email,
            Some("ada@example.com")
        );
    }

    #[test]
    fn slack_dm_email_applies_only_to_dm_user_principals() {
        let mut dm_input = derive_principal("slack:T123:D123:ts", Some("U123"), None)
            .expect("DM principal should be derivable")
            .to_principal_input();
        apply_slack_dm_email(
            "slack:T123:D123:1773364194.179929",
            Some(" ada@example.com "),
            &mut dm_input,
        );
        assert_eq!(dm_input.slack_email.as_deref(), Some("ada@example.com"));

        let mut channel_input = derive_principal("slack:T123:C123:ts", Some("U123"), None)
            .expect("channel principal should be derivable")
            .to_principal_input();
        apply_slack_dm_email(
            "slack:T123:C123:1773364194.179929",
            Some("ada@example.com"),
            &mut channel_input,
        );
        assert_eq!(channel_input.slack_email, None);
    }

    #[tokio::test]
    async fn register_session_rejects_slack_dm_without_team_id() {
        let registrar =
            SessionRegistrar::new(IronControlClient::new("http://127.0.0.1:1", "test-key"));
        let metadata = json!({ "slack_user_id": "U123" });

        let error = registrar
            .register_session("slack:D123:1773364194.179929", Some(&metadata))
            .await
            .unwrap_err();

        assert!(matches!(
            error,
            IronControlError::PrincipalDerivation(PrincipalDerivationError::MissingSlackTeamId)
        ));
    }

    #[tokio::test]
    async fn register_session_rejects_slack_dm_without_user_id() {
        let registrar =
            SessionRegistrar::new(IronControlClient::new("http://127.0.0.1:1", "test-key"));
        let metadata = json!({ "slack_team_id": "T123" });

        let error = registrar
            .register_session("slack:D123:1773364194.179929", Some(&metadata))
            .await
            .unwrap_err();

        assert!(matches!(
            error,
            IronControlError::PrincipalDerivation(PrincipalDerivationError::MissingSlackUserId)
        ));
    }

    #[tokio::test]
    async fn register_session_leaves_default_roles_to_iron_control() {
        let (base_url, requests, _bodies, server) = spawn_iron_control_stub(false).await;
        let registrar = SessionRegistrar::new(IronControlClient::new(base_url, "test-key"));
        let metadata = json!({
            "slack_user_id": "U123",
            "slack_team_id": "T123",
            "slack_conversation_name": "general"
        });

        registrar
            .register_session("slack:T123:C123:1773364194.179929", Some(&metadata))
            .await
            .unwrap();

        let requests = requests.lock().unwrap();
        assert!(
            requests.contains(&"GET /api/v1/principals/lookup/slack-channel-t123-c123".to_owned())
        );
        assert!(requests.contains(&"PUT /api/v1/principals/slack-channel-t123-c123".to_owned()));
        assert!(
            requests.contains(
                &"POST /api/v1/principals/prn_channel/slack_channel_permissions".to_owned()
            )
        );
        assert!(
            !requests
                .iter()
                .any(|request| request == "POST /api/v1/principals/prn_channel/roles"),
            "iron-control assigns configured default roles during principal creation"
        );
        server.abort();
    }

    #[tokio::test]
    async fn register_session_does_not_restore_roles_for_existing_principal() {
        let (base_url, requests, _bodies, server) = spawn_iron_control_stub(true).await;
        let registrar = SessionRegistrar::new(IronControlClient::new(base_url, "test-key"));
        let metadata = json!({
            "slack_user_id": "U123",
            "slack_team_id": "T123",
            "slack_conversation_name": "general"
        });

        registrar
            .register_session("slack:T123:C123:1773364194.179929", Some(&metadata))
            .await
            .unwrap();

        let requests = requests.lock().unwrap();
        assert!(
            requests.contains(&"GET /api/v1/principals/lookup/slack-channel-t123-c123".to_owned())
        );
        assert!(requests.contains(&"PUT /api/v1/principals/slack-channel-t123-c123".to_owned()));
        assert!(
            !requests
                .iter()
                .any(|request| request.ends_with("/slack_channel_permissions")),
            "existing principals must not have Slack permissions reset"
        );
        assert!(
            !requests
                .iter()
                .any(|request| request == "POST /api/v1/principals/prn_channel/roles"),
            "existing principals must not have manually removed roles restored"
        );
        server.abort();
    }

    #[tokio::test]
    async fn register_session_upserts_slack_dm_permission_for_new_user_principal() {
        let (base_url, requests, _bodies, server) = spawn_iron_control_stub(false).await;
        let registrar = SessionRegistrar::new(IronControlClient::new(base_url, "test-key"));
        let metadata = json!({
            "slack_user_id": "U123",
            "slack_team_id": "T123",
            "slack_conversation_name": "Ada Lovelace"
        });

        registrar
            .register_session("slack:T123:D123:1773364194.179929", Some(&metadata))
            .await
            .unwrap();

        let requests = requests.lock().unwrap();
        assert!(requests.contains(&"PUT /api/v1/principals/slack-user-t123-u123".to_owned()));
        assert!(
            requests
                .contains(&"POST /api/v1/principals/prn_user/slack_channel_permissions".to_owned())
        );
        server.abort();
    }

    #[tokio::test]
    async fn register_session_upserts_slack_dm_permission_for_existing_user_principal() {
        let (base_url, requests, _bodies, server) = spawn_iron_control_stub(true).await;
        let registrar = SessionRegistrar::new(IronControlClient::new(base_url, "test-key"));
        let metadata = json!({
            "slack_user_id": "U123",
            "slack_team_id": "T123",
            "slack_conversation_name": "Ada Lovelace"
        });

        registrar
            .register_session("slack:T123:D123:1773364194.179929", Some(&metadata))
            .await
            .unwrap();

        let requests = requests.lock().unwrap();
        assert!(requests.contains(&"PUT /api/v1/principals/slack-user-t123-u123".to_owned()));
        assert!(
            requests
                .contains(&"POST /api/v1/principals/prn_user/slack_channel_permissions".to_owned())
        );
        assert!(
            !requests
                .iter()
                .any(|request| request == "POST /api/v1/principals/prn_user/roles"),
            "existing DM principals must not have manually removed roles restored"
        );
        server.abort();
    }

    #[test]
    fn slack_email_applies_only_to_user_principals_with_non_blank_email() {
        let mut user_input = derive_principal("slack:T123:D123:ts", Some("U123"), None)
            .expect("DM principal should be derivable")
            .to_principal_input();
        set_slack_email(&mut user_input, Some(" ada@example.com "));
        assert_eq!(user_input.slack_email.as_deref(), Some("ada@example.com"));

        let mut blank_input = derive_principal("slack:T123:D123:ts", Some("U123"), None)
            .expect("DM principal should be derivable")
            .to_principal_input();
        set_slack_email(&mut blank_input, Some("   "));
        assert_eq!(blank_input.slack_email, None);

        let mut channel_input = derive_principal("slack:T123:C123:ts", None, None)
            .expect("channel principal should be derivable")
            .to_principal_input();
        set_slack_email(&mut channel_input, Some("ada@example.com"));
        assert_eq!(channel_input.slack_email, None);
    }

    #[tokio::test]
    async fn register_requester_upserts_user_principal_without_roles_or_permissions() {
        let (base_url, requests, _bodies, server) = spawn_iron_control_stub(false).await;
        let registrar = SessionRegistrar::new(IronControlClient::new(base_url, "test-key"));
        let metadata = json!({
            "slack_user_id": "U123",
            "slack_team_id": "T123",
            "slack_home_team_id": "T123",
            "slack_display_name": "Ada Lovelace",
            "slack_user_email": "ada@example.com"
        });

        let principal = registrar
            .register_requester("slack:T123:C123:1773364194.179929", Some(&metadata))
            .await
            .unwrap()
            .expect("channel requester resolves to a principal");
        assert_eq!(principal.id, "prn_user");

        let requests = requests.lock().unwrap();
        assert!(
            requests.contains(&"GET /api/v1/principals/lookup/slack-user-t123-u123".to_owned())
        );
        assert!(requests.contains(&"PUT /api/v1/principals/slack-user-t123-u123".to_owned()));
        assert!(
            !requests
                .iter()
                .any(|request| request.ends_with("/slack_channel_permissions")),
            "requester upserts must not write Slack channel permissions"
        );
        assert!(
            !requests
                .iter()
                .any(|request| request == "POST /api/v1/principals/prn_user/roles"),
            "iron-control owns default role assignment"
        );
        server.abort();
    }

    #[tokio::test]
    async fn register_requester_merges_labels_for_existing_principal() {
        let (base_url, requests, _bodies, server) = spawn_iron_control_stub(true).await;
        let registrar = SessionRegistrar::new(IronControlClient::new(base_url, "test-key"));
        let metadata = json!({
            "slack_user_id": "U123",
            "slack_team_id": "T123",
            "slack_home_team_id": "T123"
        });

        let principal = registrar
            .register_requester("slack:T123:C123:1773364194.179929", Some(&metadata))
            .await
            .unwrap()
            .expect("channel requester resolves to a principal");
        assert_eq!(principal.id, "prn_user");

        let requests = requests.lock().unwrap();
        assert!(requests.contains(&"PUT /api/v1/principals/slack-user-t123-u123".to_owned()));
        assert!(
            !requests
                .iter()
                .any(|request| request.ends_with("/slack_channel_permissions")),
            "existing requester principals must not have Slack permissions reset"
        );
        assert!(
            !requests
                .iter()
                .any(|request| request == "POST /api/v1/principals/prn_user/roles"),
            "existing requester principals must not have removed roles restored"
        );
        server.abort();
    }

    #[tokio::test]
    async fn register_requester_returns_none_for_dm_thread() {
        let (base_url, requests, _bodies, server) = spawn_iron_control_stub(false).await;
        let registrar = SessionRegistrar::new(IronControlClient::new(base_url, "test-key"));
        let metadata = json!({
            "slack_user_id": "U123",
            "slack_team_id": "T123",
            "slack_home_team_id": "T123"
        });

        let principal = registrar
            .register_requester("slack:T123:D123:1773364194.179929", Some(&metadata))
            .await
            .unwrap();

        assert_eq!(principal, None);
        assert!(requests.lock().unwrap().is_empty());
        server.abort();
    }

    #[tokio::test]
    async fn register_requester_returns_none_without_slack_user_id() {
        let (base_url, requests, _bodies, server) = spawn_iron_control_stub(false).await;
        let registrar = SessionRegistrar::new(IronControlClient::new(base_url, "test-key"));
        let metadata = json!({
            "aad_object_id": "aad-user-1",
            "user_id": "teams-user-1",
            "slack_team_id": "T123",
            "slack_home_team_id": "T123"
        });

        let principal = registrar
            .register_requester("slack:T123:C123:1773364194.179929", Some(&metadata))
            .await
            .unwrap();

        assert_eq!(principal, None);
        assert!(requests.lock().unwrap().is_empty());
        server.abort();
    }

    #[tokio::test]
    async fn register_requester_returns_none_for_non_slack_thread() {
        let (base_url, requests, _bodies, server) = spawn_iron_control_stub(false).await;
        let registrar = SessionRegistrar::new(IronControlClient::new(base_url, "test-key"));
        let metadata = json!({
            "slack_user_id": "U123",
            "slack_team_id": "T123",
            "slack_home_team_id": "T123"
        });

        let principal = registrar
            .register_requester("linear:issue-1", Some(&metadata))
            .await
            .unwrap();

        assert_eq!(principal, None);
        assert!(requests.lock().unwrap().is_empty());
        server.abort();
    }

    #[tokio::test]
    async fn register_requester_returns_none_for_external_slack_team() {
        let (base_url, requests, _bodies, server) = spawn_iron_control_stub(false).await;
        let registrar = SessionRegistrar::new(IronControlClient::new(base_url, "test-key"));
        let metadata = json!({
            "slack_user_id": "U123",
            "slack_team_id": "T_EXTERNAL",
            "slack_home_team_id": "T_HOME"
        });

        let principal = registrar
            .register_requester("slack:T_HOME:C123:1773364194.179929", Some(&metadata))
            .await
            .unwrap();

        assert_eq!(principal, None);
        assert!(requests.lock().unwrap().is_empty());
        server.abort();
    }

    #[tokio::test]
    async fn register_requester_returns_none_without_home_team() {
        let (base_url, requests, _bodies, server) = spawn_iron_control_stub(false).await;
        let registrar = SessionRegistrar::new(IronControlClient::new(base_url, "test-key"));
        let metadata = json!({
            "slack_user_id": "U123",
            "slack_team_id": "T123"
        });

        let principal = registrar
            .register_requester("slack:T123:C123:1773364194.179929", Some(&metadata))
            .await
            .unwrap();

        assert_eq!(principal, None);
        assert!(requests.lock().unwrap().is_empty());
        server.abort();
    }

    #[tokio::test]
    async fn register_requester_resolves_console_requester_principal() {
        let (base_url, requests, _bodies, server) = spawn_iron_control_stub(false).await;
        let registrar = SessionRegistrar::new(IronControlClient::new(base_url, "test-key"));
        let metadata = json!({
            "requester_principal_foreign_id": "console-user-ada-example-com-abc123"
        });

        let principal = registrar
            .register_requester(
                "console:9f1b7a3c-2d4e-4f6a-8b0c-1d2e3f4a5b6c",
                Some(&metadata),
            )
            .await
            .unwrap()
            .expect("console requester resolves to the provisioned principal");
        assert_eq!(principal.id, "prn_console_user");

        let requests = requests.lock().unwrap();
        assert_eq!(
            requests.as_slice(),
            ["GET /api/v1/principals/lookup/console-user-ada-example-com-abc123".to_owned()],
            "console requesters are fetched, never upserted"
        );
        server.abort();
    }

    #[tokio::test]
    async fn register_requester_resolves_console_requester_for_slack_thread() {
        let (base_url, requests, _bodies, server) = spawn_iron_control_stub(false).await;
        let registrar = SessionRegistrar::new(IronControlClient::new(base_url, "test-key"));
        let metadata = json!({
            "requester_principal_foreign_id": "console-user-ada-example-com-abc123"
        });

        let principal = registrar
            .register_requester("slack:T123:C123:1773364194.179929", Some(&metadata))
            .await
            .unwrap()
            .expect("console requester resolves independently of the thread namespace");

        assert_eq!(principal.id, "prn_console_user");
        assert_eq!(
            requests.lock().unwrap().as_slice(),
            ["GET /api/v1/principals/lookup/console-user-ada-example-com-abc123".to_owned()]
        );
        server.abort();
    }

    #[tokio::test]
    async fn register_requester_returns_none_for_console_thread_without_foreign_id() {
        let (base_url, requests, _bodies, server) = spawn_iron_control_stub(false).await;
        let registrar = SessionRegistrar::new(IronControlClient::new(base_url, "test-key"));
        let metadata = json!({ "user_email": "ada@example.com" });

        let principal = registrar
            .register_requester(
                "console:9f1b7a3c-2d4e-4f6a-8b0c-1d2e3f4a5b6c",
                Some(&metadata),
            )
            .await
            .unwrap();

        assert_eq!(principal, None);
        assert!(requests.lock().unwrap().is_empty());
        server.abort();
    }

    #[tokio::test]
    async fn register_requester_errors_for_unknown_console_principal() {
        let (base_url, requests, _bodies, server) = spawn_iron_control_stub(false).await;
        let registrar = SessionRegistrar::new(IronControlClient::new(base_url, "test-key"));
        let metadata = json!({
            "requester_principal_foreign_id": "console-user-ghost"
        });

        let result = registrar
            .register_requester(
                "console:9f1b7a3c-2d4e-4f6a-8b0c-1d2e3f4a5b6c",
                Some(&metadata),
            )
            .await;

        assert!(result.is_err());
        assert_eq!(
            requests.lock().unwrap().as_slice(),
            ["GET /api/v1/principals/lookup/console-user-ghost".to_owned()]
        );
        server.abort();
    }

    #[tokio::test]
    async fn register_requester_upserts_github_user_principal_without_roles_or_permissions() {
        let (base_url, requests, bodies, server) = spawn_iron_control_stub(false).await;
        let registrar = SessionRegistrar::new(IronControlClient::new(base_url, "test-key"));
        let metadata = json!({
            "user_id": "90210001",
            "user_name": "ada"
        });

        let principal = registrar
            .register_requester("github:acme/widgets:12", Some(&metadata))
            .await
            .unwrap()
            .expect("github requester resolves to a principal");
        assert_eq!(principal.id, "prn_github_user");

        let bodies = bodies.lock().unwrap();
        let upsert = bodies
            .iter()
            .find(|request| request.starts_with("PUT /api/v1/principals/github-user-90210001"))
            .expect("github requester principal is upserted");
        assert!(upsert.contains(r#""kind":"github_user""#));
        assert!(upsert.contains(r#""name":"GitHub User @ada""#));
        assert!(upsert.contains(r#""github_subject":"90210001""#));

        let requests = requests.lock().unwrap();
        assert!(
            requests.contains(&"GET /api/v1/principals/lookup/github-user-90210001".to_owned())
        );
        assert!(requests.contains(&"PUT /api/v1/principals/github-user-90210001".to_owned()));
        assert!(
            !requests
                .iter()
                .any(|request| request.ends_with("/slack_channel_permissions")),
            "github requester upserts must not write Slack channel permissions"
        );
        assert!(
            !requests
                .iter()
                .any(|request| request == "POST /api/v1/principals/prn_github_user/roles"),
            "iron-control owns default role assignment"
        );
        server.abort();
    }

    #[tokio::test]
    async fn register_requester_returns_none_for_github_thread_without_user_id() {
        let (base_url, requests, _bodies, server) = spawn_iron_control_stub(false).await;
        let registrar = SessionRegistrar::new(IronControlClient::new(base_url, "test-key"));
        let metadata = json!({ "user_name": "ada" });

        let principal = registrar
            .register_requester("github:acme/widgets:12", Some(&metadata))
            .await
            .unwrap();

        assert_eq!(principal, None);
        assert!(requests.lock().unwrap().is_empty());
        server.abort();
    }

    #[tokio::test]
    async fn register_requester_ignores_user_id_outside_github_threads() {
        let (base_url, requests, _bodies, server) = spawn_iron_control_stub(false).await;
        let registrar = SessionRegistrar::new(IronControlClient::new(base_url, "test-key"));
        let metadata = json!({
            "user_id": "90210001",
            "user_name": "ada"
        });

        let principal = registrar
            .register_requester("linear:issue-1", Some(&metadata))
            .await
            .unwrap();

        assert_eq!(principal, None);
        assert!(requests.lock().unwrap().is_empty());
        server.abort();
    }

    #[test]
    fn requester_plan_resolves_github_work_session_keys() {
        let metadata = json!({
            "user_id": "90210001",
            "user_name": "ada"
        });

        for thread_key in [
            "github:acme/widgets:12",
            "github-issue:acme/widgets:12",
            "github-manage:acme/widgets:12",
        ] {
            let Some(RequesterPlan::UpsertDerived(principal)) =
                requester_plan(thread_key, &metadata)
            else {
                panic!("expected a GitHub requester for {thread_key}");
            };
            assert_eq!(principal.foreign_id, "github-user-90210001");
        }
    }

    #[test]
    fn requester_team_eligibility_requires_matching_non_blank_teams() {
        for metadata in [
            json!({"slack_home_team_id": "T123"}),
            json!({"slack_team_id": "T123"}),
            json!({"slack_team_id": "", "slack_home_team_id": "T123"}),
            json!({"slack_team_id": "T123", "slack_home_team_id": "   "}),
            json!({"slack_team_id": "T_EXTERNAL", "slack_home_team_id": "T_HOME"}),
        ] {
            assert_eq!(eligible_slack_requester_team(&metadata), None);
        }

        assert_eq!(
            eligible_slack_requester_team(&json!({
                "slack_team_id": " T123 ",
                "slack_home_team_id": "T123"
            })),
            Some("T123")
        );
    }

    #[test]
    fn slack_permission_for_thread_skips_dm_channel_fallback_without_user() {
        assert_eq!(
            slack_permission_for_thread("slack:D123:ts", Some("D123"), None),
            None
        );
    }

    /// A stub iron-control API. `requests` records `METHOD path` per call;
    /// `bodies` additionally records the JSON body for calls that carry one,
    /// so upserting tests can assert what was written, not just where.
    async fn spawn_iron_control_stub(
        principal_exists: bool,
    ) -> (
        String,
        Arc<Mutex<Vec<String>>>,
        Arc<Mutex<Vec<String>>>,
        tokio::task::JoinHandle<()>,
    ) {
        fn content_length(headers: &str) -> usize {
            headers
                .lines()
                .filter_map(|line| line.split_once(':'))
                .find(|(name, _)| name.trim().eq_ignore_ascii_case("content-length"))
                .and_then(|(_, value)| value.trim().parse().ok())
                .unwrap_or(0)
        }

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let bodies = Arc::new(Mutex::new(Vec::new()));
        let seen = requests.clone();
        let bodies_seen = bodies.clone();
        let handle = tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    return;
                };
                let mut request = Vec::new();
                let mut buf = [0u8; 1024];
                loop {
                    let complete = request
                        .windows(4)
                        .position(|window| window == b"\r\n\r\n")
                        .is_some_and(|headers_end| {
                            let headers = String::from_utf8_lossy(&request[..headers_end]);
                            request.len() >= headers_end + 4 + content_length(&headers)
                        });
                    if complete {
                        break;
                    }
                    match stream.read(&mut buf).await {
                        Ok(0) | Err(_) => break,
                        Ok(read) => request.extend_from_slice(&buf[..read]),
                    }
                }
                let request = String::from_utf8_lossy(&request);
                let mut segments = request.splitn(2, "\r\n\r\n");
                let head = segments.next().unwrap_or_default();
                let request_body = segments.next().unwrap_or_default().trim_end();
                let first_line = head.lines().next().unwrap_or_default();
                let mut parts = first_line.split_whitespace();
                let method = parts.next().unwrap_or_default();
                let path = parts.next().unwrap_or_default();
                seen.lock().unwrap().push(format!("{method} {path}"));
                if !request_body.is_empty() {
                    bodies_seen
                        .lock()
                        .unwrap()
                        .push(format!("{method} {path} {request_body}"));
                }

                let (status_line, body) = match (method, path) {
                    ("GET", "/api/v1/principals/lookup/slack-channel-t123-c123")
                        if principal_exists =>
                    {
                        ("200 OK", channel_principal_body())
                    }
                    ("GET", "/api/v1/principals/lookup/slack-user-t123-u123")
                        if principal_exists =>
                    {
                        ("200 OK", user_principal_body())
                    }
                    ("GET", "/api/v1/principals/lookup/slack-channel-t123-c123")
                    | ("GET", "/api/v1/principals/lookup/slack-user-t123-u123") => {
                        ("404 Not Found", r#"{"error":"not found"}"#.to_owned())
                    }
                    ("PUT", "/api/v1/principals/slack-channel-t123-c123") => {
                        ("200 OK", channel_principal_body())
                    }
                    ("PUT", "/api/v1/principals/slack-user-t123-u123") => {
                        ("200 OK", user_principal_body())
                    }
                    ("GET", "/api/v1/principals/lookup/console-user-ada-example-com-abc123") => {
                        ("200 OK", console_user_principal_body())
                    }
                    ("GET", "/api/v1/principals/lookup/console-user-ghost") => {
                        ("404 Not Found", r#"{"error":"not found"}"#.to_owned())
                    }
                    ("GET", "/api/v1/principals/lookup/github-user-90210001")
                        if principal_exists =>
                    {
                        ("200 OK", github_user_principal_body())
                    }
                    ("GET", "/api/v1/principals/lookup/github-user-90210001") => {
                        ("404 Not Found", r#"{"error":"not found"}"#.to_owned())
                    }
                    ("PUT", "/api/v1/principals/github-user-90210001") => {
                        ("200 OK", github_user_principal_body())
                    }
                    (
                        "POST",
                        "/api/v1/principals/prn_channel/slack_channel_permissions"
                        | "/api/v1/principals/prn_user/slack_channel_permissions",
                    ) => ("200 OK", r#"{"data":{"ok":true}}"#.to_owned()),
                    ("POST", "/api/v1/principals/prn_channel/roles") => {
                        ("200 OK", r#"{"data":{"ok":true}}"#.to_owned())
                    }
                    _ => (
                        "500 Internal Server Error",
                        r#"{"error":"unexpected"}"#.to_owned(),
                    ),
                };
                let response = format!(
                    "HTTP/1.1 {status_line}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len(),
                );
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.shutdown().await;
            }
        });
        (base_url, requests, bodies, handle)
    }

    fn channel_principal_body() -> String {
        r#"{"data":{"id":"prn_channel","foreign_id":"slack-channel-t123-c123","name":"Slack Channel #general","labels":{}}}"#.to_owned()
    }

    fn user_principal_body() -> String {
        r#"{"data":{"id":"prn_user","foreign_id":"slack-user-t123-u123","name":"Slack DM @Ada Lovelace","labels":{}}}"#.to_owned()
    }

    fn console_user_principal_body() -> String {
        r#"{"data":{"id":"prn_console_user","foreign_id":"console-user-ada-example-com-abc123","name":"Ada Lovelace","labels":{}}}"#.to_owned()
    }

    fn github_user_principal_body() -> String {
        r#"{"data":{"id":"prn_github_user","foreign_id":"github-user-90210001","name":"GitHub User @ada","labels":{"github_subject":"90210001"}}}"#.to_owned()
    }
}
