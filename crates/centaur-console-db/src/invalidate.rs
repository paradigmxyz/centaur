//! Console's sync-config cache invalidation.
//!
//! A principal's proxy configuration is cached and rebuilt when
//! `principals.sync_config_cache_version` changes. Rails runs that invalidation
//! from model callbacks (`SyncConfigCacheInvalidation`, `SyncConfigOwner`,
//! `Grant`, `BrokerCredential`) and counts **one increment per saved or
//! destroyed record**, not per affected principal. The port writes rows with
//! SQL instead of models, so every write path states the invalidations its
//! callbacks would have produced, at the same point in its transaction and with
//! the same number of increments.
//!
//! There is exactly one entry point, [`invalidate`], and one [`Scope`] per kind
//! of affected row:
//!
//! ```ignore
//! use centaur_console_db::invalidate::{self, Scope};
//!
//! // A principal write changed a sync-relevant column.
//! invalidate::invalidate(&mut tx, Scope::Principal(principal.id)).await?;
//!
//! // A secret write replaced its nested records: Rails counts the parent (when
//! // it changed) plus every destroyed and inserted source and rule.
//! let changes = invalidate::SecretChanges::replaced(true, 1, 1, 0, 2);
//! invalidate::invalidate(&mut tx, Scope::secret_write(kind, id, changes)).await?;
//!
//! // A grant was destroyed: bump every principal it reached, directly or
//! // through its role.
//! invalidate::invalidate(&mut tx, Scope::Grantee { principal_id, role_id }).await?;
//! ```
//!
//! Pass the transaction's connection (`&mut *tx`) so the bump participates in
//! the same transaction. `invalidate` takes a plain [`sqlx::PgConnection`], which
//! both a pool and a transaction deref to.

use sqlx::PgConnection;

use crate::Result;
use crate::areas::secrets::SecretKind;

/// The rows whose save or destroy invalidates a principal's sync config.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope<'a> {
    /// One principal's own row changed.
    Principal(i64),
    /// A set of principals changed; ids are deduplicated, each bumped once.
    Principals(&'a [i64]),
    /// A role changed, so every principal that holds it changed.
    Role(i64),
    /// A grant was saved or destroyed: every principal it reaches, directly or
    /// through its role.
    Grantee {
        principal_id: Option<i64>,
        role_id: Option<i64>,
    },
    /// A secret changed: every principal its grants reach, `times` increments.
    Secret {
        kind: SecretKind,
        id: i64,
        times: i32,
    },
    /// A broker credential was refreshed or revived: every principal granted a
    /// secret whose `token_broker` source delivers this credential.
    BrokerCredential(i64),
}

/// Rails counts one sync-cache bump per saved or destroyed record associated
/// with a secret: the parent (when it changed), every destroyed and inserted
/// source, and every destroyed and inserted rule. `ssh_key_secrets` does not
/// invalidate through the parent, only through its source.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SecretChanges {
    parent_changed: bool,
    old_sources: usize,
    new_sources: usize,
    old_rules: usize,
    new_rules: usize,
}

impl SecretChanges {
    pub fn none() -> Self {
        Self::default()
    }

    /// A new secret: its parent is new and every source and rule is inserted.
    pub fn created(sources: usize, rules: usize) -> Self {
        Self {
            parent_changed: true,
            old_sources: 0,
            new_sources: sources,
            old_rules: 0,
            new_rules: rules,
        }
    }

    /// A replacement write. `old_*` are the nested rows that were destroyed,
    /// `new_*` the ones that were inserted.
    pub fn replaced(
        parent_changed: bool,
        old_sources: usize,
        new_sources: usize,
        old_rules: usize,
        new_rules: usize,
    ) -> Self {
        Self {
            parent_changed,
            old_sources,
            new_sources,
            old_rules,
            new_rules,
        }
    }

    /// The number of increments the write produced for `kind`.
    pub fn count(self, kind: SecretKind) -> i32 {
        let parent = usize::from(self.parent_changed && kind != SecretKind::SshKey);
        i32::try_from(
            parent + self.old_sources + self.new_sources + self.old_rules + self.new_rules,
        )
        .unwrap_or(i32::MAX)
    }
}

impl<'a> Scope<'a> {
    /// The invalidation a secret write performs.
    pub fn secret_write(kind: SecretKind, id: i64, changes: SecretChanges) -> Self {
        Self::Secret {
            kind,
            id,
            times: changes.count(kind),
        }
    }

    /// The invalidation a secret delete performs while its grants still exist:
    /// the destroyed parent reaches every grantee once.
    pub fn secret_delete(kind: SecretKind, id: i64) -> Self {
        Self::Secret { kind, id, times: 1 }
    }
}

/// Bumps `sync_config_cache_version` for the principals `scope` reaches.
///
/// Runs inside the caller's transaction; call it at the point Rails' callback
/// would fire, before the commit.
pub async fn invalidate(connection: &mut PgConnection, scope: Scope<'_>) -> Result<()> {
    match scope {
        Scope::Principal(id) => bump_principals(connection, &[id]).await,
        Scope::Principals(ids) => bump_principals(connection, ids).await,
        Scope::Role(role_id) => bump_grantee(connection, None, Some(role_id)).await,
        Scope::Grantee {
            principal_id,
            role_id,
        } => bump_grantee(connection, principal_id, role_id).await,
        Scope::Secret { kind, id, times } => bump_secret(connection, kind, id, times).await,
        Scope::BrokerCredential(id) => bump_broker_credential(connection, id).await,
    }
}

