//! The iron-control client api-rs registers principals, roles, secrets,
//! grants, and proxies with.
//!
//! Every call runs in-process against the Console database through the same
//! `centaur_console_db` functions the Console handlers use, so validation,
//! reconciliation, cache invalidation, and timestamps match the Console API.
//! Writes are attributed to the Console system user
//! (`centaur_console_db::SYSTEM_USER_EMAIL`), not to any operator.

use sqlx::PgPool;

use crate::database;
use crate::error::Result;
use crate::in_process;
use crate::models::{
    AwsAuthSecretInput, EffectiveConfig, GcpAuthSecretInput, GcpIdTokenSecretInput, Grant,
    GrantSecret, Grantee, HmacSecretInput, IdentityInput, OAuthTokenSecretInput, PgDsnSecretInput,
    Principal, PrincipalInput, Proxy, Role, SecretRecord, SlackChannelPermissionInput,
    StaticSecretInput,
};
use crate::registry::RoleSecretWriter;

/// Registers Centaur's Console model through the Console database.
#[derive(Clone, Debug)]
pub struct IronControlClient {
    pool: PgPool,
}

impl IronControlClient {
    /// A client writing through `pool`, a writable Console database pool.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    // ----- principals & roles ---------------------------------------------

    /// Upsert a principal by ``foreign_id`` (create if absent, update if not).
    pub async fn upsert_principal(&self, input: &PrincipalInput) -> Result<Principal> {
        in_process::upsert_principal(&self.pool, input).await
    }

    /// Upsert a principal unless Console already stores exactly `input` for it.
    pub async fn refresh_principal(&self, input: &PrincipalInput) -> Result<Principal> {
        if let Some(principal) = database::unchanged_principal(&self.pool, input).await? {
            return Ok(principal);
        }
        self.upsert_principal(input).await
    }

    /// Upsert a role by ``foreign_id``.
    pub async fn upsert_role(&self, input: &IdentityInput) -> Result<Role> {
        in_process::upsert_role(&self.pool, input).await
    }

    /// The principal's effective config: the secrets and Postgres listeners
    /// its proxy would sync. Accepts the principal OID (``prn_…``) or a
    /// ``foreign_id``.
    pub async fn effective_config(&self, principal: &str) -> Result<EffectiveConfig> {
        database::effective_config(&self.pool, principal).await
    }

    /// Fetch a principal by OID (``prn_…``) or ``foreign_id``. Never creates it.
    pub async fn get_principal(&self, principal: &str) -> Result<Principal> {
        database::principal(&self.pool, principal).await
    }

    /// Assign a role (by OID) to a principal (by OID).
    pub async fn assign_role(&self, principal_id: &str, role_id: &str) -> Result<()> {
        in_process::assign_role(&self.pool, principal_id, role_id).await
    }

    /// Create or update one Slack channel permission row on a principal without
    /// replacing that principal's other Slack permissions.
    pub async fn upsert_slack_channel_permission(
        &self,
        principal_id: &str,
        input: &SlackChannelPermissionInput,
    ) -> Result<()> {
        in_process::upsert_slack_channel_permission(&self.pool, principal_id, input).await
    }

    /// Upsert a Slack channel permission unless the principal already holds
    /// exactly `input` directly.
    pub async fn refresh_slack_channel_permission(
        &self,
        principal_id: &str,
        input: &SlackChannelPermissionInput,
    ) -> Result<()> {
        if database::slack_channel_permission_is_current(&self.pool, principal_id, input).await? {
            return Ok(());
        }
        self.upsert_slack_channel_permission(principal_id, input)
            .await
    }

    // ----- secrets and grants ---------------------------------------------

    /// Upsert a static secret by ``foreign_id``.
    pub async fn upsert_static_secret(&self, input: &StaticSecretInput) -> Result<SecretRecord> {
        in_process::upsert_static_secret(&self.pool, input).await
    }

    /// Upsert an OAuth token secret by ``foreign_id``.
    pub async fn upsert_oauth_token_secret(
        &self,
        input: &OAuthTokenSecretInput,
    ) -> Result<SecretRecord> {
        in_process::upsert_oauth_token_secret(&self.pool, input).await
    }

    /// Upsert a GCP auth secret by ``foreign_id``, or create one without.
    pub async fn upsert_gcp_auth_secret(&self, input: &GcpAuthSecretInput) -> Result<SecretRecord> {
        in_process::upsert_gcp_auth_secret(&self.pool, input).await
    }

    /// Upsert a GCP ID token secret by ``foreign_id``.
    pub async fn upsert_gcp_id_token_secret(
        &self,
        input: &GcpIdTokenSecretInput,
    ) -> Result<SecretRecord> {
        in_process::upsert_gcp_id_token_secret(&self.pool, input).await
    }

