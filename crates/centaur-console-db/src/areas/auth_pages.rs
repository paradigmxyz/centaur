//! Console data access for the auth pages area: SSO user provisioning, MCP OAuth
//! clients, authorization codes, refresh tokens, and the console-user principal
//! that MCP tokens name.
//!
//! The row shapes and validity rules follow Console's `User`, `UserIdentity`,
//! `McpOauthClient`, `McpOauthAuthorizationCode`, `McpOauthRefreshToken` models
//! and the console-user principal provisioner. Tokens are stored only as
//! SHA-256 digests and returned to the caller once, in plaintext.
//!
//! Projections derive [`sqlx::FromRow`] and are read with `query_as`; the MCP
//! client and the console-user principal are addressed through their
//! [`model`] descriptors, and timestamped inserts go through [`write`]. A
//! statement that assigns a server clock to another column (`approved_at`,
//! `consumed_at`, `expires_at`, `last_used_at`, `revoked_at`), merges JSONB, or
//! needs `ON CONFLICT`/a conditional `WHERE` stays plain SQL.

use std::net::IpAddr;

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{FromRow, PgConnection, PgPool};
use url::Url;

use crate::areas::automation::principal_labels;
use crate::invalidate::{self, Scope};
use crate::model::{self, ConsoleModel, McpOauthClient, Principal, PrincipalRole, Role};
use crate::write;
use crate::{ConsoleUser, Result, find_console_user};

/// The default MCP client grant types.
pub const DEFAULT_GRANT_TYPES: [&str; 2] = ["authorization_code", "refresh_token"];
/// The default MCP client response types.
pub const DEFAULT_RESPONSE_TYPES: [&str; 1] = ["code"];
/// The default MCP client scopes.
pub const DEFAULT_SCOPES: [&str; 1] = ["mcp:tools"];

/// Authorization code lifetime: 10 minutes.
pub const AUTHORIZATION_CODE_TTL_SECONDS: i64 = 10 * 60;
/// Refresh token lifetime: 90 days.
pub const REFRESH_TOKEN_TTL_SECONDS: i64 = 90 * 24 * 60 * 60;

const AUTHORIZATION_CODE_PREFIX: &str = "mcpauth_";
const REFRESH_TOKEN_PREFIX: &str = "mcprt_";
const TOKEN_BYTES: usize = 48;

/// `HashedTokenLookup.hash_token`: the SHA-256 hex digest of a token's
/// plaintext.
pub fn hash_token(value: &str) -> String {
    hex::encode(Sha256::digest(value.as_bytes()))
}

/// A fresh `mcpauth_` code, matching `SecureRandom.urlsafe_base64(48)`.
pub fn issue_authorization_code() -> String {
    random_token(AUTHORIZATION_CODE_PREFIX)
}

/// A fresh `mcprt_` refresh token, matching `SecureRandom.urlsafe_base64(48)`.
pub fn issue_refresh_token() -> String {
    random_token(REFRESH_TOKEN_PREFIX)
}

fn random_token(prefix: &str) -> String {
    let mut bytes = [0; TOKEN_BYTES];
    getrandom::fill(&mut bytes).expect("operating system randomness");
    format!("{prefix}{}", URL_SAFE_NO_PAD.encode(bytes))
}

/// The `foreign_id` projection a principal is addressed by.
#[derive(Clone, Debug, FromRow)]
struct PrincipalRow {
    foreign_id: String,
}

/// The identity row a returning sign-in resolves by `(provider, subject)`.
#[derive(Clone, Debug, FromRow)]
struct IdentityRow {
    id: i64,
    user_id: i64,
    team_id: Option<String>,
}

/// The stable identity an SSO provider verified for one login.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SsoIdentity {
    pub subject: String,
    pub email: Option<String>,
    pub email_verified: bool,
    pub name: Option<String>,
    /// Slack's `https://slack.com/team_id` id_token claim.
    pub team_id: Option<String>,
}

