//! The Console tables the port addresses, and the lookups and deletes every
//! area needs.
//!
//! Each area used to repeat the same three pieces of plumbing for its resource:
//! the table name, the opaque-id prefix, the Rails model name a 404 renders, and
//! the `WHERE id = $1` / `WHERE foreign_id = $1` queries. [`ConsoleModel`]
//! carries that descriptor as associated constants, and the free functions in
//! this module run the shared queries against any row type that derives
//! [`sqlx::FromRow`].
//!
//! ```ignore
//! use centaur_console_db::model::{self, ApiKey, ConsoleModel};
//!
//! let key = model::find_by_oid::<ApiKey, ApiKeyRow, _>(pool, oid)
//!     .await?
//!     .ok_or_else(ApiKey::not_found)?;
//! model::delete::<ApiKey, _>(pool, key.id).await?; // soft delete
//! ```
//!
//! The descriptor and the row type are separate parameters: the marker types
//! ([`ApiKey`], [`Principal`], …) describe a table, and the area's own `FromRow`
//! struct is the projection it selects, because one table can have several.

use sqlx::postgres::PgRow;
use sqlx::{Executor, FromRow, Postgres};

use crate::error::{Error, Result};
use crate::oid::{decode_oid, oid};

/// One Console table as the routes address it.
///
/// The capability constants describe the model rather than the query: they tell
/// a generic helper whether a column exists and how a delete behaves, so an area
/// never re-derives it. `SOFT_DELETE_COLUMN` is the column Rails' model scopes
/// out (`ApiKey#deleted_at`); without one, [`delete`] removes the row.
pub trait ConsoleModel: 'static {
    /// The Postgres table.
    const TABLE: &'static str;
    /// Console's opaque-id prefix (`prn_...`, `role_...`).
    const OID_PREFIX: &'static str;
    /// The Rails model name; a missing row renders `"<NAME> not found"`.
    const NAME: &'static str;
    /// Whether the table has a unique `foreign_id` column.
    const HAS_FOREIGN_ID: bool = false;
    /// Whether the table has a `labels` jsonb column.
    const HAS_LABELS: bool = false;
    /// The soft-delete column, when deletes only scope the row out.
    const SOFT_DELETE_COLUMN: Option<&'static str> = None;

    /// Console's opaque id for row `id`. A table without an opaque id has an
    /// empty prefix and no routes that address it this way.
    fn oid(id: i64) -> String {
        oid(Self::OID_PREFIX, id)
    }

    /// The row id an opaque id names, when it is canonical for this model. A
    /// table without an opaque id never matches.
    fn decode_oid(value: &str) -> Option<i64> {
        if Self::OID_PREFIX.is_empty() {
            return None;
        }
        decode_oid(Self::OID_PREFIX, value)
    }

    /// The uniform 404 both a missing path segment and a missing by-name
    /// reference render.
    fn not_found() -> Error {
        Error::not_found(Self::NAME)
    }
}

/// The row of `M` whose primary key is `id`, projected as `Row`.
///
/// Soft-deleted rows are not found, matching Rails' model scope.
pub async fn find_by_id<'e, M, Row, E>(executor: E, id: i64) -> Result<Option<Row>>
where
    M: ConsoleModel,
    Row: for<'r> FromRow<'r, PgRow> + Send + Unpin,
    E: Executor<'e, Database = Postgres>,
{
    let mut sql = format!("SELECT * FROM {} WHERE id = $1", M::TABLE);
    if let Some(column) = M::SOFT_DELETE_COLUMN {
        sql.push_str(&format!(" AND {column} IS NULL"));
    }
    Ok(sqlx::query_as::<_, Row>(&sql)
        .bind(id)
        .fetch_optional(executor)
        .await?)
}

/// The row `oid` names, when `oid` is canonical for `M` and the row exists.
pub async fn find_by_oid<'e, M, Row, E>(executor: E, oid: &str) -> Result<Option<Row>>
where
    M: ConsoleModel,
    Row: for<'r> FromRow<'r, PgRow> + Send + Unpin,
    E: Executor<'e, Database = Postgres>,
{
    let Some(id) = M::decode_oid(oid) else {
        return Ok(None);
    };
    find_by_id::<M, Row, _>(executor, id).await
}