    /// Upsert a Postgres DSN secret by ``foreign_id``.
    pub async fn upsert_pg_dsn_secret(&self, input: &PgDsnSecretInput) -> Result<SecretRecord> {
        in_process::upsert_pg_dsn_secret(&self.pool, input).await
    }

    /// Upsert an HMAC signing secret by ``foreign_id``.
    pub async fn upsert_hmac_secret(&self, input: &HmacSecretInput) -> Result<SecretRecord> {
        in_process::upsert_hmac_secret(&self.pool, input).await
    }

    /// Upsert an AWS auth secret by ``foreign_id``.
    pub async fn upsert_aws_auth_secret(&self, input: &AwsAuthSecretInput) -> Result<SecretRecord> {
        in_process::upsert_aws_auth_secret(&self.pool, input).await
    }

    /// Attach a secret to a grantee (principal or role).
    pub async fn create_grant(&self, grantee: &Grantee, secret: &GrantSecret) -> Result<Grant> {
        in_process::create_grant(&self.pool, grantee, secret).await
    }

    /// The grants attached to a role (by OID).
    pub async fn list_role_grants(&self, role: &str) -> Result<Vec<Grant>> {
        in_process::list_role_grants(&self.pool, role).await
    }

    // ----- proxies ---------------------------------------------------------

    /// Register a proxy owned by ``principal_id``, optionally bound to the
    /// requesting user's principal for the current turn. The returned
    /// [`Proxy::token`] is the plaintext ``iprx_`` bearer and is only
    /// available here.
    pub async fn create_proxy(
        &self,
        name: impl Into<String>,
        principal_id: impl Into<String>,
        requester_principal_id: Option<String>,
        labels: std::collections::BTreeMap<String, String>,
    ) -> Result<Proxy> {
        in_process::create_proxy(
            &self.pool,
            &name.into(),
            &principal_id.into(),
            requester_principal_id,
            &labels,
        )
        .await
    }

    /// Reassign a proxy to a different principal. The ``iprx_`` token is
    /// unchanged; the proxy picks up the new principal's grants on its next
    /// sync from proxy-sync. This is how a warm-pool proxy, booted under a
    /// bootstrap principal, is bound to a session's principal at checkout
    /// without a restart or token swap. The requester principal is re-bound
    /// (or cleared) on every assignment.
    pub async fn assign_proxy_principal(
        &self,
        id: &str,
        principal_id: &str,
        requester_principal_id: Option<&str>,
        labels: &std::collections::BTreeMap<String, String>,
    ) -> Result<Proxy> {
        in_process::assign_proxy_principal(
            &self.pool,
            id,
            principal_id,
            requester_principal_id,
            labels,
        )
        .await
    }

    /// Deregister a proxy by OID.
    pub async fn delete_proxy(&self, id: &str) -> Result<()> {
        in_process::delete_proxy(&self.pool, id).await
    }
}

impl RoleSecretWriter for IronControlClient {
    type Error = crate::IronControlError;

    async fn list_role_grants(&self, role: &str) -> Result<Vec<Grant>> {
        IronControlClient::list_role_grants(self, role).await
    }
    async fn upsert_static_secret(&self, input: &StaticSecretInput) -> Result<SecretRecord> {
        IronControlClient::upsert_static_secret(self, input).await
    }
    async fn upsert_oauth_token_secret(
        &self,
        input: &OAuthTokenSecretInput,
    ) -> Result<SecretRecord> {
        IronControlClient::upsert_oauth_token_secret(self, input).await
    }
    async fn upsert_gcp_auth_secret(&self, input: &GcpAuthSecretInput) -> Result<SecretRecord> {
        IronControlClient::upsert_gcp_auth_secret(self, input).await
    }
    async fn upsert_gcp_id_token_secret(
        &self,
        input: &GcpIdTokenSecretInput,
    ) -> Result<SecretRecord> {
        IronControlClient::upsert_gcp_id_token_secret(self, input).await
    }
    async fn upsert_pg_dsn_secret(&self, input: &PgDsnSecretInput) -> Result<SecretRecord> {
        IronControlClient::upsert_pg_dsn_secret(self, input).await
    }
    async fn upsert_hmac_secret(&self, input: &HmacSecretInput) -> Result<SecretRecord> {
        IronControlClient::upsert_hmac_secret(self, input).await
    }
    async fn upsert_aws_auth_secret(&self, input: &AwsAuthSecretInput) -> Result<SecretRecord> {
        IronControlClient::upsert_aws_auth_secret(self, input).await
    }
    async fn create_grant(&self, grantee: &Grantee, secret: &GrantSecret) -> Result<Grant> {
        IronControlClient::create_grant(self, grantee, secret).await
    }
}