/// `User.link_or_provision`: resolve the identity to a console user, linking a
/// verified email to an existing account or creating an active user. A pending
/// user is activated on login; a disabled user is never re-enabled.
pub async fn link_or_provision_user(
    pool: &PgPool,
    provider: &str,
    identity: &SsoIdentity,
    bootstrap_admin: bool,
) -> Result<ConsoleUser> {
    let email = identity
        .email
        .as_deref()
        .map(str::trim)
        .filter(|email| !email.is_empty())
        .map(str::to_lowercase);
    let name = identity
        .name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty());
    let team_id = (provider == "slack")
        .then(|| {
            identity
                .team_id
                .as_deref()
                .map(str::trim)
                .filter(|team| !team.is_empty())
        })
        .flatten();

    let mut tx = pool.begin().await?;
    let existing: Option<IdentityRow> = sqlx::query_as(
        "SELECT id, user_id, team_id FROM user_identities WHERE provider = $1 AND subject = $2",
    )
    .bind(provider)
    .bind(&identity.subject)
    .fetch_optional(&mut *tx)
    .await?;
    let slack_identity_changed;
    let user_id: i64 = if let Some(existing) = existing {
        let previous_team_id = existing.team_id;
        let effective_team_id = team_id
            .map(str::to_owned)
            .or_else(|| previous_team_id.clone());
        slack_identity_changed = provider == "slack" && effective_team_id != previous_team_id;
        // Only a provider-supplied `team_id` is assigned; keeping the stored
        // value otherwise is the same write.
        let sql = write::update_sql(
            model::UserIdentity::TABLE,
            &["email", "email_verified", "team_id"],
        );
        sqlx::query(&sql)
            .bind(email.as_deref())
            .bind(identity.email_verified)
            .bind(effective_team_id.as_deref())
            .bind(existing.id)
            .execute(&mut *tx)
            .await?;
        if let Some(name) = name {
            // An empty `users.name` is a `WHERE`, not a column list; the helpers
            // only shape the timestamped assignments of a plain `id` update.
            sqlx::query(
                "UPDATE users SET name = $2, updated_at = now() \
                 WHERE id = $1 AND (name IS NULL OR btrim(name) = '')",
            )
            .bind(existing.user_id)
            .bind(name)
            .execute(&mut *tx)
            .await?;
        }
        existing.user_id
    } else {
        // `User`'s create-side validations; a malformed IdP email surfaces as
        // the provisioning failure.
        let Some(email_value) = email.as_deref() else {
            return Err(crate::Error::InvalidData("Email can't be blank".to_owned()));
        };
        if !valid_console_email(email_value) {
            return Err(crate::Error::InvalidData("Email is invalid".to_owned()));
        }
        // The system user api-rs writes as never signs in.
        if email_value.eq_ignore_ascii_case(crate::SYSTEM_USER_EMAIL) {
            return Err(crate::Error::InvalidData("Email is reserved".to_owned()));
        }
        let linkable = if identity.email_verified {
            match email.as_deref() {
                Some(email) => {
                    sqlx::query_scalar("SELECT id FROM users WHERE email = $1")
                        .bind(email)
                        .fetch_optional(&mut *tx)
                        .await?
                }
                None => None,
            }
        } else {
            None
        };
        let user_id = match linkable {
            Some(user_id) => user_id,
            None => {
                let sql = write::insert_returning_sql(
                    model::User::TABLE,
                    &["email", "name", "status", "admin"],
                    "id",
                );
                sqlx::query_scalar(&sql)
                    .bind(email.as_deref())
                    .bind(name)
                    .bind("active")
                    .bind(bootstrap_admin)
                    .fetch_one(&mut *tx)
                    .await?
            }
        };
        slack_identity_changed = provider == "slack";
        let sql = write::insert_sql(
            model::UserIdentity::TABLE,
            &[
                "user_id",
                "provider",
                "subject",
                "email",
                "email_verified",
                "team_id",
            ],
        );
        sqlx::query(&sql)
            .bind(user_id)
            .bind(provider)
            .bind(&identity.subject)
            .bind(email.as_deref())
            .bind(identity.email_verified)
            .bind(team_id)
            .execute(&mut *tx)
            .await?;
        user_id
    };
    // `User.activate_on_login`: legacy pending accounts become active on their
    // next SSO login, without an approver and without gaining admin. The
    // `status = 'pending'` guard and `approved_at` are not a plain `id` update.
    sqlx::query(
        "UPDATE users SET status = 'active', approved_at = now(), updated_at = now() \
         WHERE id = $1 AND status = 'pending'",
    )
    .bind(user_id)
    .execute(&mut *tx)
    .await?;
    if slack_identity_changed {
        // `UserIdentity` after-commit hooks re-run credential reconciliation for
        // every OAuth credential the user owns. The principals area implements
        // the matching; until then the hook is a no-op.
        let credential_ids: Vec<i64> = sqlx::query_scalar(
            "SELECT id FROM broker_credentials \
             WHERE created_by_id = $1 AND oauth_app_id IS NOT NULL ORDER BY id",
        )
        .bind(user_id)
        .fetch_all(&mut *tx)
        .await?;
        for credential_id in credential_ids {
            crate::areas::reconciliation::apply_for_credential(&mut tx, credential_id).await?;
        }
    }
    tx.commit().await?;
    Ok(find_console_user(pool, user_id)
        .await?
        .expect("the provisioned user exists"))
}

