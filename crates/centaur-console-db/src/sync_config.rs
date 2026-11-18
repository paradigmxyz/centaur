//! Sync-config cache invalidation.
//!
//! proxy-sync caches each principal's config by
//! `principals.sync_config_cache_version`. Writers call these inside their
//! transaction, after the last write and just before `tx.commit()`, so the
//! affected principals are resolved against the final state. `updated_at` is
//! not touched.

use sqlx::PgConnection;

use crate::Result;

/// Every grant column naming a grantable.
pub const GRANT_COLUMNS: &[&str] = &[
    "static_secret_id",
    "gcp_auth_secret_id",
    "gcp_id_token_secret_id",
    "aws_auth_secret_id",
    "oauth_token_secret_id",
    "pg_dsn_secret_id",
    "hmac_secret_id",
    "ssh_key_secret_id",
];

/// Invalidates the cached sync config of `principal_ids`.
pub async fn bump_sync_config(conn: &mut PgConnection, principal_ids: &[i64]) -> Result<()> {
    if !principal_ids.is_empty() {
        sqlx::query(
            "UPDATE principals SET sync_config_cache_version = sync_config_cache_version + 1 \
             WHERE id = ANY($1)",
        )
        .bind(principal_ids)
        .execute(conn)
        .await?;
    }
    Ok(())
}

/// The principals granted a grantable (`grant_column` is one of
/// [`GRANT_COLUMNS`], `id` the grantable's id), directly or through a role.
pub async fn grantee_ids(
    conn: &mut PgConnection,
    grant_column: &'static str,
    id: i64,
) -> Result<Vec<i64>> {
    assert!(
        GRANT_COLUMNS.contains(&grant_column),
        "unknown grant column {grant_column}"
    );
    Ok(sqlx::query_scalar(&format!(
        "SELECT principal_id FROM grants WHERE {grant_column} = $1 AND principal_id IS NOT NULL \
         UNION SELECT pr.principal_id FROM principal_roles pr \
           JOIN grants g ON g.role_id = pr.role_id WHERE g.{grant_column} = $1"
    ))
    .bind(id)
    .fetch_all(conn)
    .await?)
}

/// Every member of a role.
pub async fn role_member_ids(conn: &mut PgConnection, role_id: i64) -> Result<Vec<i64>> {
    Ok(
        sqlx::query_scalar("SELECT principal_id FROM principal_roles WHERE role_id = $1")
            .bind(role_id)
            .fetch_all(conn)
            .await?,
    )
}

/// Every principal (system-wide settings changed).
pub async fn bump_all_sync_config(conn: &mut PgConnection) -> Result<()> {
    sqlx::query("UPDATE principals SET sync_config_cache_version = sync_config_cache_version + 1")
        .execute(conn)
        .await?;
    Ok(())
}

#[cfg(all(test, feature = "testing"))]
mod tests {
    use super::*;
    use crate::testing::TestDb;

    #[tokio::test]
    async fn bumps_only_the_named_principals() {
        let Some(db) = TestDb::migrated().await else {
            return;
        };
        let mut conn = db.pool.acquire().await.unwrap();
        let user: i64 = sqlx::query_scalar(
            "INSERT INTO users (email, status, admin, created_at, updated_at) \
             VALUES ('admin@example.test', 'active', true, now(), now()) RETURNING id",
        )
        .fetch_one(&mut *conn)
        .await
        .unwrap();
        for name in ["a", "b"] {
            sqlx::query(
                "INSERT INTO principals (kind, name, created_by_id, created_at, updated_at) \
                 VALUES ('slack_channel', $1, $2, now(), now())",
            )
            .bind(name)
            .bind(user)
            .execute(&mut *conn)
            .await
            .unwrap();
        }
        let ids: Vec<i64> = sqlx::query_scalar("SELECT id FROM principals ORDER BY name")
            .fetch_all(&mut *conn)
            .await
            .unwrap();
        bump_sync_config(&mut conn, &ids[..1]).await.unwrap();
        bump_sync_config(&mut conn, &[]).await.unwrap();
        let versions: Vec<i64> = sqlx::query_scalar(
            "SELECT sync_config_cache_version::bigint FROM principals ORDER BY name",
        )
        .fetch_all(&mut *conn)
        .await
        .unwrap();
        assert_eq!(versions[0], versions[1] + 1);
        bump_all_sync_config(&mut conn).await.unwrap();
        assert!(role_member_ids(&mut conn, 0).await.unwrap().is_empty());
        assert!(
            grantee_ids(&mut conn, "static_secret_id", 0)
                .await
                .unwrap()
                .is_empty()
        );
    }
}