/// Rails' `bump_sync_config_cache_versions`: one increment per principal.
async fn bump_principals(connection: &mut PgConnection, ids: &[i64]) -> Result<()> {
    let mut ids = ids.to_vec();
    ids.sort_unstable();
    ids.dedup();
    if ids.is_empty() {
        return Ok(());
    }
    sqlx::query(
        "UPDATE principals SET sync_config_cache_version = sync_config_cache_version + 1 \
         WHERE id = ANY($1)",
    )
    .bind(&ids)
    .execute(connection)
    .await?;
    Ok(())
}

/// The distinct principal ids reachable through a grantee's grants.
async fn grantee_principals(
    connection: &mut PgConnection,
    principal_id: Option<i64>,
    role_id: Option<i64>,
) -> Result<Vec<i64>> {
    if let Some(id) = principal_id {
        return Ok(vec![id]);
    }
    let Some(role_id) = role_id else {
        return Ok(Vec::new());
    };
    Ok(
        sqlx::query_scalar("SELECT principal_id FROM principal_roles WHERE role_id = $1")
            .bind(role_id)
            .fetch_all(connection)
            .await?,
    )
}

/// `Grant`'s own invalidation for a single grantee: bump every principal the
/// grant reaches, directly or through its role.
async fn bump_grantee(
    connection: &mut PgConnection,
    principal_id: Option<i64>,
    role_id: Option<i64>,
) -> Result<()> {
    let affected = grantee_principals(connection, principal_id, role_id).await?;
    bump_principals(connection, &affected).await
}

/// Bump `sync_config_cache_version` for every principal a grant of this secret
/// reaches, mirroring `Principal.bump_sync_config_cache_versions`.
async fn bump_secret(
    connection: &mut PgConnection,
    kind: SecretKind,
    secret_id: i64,
    times: i32,
) -> Result<()> {
    if times <= 0 {
        return Ok(());
    }
    sqlx::query(&format!(
        "UPDATE principals SET sync_config_cache_version = sync_config_cache_version + $1 \
         WHERE id IN ( \
           SELECT principal_id FROM grants WHERE {} = $2 AND principal_id IS NOT NULL \
           UNION \
           SELECT pr.principal_id FROM principal_roles pr \
           WHERE pr.role_id IN (SELECT role_id FROM grants WHERE {} = $2 AND role_id IS NOT NULL) \
         )",
        kind.grant_column(),
        kind.grant_column()
    ))
    .bind(times)
    .bind(secret_id)
    .execute(connection)
    .await?;
    Ok(())
}

/// Rails' `BrokerCredential#bump_referencing_principal_sync_config_versions`,
/// run from `before_commit` whenever `access_token`, `dead`, `last_refresh`, or
/// `expires_at` changed. It is the same invalidation `centaur-token-broker`
/// performs after a token refresh.
async fn bump_broker_credential(connection: &mut PgConnection, credential_id: i64) -> Result<()> {
    sqlx::query(
        "WITH owner_grants AS ( \
             SELECT grants.principal_id, grants.role_id \
             FROM secret_sources sources \
             JOIN grants ON grants.static_secret_id = sources.static_secret_id \
                 OR grants.gcp_auth_secret_id = sources.gcp_auth_secret_id \
                 OR grants.gcp_id_token_secret_id = sources.gcp_id_token_secret_id \
                 OR grants.aws_auth_secret_id = sources.aws_auth_secret_id \
                 OR grants.oauth_token_secret_id = sources.oauth_token_secret_id \
                 OR grants.pg_dsn_secret_id = sources.pg_dsn_secret_id \
                 OR grants.hmac_secret_id = sources.hmac_secret_id \
                 OR grants.ssh_key_secret_id = sources.ssh_key_secret_id \
             WHERE sources.source_type = 'token_broker' \
               AND sources.broker_credential_id = $1 \
         ) \
         UPDATE principals SET sync_config_cache_version = sync_config_cache_version + 1 \
         WHERE id IN (SELECT principal_id FROM owner_grants) \
            OR id IN (SELECT principal_roles.principal_id FROM principal_roles \
                      JOIN owner_grants ON owner_grants.role_id = principal_roles.role_id)",
    )
    .bind(credential_id)
    .execute(connection)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_changes_count_every_saved_and_destroyed_record() {
        assert_eq!(SecretChanges::created(1, 2).count(SecretKind::Static), 4);
        assert_eq!(
            SecretChanges::replaced(true, 1, 1, 0, 2).count(SecretKind::Static),
            5
        );
        // ssh_key_secrets invalidate through the source, never the parent.
        assert_eq!(SecretChanges::created(1, 0).count(SecretKind::SshKey), 1);
        assert_eq!(SecretChanges::none().count(SecretKind::Static), 0);
    }

    #[test]
    fn scopes_carry_the_same_increment() {
        assert_eq!(
            Scope::secret_write(
                SecretKind::Static,
                7,
                SecretChanges::replaced(false, 1, 1, 0, 0)
            ),
            Scope::Secret {
                kind: SecretKind::Static,
                id: 7,
                times: 2,
            }
        );
        assert_eq!(
            Scope::secret_delete(SecretKind::SshKey, 7),
            Scope::Secret {
                kind: SecretKind::SshKey,
                id: 7,
                times: 1,
            }
        );
    }
}