const MCP_CLIENT_SELECT: &str = "SELECT id, COALESCE(name, '') AS name, redirect_uris, \
     grant_types, response_types, scopes FROM mcp_oauth_clients";

/// `McpOauthClient` as the OAuth endpoints need it.
#[derive(Clone, Debug, FromRow)]
pub struct McpClient {
    pub id: i64,
    pub name: String,
    #[sqlx(json)]
    pub redirect_uris: Vec<String>,
    #[sqlx(json)]
    pub grant_types: Vec<String>,
    #[sqlx(json)]
    pub response_types: Vec<String>,
    #[sqlx(json)]
    pub scopes: Vec<String>,
}

impl McpClient {
    pub fn public_client_id(&self) -> String {
        McpOauthClient::oid(self.id)
    }

    /// Exact match, plus RFC 8252 loopback matching that ignores the
    /// registered port.
    pub fn redirect_uri_allowed(&self, uri: &str) -> bool {
        if !allowed_redirect_uri(uri) {
            return false;
        }
        let Ok(requested) = Url::parse(uri) else {
            return false;
        };
        self.redirect_uris.iter().any(|registered| {
            if !allowed_redirect_uri(registered) {
                return false;
            }
            if registered == uri {
                return true;
            }
            Url::parse(registered)
                .ok()
                .is_some_and(|registered| loopback_redirect_uri_match(&registered, &requested))
        })
    }
}

/// `McpOauthClient` attributes supplied by dynamic client registration.
#[derive(Clone, Debug)]
pub struct NewMcpClient {
    pub name: String,
    pub redirect_uris: Vec<String>,
    pub grant_types: Vec<String>,
    pub response_types: Vec<String>,
    pub scopes: Vec<String>,
    pub metadata: Value,
}

pub async fn create_mcp_client(pool: &PgPool, client: &NewMcpClient) -> Result<McpClient> {
    let sql = write::insert_returning_sql(
        McpOauthClient::TABLE,
        &[
            "name",
            "redirect_uris",
            "grant_types",
            "response_types",
            "scopes",
            "metadata",
        ],
        "id",
    );
    let id: i64 = sqlx::query_scalar(&sql)
        .bind(&client.name)
        .bind(Value::from(client.redirect_uris.clone()))
        .bind(Value::from(client.grant_types.clone()))
        .bind(Value::from(client.response_types.clone()))
        .bind(Value::from(client.scopes.clone()))
        .bind(&client.metadata)
        .fetch_one(pool)
        .await?;
    Ok(McpClient {
        id,
        name: client.name.clone(),
        redirect_uris: client.redirect_uris.clone(),
        grant_types: client.grant_types.clone(),
        response_types: client.response_types.clone(),
        scopes: client.scopes.clone(),
    })
}

/// The client whose public id is `public_client_id`.
pub async fn find_mcp_client(pool: &PgPool, public_client_id: &str) -> Result<Option<McpClient>> {
    let Some(id) = McpOauthClient::decode_oid(public_client_id) else {
        return Ok(None);
    };
    Ok(
        sqlx::query_as(&format!("{MCP_CLIENT_SELECT} WHERE id = $1"))
            .bind(id)
            .fetch_optional(pool)
            .await?,
    )
}

/// A stored authorization-code row, minus the digest.
#[derive(Clone, Debug, FromRow)]
pub struct StoredAuthorizationCode {
    pub id: i64,
    pub client_id: i64,
    pub user_id: i64,
    pub principal_id: i64,
    pub redirect_uri: String,
    pub resource: String,
    #[sqlx(json)]
    pub scopes: Vec<String>,
    pub code_challenge: String,
}