/// The row whose `foreign_id` is `foreign_id`. A model without that column has
/// no such row, so this is always `Ok(None)`.
pub async fn find_by_foreign_id<'e, M, Row, E>(executor: E, foreign_id: &str) -> Result<Option<Row>>
where
    M: ConsoleModel,
    Row: for<'r> FromRow<'r, PgRow> + Send + Unpin,
    E: Executor<'e, Database = Postgres>,
{
    if !M::HAS_FOREIGN_ID {
        return Ok(None);
    }
    let mut sql = format!("SELECT * FROM {} WHERE foreign_id = $1", M::TABLE);
    if let Some(column) = M::SOFT_DELETE_COLUMN {
        sql.push_str(&format!(" AND {column} IS NULL"));
    }
    Ok(sqlx::query_as::<_, Row>(&sql)
        .bind(foreign_id)
        .fetch_optional(executor)
        .await?)
}

/// The row a path segment names: an opaque id when it decodes for `M`, otherwise
/// a `foreign_id`, which is how the lookup routes accept both spellings.
pub async fn find_by_ident<'e, M, Row, E>(executor: E, ident: &str) -> Result<Option<Row>>
where
    M: ConsoleModel,
    Row: for<'r> FromRow<'r, PgRow> + Send + Unpin,
    E: Executor<'e, Database = Postgres>,
{
    if let Some(id) = M::decode_oid(ident) {
        return find_by_id::<M, Row, _>(executor, id).await;
    }
    find_by_foreign_id::<M, Row, _>(executor, ident).await
}

/// Deletes row `id`, soft-deleting it when the model has a soft-delete column.
/// Returns the number of affected rows, so `0` is a row that was already gone.
///
/// This is the plain record delete only. A model with dependent rows or
/// invalidation callbacks keeps those in its area, which must run them around
/// this call the way Rails' `destroy` does.
pub async fn delete<'e, M, E>(executor: E, id: i64) -> Result<u64>
where
    M: ConsoleModel,
    E: Executor<'e, Database = Postgres>,
{
    let sql = match M::SOFT_DELETE_COLUMN {
        Some(column) => format!("UPDATE {} SET {column} = now() WHERE id = $1", M::TABLE),
        None => format!("DELETE FROM {} WHERE id = $1", M::TABLE),
    };
    Ok(sqlx::query(&sql)
        .bind(id)
        .execute(executor)
        .await?
        .rows_affected())
}

