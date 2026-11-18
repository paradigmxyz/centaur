//! The principal a console user's own work runs as (MCP sessions, the
//! user's scheduled tasks): a `console_user` principal with a stable
//! foreign id, linked to the user, labelled `managed-by=centaur`, carrying
//! the user's Slack identity when it is unambiguous, and holding the
//! `user-mcp` role.

use centaur_console_db::sync_config::bump_sync_config;
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::PgConnection;

use super::{
    effects::{self, Committed},
    payload::REPO_CACHE_LABEL,
    reconcile::slack_identities,
};
use crate::{ApiResult, AppState};

pub const USER_MCP_ROLE_FOREIGN_ID: &str = "user-mcp";

/// The user a principal is provisioned for.
pub struct ConsoleUser<'a> {
    pub id: i64,
    pub email: &'a str,
    pub name: Option<&'a str>,
}

/// `console-user-<safe email>-<digest>`: URL-safe, at most 48 characters of
/// the email, and a digest so distinct emails never collide.
pub fn console_user_foreign_id(email: &str) -> String {
    let normalized = email.trim().to_lowercase();
    let mut safe = String::new();
    for c in normalized.chars() {
        let c = if c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_' | '~') {
            c
        } else {
            '-'
        };
        if c == '-' && safe.ends_with('-') {
            continue;
        }
        safe.push(c);
    }
    let safe: String = safe.chars().take(48).collect();
    let digest = hex::encode(Sha256::digest(normalized.as_bytes()));
    format!("console-user-{safe}-{}", &digest[..12])
}

/// What provisioning did.
#[derive(Debug)]
pub struct Provisioned {
    pub principal_id: i64,
    /// The principal row was created or changed (the follow-ups are due).
    pub saved: bool,
    pub created: bool,
}

#[derive(sqlx::FromRow)]
struct Existing {
    id: i64,
    name: Option<String>,
    kind: String,
    labels: Value,
    slack_user_id: Option<String>,
    slack_team_id: Option<String>,
    console_user_id: Option<i64>,
    sandbox_repo_cache: String,
}

/// Creates or brings up to date the user's principal, inside the caller's
/// transaction. The caller runs [`effects::principal_committed`] once it
/// has committed, when [`Provisioned::saved`].
pub async fn provision_in(
    conn: &mut PgConnection,
    user: &ConsoleUser<'_>,
    now: DateTime<Utc>,
) -> centaur_console_db::Result<Provisioned> {
    let foreign_id = console_user_foreign_id(user.email);
    let name = user
        .name
        .filter(|name| !name.trim().is_empty())
        .unwrap_or(user.email);
    let slack = slack_identities(&mut *conn, &[user.id])
        .await?
        .remove(&user.id);
    let now = now.naive_utc();

    let mut attempts = 0;
    let (principal_id, saved, created) = loop {
        attempts += 1;
        let existing: Option<Existing> = sqlx::query_as(
            "SELECT id, name, kind, labels, slack_user_id, slack_team_id, console_user_id, \
             sandbox_repo_cache FROM principals WHERE foreign_id = $1 FOR UPDATE",
        )
        .bind(&foreign_id)
        .fetch_optional(&mut *conn)
        .await?;
        if let Some(row) = existing {
            let (slack_user_id, slack_team_id) = match &slack {
                Some((user_id, team_id)) => (Some(user_id.clone()), Some(team_id.clone())),
                None => (row.slack_user_id.clone(), row.slack_team_id.clone()),
            };
            let mut labels = row.labels.as_object().cloned().unwrap_or_default();
            labels.insert("managed-by".into(), "centaur".into());
            labels.insert(
                REPO_CACHE_LABEL.into(),
                row.sandbox_repo_cache.clone().into(),
            );
            let labels = Value::Object(labels);
            let changed = row.name.as_deref() != Some(name)
                || row.kind != "console_user"
                || row.console_user_id != Some(user.id)
                || row.slack_user_id != slack_user_id
                || row.slack_team_id != slack_team_id
                || row.labels != labels;
            if changed {
                sqlx::query(
                    "UPDATE principals SET name = $2, kind = 'console_user', console_user_id = $3, \
                     slack_user_id = $4, slack_team_id = $5, labels = $6, updated_at = $7 \
                     WHERE id = $1",
                )
                .bind(row.id)
                .bind(name)
                .bind(user.id)
                .bind(slack_user_id)
                .bind(slack_team_id)
                .bind(labels)
                .bind(now)
                .execute(&mut *conn)
                .await?;
                bump_sync_config(&mut *conn, &[row.id]).await?;
            }
            break (row.id, changed, false);
        }
        let (slack_user_id, slack_team_id) = slack.clone().unzip();
        let inserted: Option<i64> = sqlx::query_scalar(
            "INSERT INTO principals (foreign_id, name, kind, console_user_id, labels, \
             slack_user_id, slack_team_id, created_by_id, created_at, updated_at) \
             VALUES ($1, $2, 'console_user', $3, $4, $5, $6, $3, $7, $7) \
             ON CONFLICT (foreign_id) DO NOTHING RETURNING id",
        )
        .bind(&foreign_id)
        .bind(name)
        .bind(user.id)
        .bind(json!({"managed-by": "centaur", REPO_CACHE_LABEL: "all"}))
        .bind(slack_user_id)
        .bind(slack_team_id)
        .bind(now)
        .fetch_optional(&mut *conn)
        .await?;
        match inserted {
            Some(id) => {
                sqlx::query(
                    "INSERT INTO principal_roles (principal_id, role_id, created_at, updated_at) \
                     SELECT $1, id, $2, $2 FROM roles WHERE assign_by_default ORDER BY id",
                )
                .bind(id)
                .bind(now)
                .execute(&mut *conn)
                .await?;
                break (id, true, true);
            }
            None if attempts < 5 => {}
            None => {
                return Err(centaur_console_db::Error::InvalidData(
                    "console user principal kept conflicting".into(),
                ));
            }
        }
    };
    if created {
        assign_user_mcp_role(conn, principal_id, user.id, now).await?;
    }
    Ok(Provisioned {
        principal_id,
        saved,
        created,
    })
}