#[allow(clippy::too_many_arguments)]
pub async fn create_authorization_code(
    pool: &PgPool,
    client_id: i64,
    user_id: i64,
    principal_id: i64,
    redirect_uri: &str,
    code_challenge: &str,
    resource: &str,
    scopes: &[String],
) -> Result<String> {
    let plaintext = issue_authorization_code();
    sqlx::query(
        "INSERT INTO mcp_oauth_authorization_codes \
         (mcp_oauth_client_id, user_id, principal_id, redirect_uri, code_challenge, code_hash, \
          resource, scopes, expires_at, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, \
          (now() + ($9 * interval '1 second')), now(), now())",
    )
    .bind(client_id)
    .bind(user_id)
    .bind(principal_id)
    .bind(redirect_uri)
    .bind(code_challenge)
    .bind(hash_token(&plaintext))
    .bind(resource)
    .bind(Value::from(scopes.to_vec()))
    .bind(AUTHORIZATION_CODE_TTL_SECONDS)
    .execute(pool)
    .await?;
    Ok(plaintext)
}

pub async fn find_usable_authorization_code(
    pool: &PgPool,
    plaintext: &str,
) -> Result<Option<StoredAuthorizationCode>> {
    Ok(sqlx::query_as(
        "SELECT id, mcp_oauth_client_id AS client_id, user_id, principal_id, redirect_uri, \
         resource, scopes, code_challenge \
         FROM mcp_oauth_authorization_codes \
         WHERE code_hash = $1 AND consumed_at IS NULL AND expires_at > now()",
    )
    .bind(hash_token(plaintext))
    .fetch_optional(pool)
    .await?)
}

