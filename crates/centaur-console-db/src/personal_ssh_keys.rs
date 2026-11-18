//! Granting a user's personal SSH key to the principals that verifiably are
//! that user: their console principal and their Slack DM principal.
//!
//! Shared by the console's personal-key endpoints, sign-in (a Slack identity
//! was linked) and principal creation. Every function is idempotent and
//! invalidates the synced config of principals it granted.

use chrono::{DateTime, Utc};
use sqlx::{PgConnection, PgPool};

/// `principals.kind` of a console user's own principal.
pub const CONSOLE_USER_KIND: &str = "console_user";
/// `principals.kind` of a Slack DM principal.
pub const SLACK_DM_KIND: &str = "slack_dm";
/// The priority of a direct grant.
const DIRECT_PRIORITY: i32 = 100;

fn slack_user_id_valid(value: &str) -> bool {
    value == "USLACK"
        || (value.len() >= 9
            && matches!(value.as_bytes()[0], b'U' | b'W')
            && value.bytes().skip(1).all(is_upper_alnum))
}

fn slack_team_id_valid(value: &str) -> bool {
    value.len() >= 9
        && matches!(value.as_bytes()[0], b'T' | b'E')
        && value.bytes().skip(1).all(is_upper_alnum)
}

fn is_upper_alnum(byte: u8) -> bool {
    byte.is_ascii_uppercase() || byte.is_ascii_digit()
}

/// The user's single, well-formed Slack identity `(user id, team id)`; none
/// when they have no Slack identity, several, or a malformed one.
async fn slack_identity(
    conn: &mut PgConnection,
    user_id: i64,
) -> crate::Result<Option<(String, String)>> {
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT DISTINCT subject, team_id FROM user_identities \
         WHERE user_id = $1 AND provider = 'slack' AND team_id IS NOT NULL",
    )
    .bind(user_id)
    .fetch_all(conn)
    .await?;
    Ok(match rows.as_slice() {
        [(subject, team)] if slack_user_id_valid(subject) && slack_team_id_valid(team) => {
            Some((subject.clone(), team.clone()))
        }
        _ => None,
    })
}

/// Grants the key of `owner_id` to `principal_id` unless it has it; whether
/// a grant was created. A no-op for an owner without a key or not active.
async fn grant(
    pool: &PgPool,
    principal_id: i64,
    owner_id: i64,
    now: DateTime<Utc>,
) -> crate::Result<bool> {
    let mut tx = pool.begin().await?;
    let inserted = sqlx::query(
        "INSERT INTO grants (principal_id, ssh_key_secret_id, priority, created_by_id, created_at, updated_at) \
         SELECT $1, k.id, $3, k.owner_id, $4, $4 FROM ssh_key_secrets k \
         JOIN users u ON u.id = k.owner_id AND u.status = 'active' \
         WHERE k.owner_id = $2 \
         ON CONFLICT DO NOTHING",
    )
    .bind(principal_id)
    .bind(owner_id)
    .bind(DIRECT_PRIORITY)
    .bind(now.naive_utc())
    .execute(&mut *tx)
    .await?
    .rows_affected()
        > 0;
    if inserted {
        crate::sync_config::bump_sync_config(&mut tx, &[principal_id]).await?;
    }
    tx.commit().await?;
    Ok(inserted)
}

/// Grants the user's personal key to their console principal and their
/// verified Slack DM principal; the number of grants created.
pub async fn apply_for_user(
    pool: &PgPool,
    user_id: i64,
    now: DateTime<Utc>,
) -> crate::Result<usize> {
    let identity = slack_identity(&mut *pool.acquire().await?, user_id).await?;
    let principal_ids: Vec<i64> = sqlx::query_scalar(
        "SELECT id FROM principals WHERE (kind = $1 AND console_user_id = $2) \
         OR ($3::text IS NOT NULL AND kind = $4 AND slack_user_id = $3 AND slack_team_id = $5) \
         ORDER BY id",
    )
    .bind(CONSOLE_USER_KIND)
    .bind(user_id)
    .bind(identity.as_ref().map(|(subject, _)| subject.as_str()))
    .bind(SLACK_DM_KIND)
    .bind(identity.as_ref().map(|(_, team)| team.as_str()))
    .fetch_all(pool)
    .await?;
    let mut created = 0;
    for principal_id in principal_ids {
        if grant(pool, principal_id, user_id, now).await? {
            created += 1;
        }
    }
    Ok(created)
}

/// Grants the personal key of the user a console or Slack DM principal
/// verifiably belongs to; whether a grant was created. Call it when such a
/// principal is created or re-identified.
pub async fn apply_for_principal(
    pool: &PgPool,
    principal_id: i64,
    now: DateTime<Utc>,
) -> crate::Result<bool> {
    #[derive(sqlx::FromRow)]
    struct Identity {
        kind: String,
        console_user_id: Option<i64>,
        slack_user_id: Option<String>,
        slack_team_id: Option<String>,
    }
    let row: Option<Identity> = sqlx::query_as(
        "SELECT kind, console_user_id, slack_user_id, slack_team_id FROM principals WHERE id = $1",
    )
    .bind(principal_id)
    .fetch_optional(pool)
    .await?;
    let Some(principal) = row else {
        return Ok(false);
    };
    let owner = match principal.kind.as_str() {
        CONSOLE_USER_KIND => principal.console_user_id,
        SLACK_DM_KIND => {
            let (Some(subject), Some(team)) = (principal.slack_user_id, principal.slack_team_id)
            else {
                return Ok(false);
            };
            let user_id: Option<i64> = sqlx::query_scalar(
                "SELECT user_id FROM user_identities \
                 WHERE provider = 'slack' AND subject = $1 AND team_id = $2 LIMIT 1",
            )
            .bind(&subject)
            .bind(&team)
            .fetch_optional(pool)
            .await?;
            match user_id {
                Some(user_id)
                    if slack_identity(&mut *pool.acquire().await?, user_id).await?
                        == Some((subject, team)) =>
                {
                    Some(user_id)
                }
                _ => None,
            }
        }
        _ => None,
    };
    match owner {
        Some(owner) => grant(pool, principal_id, owner, now).await,
        None => Ok(false),
    }
}