async fn assign_user_mcp_role(
    conn: &mut PgConnection,
    principal_id: i64,
    user_id: i64,
    now: chrono::NaiveDateTime,
) -> centaur_console_db::Result<()> {
    sqlx::query(
        "INSERT INTO roles (foreign_id, name, labels, created_by_id, created_at, updated_at) \
         VALUES ($1, 'User MCP', '{\"managed-by\": \"centaur\"}', $2, $3, $3) \
         ON CONFLICT (foreign_id) DO NOTHING",
    )
    .bind(USER_MCP_ROLE_FOREIGN_ID)
    .bind(user_id)
    .bind(now)
    .execute(&mut *conn)
    .await?;
    let assigned = sqlx::query(
        "INSERT INTO principal_roles (principal_id, role_id, created_at, updated_at) \
         SELECT $1, id, $3, $3 FROM roles WHERE foreign_id = $2 ON CONFLICT DO NOTHING",
    )
    .bind(principal_id)
    .bind(USER_MCP_ROLE_FOREIGN_ID)
    .bind(now)
    .execute(&mut *conn)
    .await?
    .rows_affected();
    if assigned > 0 {
        bump_sync_config(conn, &[principal_id]).await?;
    }
    Ok(())
}

/// Provisions the user's principal in its own transaction, then runs the
/// follow-ups. Returns the principal's id.
pub async fn provision(state: &AppState, user: &ConsoleUser<'_>) -> ApiResult<i64> {
    let mut tx = state.db.begin().await?;
    let provisioned = provision_in(&mut tx, user, state.now()).await?;
    tx.commit().await?;
    if provisioned.saved {
        effects::principal_committed(
            state,
            Committed {
                principal_id: provisioned.principal_id,
                created: provisioned.created,
                identity_changed: true,
                slack_channel_id: None,
            },
        )
        .await;
    }
    Ok(provisioned.principal_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn foreign_ids_are_url_safe_and_digest_suffixed() {
        let id = console_user_foreign_id(" Ada.Lovelace+x@Example.COM ");
        assert!(id.starts_with("console-user-ada.lovelace-x-example.com-"));
        let digest = id.rsplit('-').next().unwrap();
        assert_eq!(digest.len(), 12);
        assert_ne!(
            console_user_foreign_id("a@b.test"),
            console_user_foreign_id("a-b.test")
        );
        let long = console_user_foreign_id(&format!("{}@example.test", "x".repeat(80)));
        assert!(long.len() <= "console-user-".len() + 48 + 1 + 12);
    }
}