/// Consume a code if it is still usable. Returns whether this call won the race
/// against a concurrent exchange.
///
/// `consumed_at` is a server clock value, so the statement stays plain SQL.
pub async fn consume_authorization_code(pool: &PgPool, id: i64) -> Result<bool> {
    let result = sqlx::query(
        "UPDATE mcp_oauth_authorization_codes SET consumed_at = now(), updated_at = now() \
         WHERE id = $1 AND consumed_at IS NULL AND expires_at > now()",
    )
    .bind(id)
    .execute(pool)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// A stored refresh-token row, minus the digest.
#[derive(Clone, Debug, FromRow)]
pub struct StoredRefreshToken {
    pub id: i64,
    pub client_id: i64,
    pub user_id: i64,
    pub principal_id: i64,
    pub resource: String,
    #[sqlx(json)]
    pub scopes: Vec<String>,
}

async fn insert_refresh_token(
    executor: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    client_id: i64,
    user_id: i64,
    principal_id: i64,
    resource: &str,
    scopes: &[String],
) -> Result<String> {
    let plaintext = issue_refresh_token();
    sqlx::query(
        "INSERT INTO mcp_oauth_refresh_tokens \
         (mcp_oauth_client_id, user_id, principal_id, resource, scopes, token_hash, \
          expires_at, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $6, \
          (now() + ($7 * interval '1 second')), now(), now())",
    )
    .bind(client_id)
    .bind(user_id)
    .bind(principal_id)
    .bind(resource)
    .bind(Value::from(scopes.to_vec()))
    .bind(hash_token(&plaintext))
    .bind(REFRESH_TOKEN_TTL_SECONDS)
    .execute(executor)
    .await?;
    Ok(plaintext)
}

pub async fn create_refresh_token(
    pool: &PgPool,
    client_id: i64,
    user_id: i64,
    principal_id: i64,
    resource: &str,
    scopes: &[String],
) -> Result<String> {
    insert_refresh_token(pool, client_id, user_id, principal_id, resource, scopes).await
}

pub async fn find_usable_refresh_token(
    pool: &PgPool,
    plaintext: &str,
) -> Result<Option<StoredRefreshToken>> {
    Ok(sqlx::query_as(
        "SELECT id, mcp_oauth_client_id AS client_id, user_id, principal_id, resource, scopes \
         FROM mcp_oauth_refresh_tokens \
         WHERE token_hash = $1 AND revoked_at IS NULL AND expires_at > now()",
    )
    .bind(hash_token(plaintext))
    .fetch_optional(pool)
    .await?)
}

/// Revokes every usable refresh token of the user. `revoked_at` is a server
/// clock value, so the statement stays plain SQL.
pub async fn revoke_refresh_tokens_for_user(pool: &PgPool, user_id: i64) -> Result<()> {
    sqlx::query(
        "UPDATE mcp_oauth_refresh_tokens \
         SET revoked_at = now(), updated_at = now() \
         WHERE user_id = $1 AND revoked_at IS NULL AND expires_at > now()",
    )
    .bind(user_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Touches `last_used_at`. The touched column is a server clock value, so the
/// statement stays plain SQL.
pub async fn touch_mcp_client_last_used(pool: &PgPool, id: i64) -> Result<()> {
    sqlx::query(
        "UPDATE mcp_oauth_clients SET last_used_at = now(), updated_at = now() WHERE id = $1",
    )
    .bind(id)
    .execute(pool)
    .await?;
    Ok(())
}

/// The principal an MCP token names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UserPrincipal {
    pub id: i64,
    pub foreign_id: String,
}

/// The provisioning retry budget.
const PROVISION_MAX_ATTEMPTS: u32 = 3;

/// Find or create the console-user principal for `user`, and seed the
/// `user-mcp` role on first creation.
///
/// Concurrent callers for one user race to insert the same `foreign_id`, so
/// this retries Postgres' `unique_violation` and finds the winner's row on the
/// next attempt.
pub async fn provision_user_principal(pool: &PgPool, user: &ConsoleUser) -> Result<UserPrincipal> {
    let mut attempts = 0;
    loop {
        attempts += 1;
        match provision_user_principal_once(pool, user).await {
            Err(crate::Error::Database(error))
                if is_unique_violation(&error) && attempts < PROVISION_MAX_ATTEMPTS =>
            {
                continue;
            }
            result => return result,
        }
    }
}

/// The provisioner addressed by user id: the `ScheduledTask` author path.
/// Returns `None` when the user no longer exists.
///
/// This is the same provisioner the MCP consent path calls, so both paths write
/// an identical principal row and the same role assignments.
pub(crate) async fn provision_console_user_principal(
    pool: &PgPool,
    user_id: i64,
) -> Result<Option<UserPrincipal>> {
    let Some(user) = find_console_user(pool, user_id).await? else {
        return Ok(None);
    };
    provision_user_principal(pool, &user).await.map(Some)
}

/// Postgres' `unique_violation`.
fn is_unique_violation(error: &sqlx::Error) -> bool {
    matches!(
        error,
        sqlx::Error::Database(database) if database.code().as_deref() == Some("23505")
    )
}

async fn provision_user_principal_once(pool: &PgPool, user: &ConsoleUser) -> Result<UserPrincipal> {
    let mut tx = pool.begin().await?;
    let principal = provision_principal_in_tx(&mut tx, user).await?;
    tx.commit().await?;
    Ok(principal)
}

/// The provisioner's transaction body. Shared by every path that provisions a
/// console-user principal so they cannot drift.
async fn provision_principal_in_tx(
    tx: &mut PgConnection,
    user: &ConsoleUser,
) -> Result<UserPrincipal> {
    let foreign_id = console_user_foreign_id(&user.email);
    let name = user
        .name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .unwrap_or(&user.email);

    let existing =
        model::find_by_foreign_id::<Principal, ProvisionedPrincipalRow, _>(&mut *tx, &foreign_id)
            .await?;
    let (principal_id, foreign_id, newly_created) = match existing {
        Some(row) => {
            let id = row.id;
            let saved_foreign_id = row.foreign_id;
            let (slack_user_id, slack_team_id) =
                unambiguous_slack_identity(&mut *tx, user.id).await?;
            // Only the identity fields that were found are merged, so an absent
            // identity leaves a stored link alone, and the repo-cache label is
            // re-derived from the row's own capability value.
            let next_slack_user_id = slack_user_id.as_deref().or(row.slack_user_id.as_deref());
            let next_slack_team_id = slack_team_id.as_deref().or(row.slack_team_id.as_deref());
            let labels = principal_labels(&row.labels, &row.sandbox_repo_cache);
            let changed = row.name.as_deref() != Some(name)
                || row.kind != "console_user"
                || row.console_user_id != Some(user.id)
                || row.created_by_id.is_none()
                || row.slack_user_id.as_deref() != next_slack_user_id
                || row.slack_team_id.as_deref() != next_slack_team_id
                || row.labels != labels;
            if changed {
                // `created_by_id` is only filled in when it is still empty;
                // every other assignment is a plain literal now that the label
                // set is computed in Rust.
                sqlx::query(
                    "UPDATE principals SET \
                     created_by_id = COALESCE(created_by_id, $2), name = $3, kind = 'console_user', \
                     console_user_id = $2, slack_user_id = $4, slack_team_id = $5, labels = $6, \
                     updated_at = now() WHERE id = $1",
                )
                .bind(id)
                .bind(user.id)
                .bind(name)
                .bind(next_slack_user_id)
                .bind(next_slack_team_id)
                .bind(&labels)
                .execute(&mut *tx)
                .await?;
                // A sync-config field change bumps the principal's own cache
                // version.
                invalidate::invalidate(&mut *tx, Scope::Principal(id)).await?;
            }
            (id, saved_foreign_id, false)
        }
        None => {
            let (slack_user_id, slack_team_id) =
                unambiguous_slack_identity(&mut *tx, user.id).await?;
            let sql = write::insert_returning_sql(
                Principal::TABLE,
                &[
                    "foreign_id",
                    "name",
                    "kind",
                    "console_user_id",
                    "created_by_id",
                    "labels",
                    "slack_user_id",
                    "slack_team_id",
                ],
                "id",
            );
            let id: i64 = sqlx::query_scalar(&sql)
                .bind(&foreign_id)
                .bind(name)
                .bind("console_user")
                .bind(user.id)
                .bind(user.id)
                // A new `principals` row takes the schema's `sandbox_repo_cache`
                // default ("all"), which `apply_sandbox_repo_cache_label` copies
                // into the labels before the insert.
                .bind(principal_labels(&json!({}), "all"))
                .bind(slack_user_id)
                .bind(slack_team_id)
                .fetch_one(&mut *tx)
                .await?;
            (id, foreign_id, true)
        }
    };

    if newly_created {
        // Assigning the default roles is a set-style insert, so it produces no
        // per-record sync-config invalidation.
        sqlx::query(
            "INSERT INTO principal_roles (principal_id, role_id, created_at, updated_at) \
             SELECT $1, id, now(), now() FROM roles WHERE assign_by_default = true \
             ON CONFLICT DO NOTHING",
        )
        .bind(principal_id)
        .execute(&mut *tx)
        .await?;
        seed_user_mcp_role(&mut *tx, user.id, principal_id).await?;
    }
    // `Principal`'s after-commit hook reconciles matching OAuth credentials.
    crate::areas::reconciliation::apply_for_principal(&mut *tx, principal_id).await?;
    Ok(UserPrincipal {
        id: principal_id,
        foreign_id,
    })
}

/// The `principals` columns the provisioner compares a stored row against.
#[derive(Debug, FromRow)]
struct ProvisionedPrincipalRow {
    id: i64,
    foreign_id: String,
    name: Option<String>,
    kind: String,
    console_user_id: Option<i64>,
    created_by_id: Option<i64>,
    slack_user_id: Option<String>,
    slack_team_id: Option<String>,
    labels: Value,
    sandbox_repo_cache: String,
}

/// The label every provisioned principal and the `user-mcp` role carry.
fn managed_by_labels() -> Value {
    json!({ "managed-by": "centaur" })
}

async fn seed_user_mcp_role(tx: &mut PgConnection, user_id: i64, principal_id: i64) -> Result<()> {
    // An existing role keeps its name and labels.
    let role_sql = format!(
        "{} ON CONFLICT (foreign_id) DO NOTHING",
        write::insert_sql(
            Role::TABLE,
            &["foreign_id", "name", "labels", "created_by_id"]
        )
    );
    sqlx::query(&role_sql)
        .bind("user-mcp")
        .bind("User MCP")
        .bind(managed_by_labels())
        .bind(user_id)
        .execute(&mut *tx)
        .await?;
    let role_id: i64 = sqlx::query_scalar("SELECT id FROM roles WHERE foreign_id = 'user-mcp'")
        .fetch_one(&mut *tx)
        .await?;
    let assignment_sql = format!(
        "{} ON CONFLICT (principal_id, role_id) DO NOTHING",
        write::insert_sql(PrincipalRole::TABLE, &["principal_id", "role_id"])
    );
    let assigned = sqlx::query(&assignment_sql)
        .bind(principal_id)
        .bind(role_id)
        .execute(&mut *tx)
        .await?
        .rows_affected()
        > 0;
    if assigned {
        // `PrincipalRole` includes `SyncConfigCacheInvalidation`, so creating
        // the assignment bumps the principal once.
        invalidate::invalidate(&mut *tx, Scope::Principal(principal_id)).await?;
    }
    Ok(())
}

/// The `(subject, team_id)` pair of one Slack identity.
#[derive(Clone, Debug, FromRow)]
struct SlackIdentityRow {
    subject: Option<String>,
    team_id: Option<String>,
}

async fn unambiguous_slack_identity(
    tx: &mut PgConnection,
    user_id: i64,
) -> Result<(Option<String>, Option<String>)> {
    let rows: Vec<SlackIdentityRow> = sqlx::query_as(
        "SELECT subject, team_id FROM user_identities \
         WHERE user_id = $1 AND provider = 'slack' ORDER BY id",
    )
    .bind(user_id)
    .fetch_all(&mut *tx)
    .await?;
    let mut pairs: Vec<(String, String)> = rows
        .into_iter()
        .filter_map(|row| {
            let subject = row
                .subject
                .map(|value| value.trim().to_owned())
                .filter(|v| !v.is_empty())?;
            let team_id = row
                .team_id
                .map(|value| value.trim().to_owned())
                .filter(|v| !v.is_empty())?;
            Some((subject, team_id))
        })
        .collect();
    pairs.sort();
    pairs.dedup();
    let [(subject, team_id)] = pairs.as_slice() else {
        return Ok((None, None));
    };
    if !slack_user_id(subject) || !slack_team_id(team_id) {
        return Ok((None, None));
    }
    Ok((Some(subject.clone()), Some(team_id.clone())))
}

fn slack_user_id(value: &str) -> bool {
    if value == "USLACK" {
        return true;
    }
    let bytes = value.as_bytes();
    matches!(bytes.first(), Some(b'U' | b'W'))
        && bytes.len() >= 9
        && bytes[1..]
            .iter()
            .all(|byte| byte.is_ascii_digit() || byte.is_ascii_uppercase())
}

fn slack_team_id(value: &str) -> bool {
    let bytes = value.as_bytes();
    matches!(bytes.first(), Some(b'T' | b'E'))
        && bytes.len() >= 9
        && bytes[1..]
            .iter()
            .all(|byte| byte.is_ascii_digit() || byte.is_ascii_uppercase())
}

/// The email shape an SSO user's address is checked against.
fn valid_console_email(email: &str) -> bool {
    let Some((local, domain)) = email.split_once('@') else {
        return false;
    };
    if local.is_empty() || domain.is_empty() {
        return false;
    }
    const LOCAL: &str =
        "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789.!#$%&'*+/=?^_`{|}~-";
    if !local.chars().all(|character| LOCAL.contains(character)) {
        return false;
    }
    domain.split('.').all(|label| {
        !label.is_empty()
            && label.len() <= 63
            && label
                .chars()
                .next()
                .is_some_and(|character| character.is_ascii_alphanumeric())
            && label
                .chars()
                .last()
                .is_some_and(|character| character.is_ascii_alphanumeric())
            && label
                .chars()
                .all(|character| character.is_ascii_alphanumeric() || character == '-')
    })
}

/// The console-user principal's `foreign_id` for an email.
pub fn console_user_foreign_id(email: &str) -> String {
    let normalized = email.trim().to_lowercase();
    let mut safe = String::new();
    for character in normalized.chars() {
        let character = if character.is_ascii_alphanumeric() || "-._~".contains(character) {
            character
        } else {
            '-'
        };
        if character == '-' && safe.ends_with('-') {
            continue;
        }
        safe.push(character);
    }
    let safe: String = safe.chars().take(48).collect();
    let digest = hex::encode(Sha256::digest(normalized.as_bytes()));
    format!("console-user-{safe}-{}", &digest[..12])
}

/// Whether a client may use `value` as a redirect URI.
pub fn allowed_redirect_uri(value: &str) -> bool {
    if value.contains('*') {
        return false;
    }
    let Ok(uri) = Url::parse(value) else {
        return false;
    };
    if uri.fragment().is_some_and(|fragment| !fragment.is_empty()) || uri.scheme().is_empty() {
        return false;
    }
    match uri.scheme() {
        "https" => uri.host().is_some(),
        "http" => uri.host_str().is_some_and(loopback_host),
        // RFC 8252 private-use scheme; exact matching happens separately.
        _ => true,
    }
}

fn loopback_redirect_uri_match(registered: &Url, requested: &Url) -> bool {
    if registered.scheme() != "http" || requested.scheme() != "http" {
        return false;
    }
    if !loopback_host(registered.host_str().unwrap_or_default())
        || !loopback_host(requested.host_str().unwrap_or_default())
    {
        return false;
    }
    let registered_port = registered.port();
    if registered_port.is_some_and(|port| port != 80) {
        return false;
    }
    registered.path() == requested.path() && registered.query() == requested.query()
}

fn loopback_host(host: &str) -> bool {
    let normalized = host.trim_matches(['[', ']']).to_ascii_lowercase();
    normalized == "localhost"
        || normalized
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback())
}

/// Rotate a refresh token atomically: revoke the presented row and insert its
/// replacement in one transaction. Returns `None` when the old token lost a
/// concurrent rotation race.
pub async fn rotate_refresh_token(
    pool: &PgPool,
    old_id: i64,
    client_id: i64,
    user_id: i64,
    principal_id: i64,
    resource: &str,
    scopes: &[String],
) -> Result<Option<String>> {
    let mut transaction = pool.begin().await?;
    let revoked = sqlx::query(
        "UPDATE mcp_oauth_refresh_tokens \
         SET revoked_at = now(), last_used_at = now(), updated_at = now() \
         WHERE id = $1 AND revoked_at IS NULL AND expires_at > now()",
    )
    .bind(old_id)
    .execute(&mut *transaction)
    .await?;
    if revoked.rows_affected() != 1 {
        transaction.rollback().await?;
        return Ok(None);
    }
    let plaintext = insert_refresh_token(
        &mut *transaction,
        client_id,
        user_id,
        principal_id,
        resource,
        scopes,
    )
    .await?;
    transaction.commit().await?;
    Ok(Some(plaintext))
}

/// The stable `foreign_id` of a principal, for MCP token observability claims.
pub async fn principal_foreign_id(pool: &PgPool, principal_id: i64) -> Result<Option<String>> {
    Ok(
        model::find_by_id::<Principal, PrincipalRow, _>(pool, principal_id)
            .await?
            .map(|row| row.foreign_id),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sso_user_creation_requires_a_valid_email() {
        assert!(valid_console_email("operator@example.test"));
        for malformed in ["not-an-email", "operator@", "@example.test", "a@b@c"] {
            assert!(!valid_console_email(malformed), "{malformed}");
        }
    }

    #[test]
    fn client_redirect_uris_follow_the_public_client_rules() {
        for allowed in [
            "http://localhost/callback",
            "http://127.0.0.1/callback",
            "http://[::1]:8080/callback",
            "https://example.test/callback",
            "cursor://anysphere.cursor-mcp/oauth/callback",
            "https://example.test/callback#",
        ] {
            assert!(allowed_redirect_uri(allowed), "{allowed}");
        }
        for refused in [
            "",
            "callback",
            "http://example.test/callback",
            "https://example.test/callback#fragment",
            "https://example.test/*",
        ] {
            assert!(!allowed_redirect_uri(refused), "{refused}");
        }
    }

    #[test]
    fn loopback_matching_ignores_only_the_registered_port() {
        let client = McpClient {
            id: 1,
            name: "Example".to_owned(),
            redirect_uris: vec!["http://localhost/callback".to_owned()],
            grant_types: vec![],
            response_types: vec![],
            scopes: vec![],
        };
        assert!(client.redirect_uri_allowed("http://localhost/callback"));
        assert!(client.redirect_uri_allowed("http://localhost:49153/callback"));
        assert!(client.redirect_uri_allowed("http://127.0.0.1:49153/callback"));
        assert!(!client.redirect_uri_allowed("http://localhost:49153/other"));
        assert!(!client.redirect_uri_allowed("http://localhost:49153/callback?x=1"));
        assert!(!client.redirect_uri_allowed("http://localhost:49153/callback#x"));

        let registered_port = McpClient {
            redirect_uris: vec!["http://localhost:3000/callback".to_owned()],
            ..client
        };
        assert!(!registered_port.redirect_uri_allowed("http://localhost:49153/callback"));
        assert!(registered_port.redirect_uri_allowed("http://localhost:3000/callback"));
    }

    #[test]
    fn tokens_use_console_prefixes_and_url_safe_bodies() {
        let code = issue_authorization_code();
        assert!(code.starts_with("mcpauth_"));
        assert_eq!(code.len(), "mcpauth_".len() + 64);
        assert!(
            code["mcpauth_".len()..]
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        );
        let refresh = issue_refresh_token();
        assert!(refresh.starts_with("mcprt_"));
        assert_eq!(hash_token("value").len(), 64);
    }
}