/// The Console tables the areas address. The doc comment on each marker is the
/// Rails model whose name a 404 renders; the arguments after it are
/// `table, oid prefix, NAME, has foreign_id, has labels, soft-delete column`.
macro_rules! console_models {
    ($($(#[$doc:meta])* $marker:ident: $table:literal, $prefix:literal, $name:literal,
        $foreign_id:literal, $labels:literal, $soft:expr;)*) => {
        $(
            $(#[$doc])*
            #[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
            pub struct $marker;

            impl ConsoleModel for $marker {
                const TABLE: &'static str = $table;
                const OID_PREFIX: &'static str = $prefix;
                const NAME: &'static str = $name;
                const HAS_FOREIGN_ID: bool = $foreign_id;
                const HAS_LABELS: bool = $labels;
                const SOFT_DELETE_COLUMN: Option<&'static str> = $soft;
            }
        )*
    };
}

console_models! {
    /// `Principal`
    Principal: "principals", "prn", "Principal", true, true, None;
    /// `Role`
    Role: "roles", "role", "Role", true, true, None;
    /// `Grant`
    Grant: "grants", "grant", "Grant", false, false, None;
    /// `PrincipalRole` (nested under a principal and a role)
    PrincipalRole: "principal_roles", "prole", "PrincipalRole", false, false, None;
    /// `SecretSource` (nested under a secret)
    SecretSource: "secret_sources", "scs", "SecretSource", false, false, None;
    /// `RequestRule` (nested under a secret)
    RequestRule: "request_rules", "rqr", "RequestRule", false, false, None;
    /// `StaticSecret`
    StaticSecret: "static_secrets", "ssr", "StaticSecret", true, true, None;
    /// `GcpAuthSecret`
    GcpAuthSecret: "gcp_auth_secrets", "gas", "GcpAuthSecret", true, true, None;
    /// `GcpIdTokenSecret`
    GcpIdTokenSecret: "gcp_id_token_secrets", "gid", "GcpIdTokenSecret", true, true, None;
    /// `AwsAuthSecret`
    AwsAuthSecret: "aws_auth_secrets", "aas", "AwsAuthSecret", true, true, None;
    /// `OauthTokenSecret`
    OauthTokenSecret: "oauth_token_secrets", "ots", "OauthTokenSecret", true, true, None;
    /// `PgDsnSecret`
    PgDsnSecret: "pg_dsn_secrets", "pgs", "PgDsnSecret", true, true, None;
    /// `HmacSecret`
    HmacSecret: "hmac_secrets", "hms", "HmacSecret", true, true, None;
    /// `SshKeySecret` (its owner is unique, so it is found by owner too)
    SshKeySecret: "ssh_key_secrets", "sks", "SshKeySecret", true, true, None;
    /// `OauthApp`
    OauthApp: "oauth_apps", "oap", "OauthApp", false, true, None;
    /// `BrokerCredential`
    BrokerCredential: "broker_credentials", "bcr", "BrokerCredential", true, true, None;
    /// `McpOauthClient`
    McpOauthClient: "mcp_oauth_clients", "moc", "McpOauthClient", false, false, None;
    /// `McpOauthAuthorizationCode` (no opaque id of its own)
    McpOauthAuthorizationCode: "mcp_oauth_authorization_codes", "moa", "McpOauthAuthorizationCode", false, false, None;
    /// `McpOauthRefreshToken`
    McpOauthRefreshToken: "mcp_oauth_refresh_tokens", "mor", "McpOauthRefreshToken", false, false, None;
    /// `User`
    User: "users", "usr", "User", false, false, None;
    /// `UserIdentity` (nested under a user)
    UserIdentity: "user_identities", "usid", "UserIdentity", false, false, None;
    /// `ApiClient`
    ApiClient: "api_clients", "acl", "ApiClient", false, false, None;
    /// `ApiClientPrincipal` (nested under a client)
    ApiClientPrincipal: "api_client_principals", "aclp", "ApiClientPrincipal", false, false, None;
    /// `ApiKey`
    ApiKey: "api_keys", "ak", "ApiKey", false, false, Some("deleted_at");
    /// `Proxy`
    Proxy: "proxies", "prx", "Proxy", false, true, None;
    /// `SystemSetting` (a singleton, no opaque id)
    SystemSetting: "system_settings", "", "SystemSetting", false, false, None;
    /// `ScheduledTask`
    ScheduledTask: "scheduled_tasks", "tsk", "ScheduledTask", false, false, None;
    /// `Skill`
    Skill: "skills", "skl", "Skill", false, false, None;
    /// `SkillEditor` (nested under a skill; no opaque id)
    SkillEditor: "skill_editors", "", "SkillEditor", false, false, None;
    /// `SlackBotChannel` (no opaque id)
    SlackBotChannel: "slack_bot_channels", "", "SlackBotChannel", false, false, None;
    /// `SlackChannelPermission`
    SlackChannelPermission: "slack_channel_permissions", "scp", "SlackChannelPermission", false, false, None;
    /// `PrincipalSyncConfigSnapshot` (no opaque id)
    PrincipalSyncConfigSnapshot: "principal_sync_config_snapshots", "", "PrincipalSyncConfigSnapshot", false, false, None;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Clippy rejects both `assert!`/`assert_eq!` on a constant, so the flags
    /// are asserted through a call.
    fn assert_flag(value: bool) {
        assert!(value);
    }

    #[test]
    fn descriptors_match_the_schema() {
        assert_eq!(Principal::TABLE, "principals");
        assert_eq!(Principal::OID_PREFIX, "prn");
        assert_eq!(Principal::NAME, "Principal");
        assert_flag(Principal::HAS_FOREIGN_ID);
        assert_flag(Principal::HAS_LABELS);
        assert_eq!(Principal::SOFT_DELETE_COLUMN, None);
        assert_eq!(ApiKey::SOFT_DELETE_COLUMN, Some("deleted_at"));
        assert_flag(!ApiKey::HAS_FOREIGN_ID);
        assert_eq!(StaticSecret::TABLE, "static_secrets");
    }

    #[test]
    fn not_found_is_the_plain_model_name() {
        assert_eq!(Principal::not_found().to_string(), "Principal not found");
        assert_eq!(ApiKey::not_found().to_string(), "ApiKey not found");
    }

    #[test]
    fn oids_decode_only_for_their_own_model() {
        let value = Principal::oid(123);
        assert_eq!(Principal::decode_oid(&value), Some(123));
        assert_eq!(Role::decode_oid(&value), None);
    }
}
