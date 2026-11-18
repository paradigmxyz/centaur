//! The grantable static secret that wraps a minted broker credential, so an
//! operator can grant the integration's token to a principal. Created once
//! per credential and left alone on re-consent, so operator edits survive.
//!
//! The rows come from the provider's fixed configuration, which always
//! satisfies the static-secret rules (custom kind: an Authorization header
//! injector plus one rule per API host; `github_token`: the canonical
//! placeholder replacement and GitHub's hosts), so they are written without
//! running the secrets area's validations.

use chrono::NaiveDateTime;
use serde_json::json;
use sqlx::PgConnection;

use super::providers::{Provider, RuleSpec};
use crate::ApiError;

const GITHUB_HOSTS: [&str; 3] = ["api.github.com", "github.com", "api.githubcopilot.com"];

/// Creates the wrapper (secret, token_broker source, request rules) unless
/// the credential has one. Returns whether it was created.
pub async fn ensure(
    conn: &mut PgConnection,
    provider: &Provider,
    credential_id: i64,
    credential_oid: &str,
    credential_name: Option<&str>,
    now: NaiveDateTime,
) -> Result<bool, ApiError> {
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM static_secrets WHERE broker_credential_id = $1)",
    )
    .bind(credential_id)
    .fetch_one(&mut *conn)
    .await?;
    if exists {
        return Ok(false);
    }
    let kind = provider.wrapping_secret_kind();
    let (inject_config, replace_config, rules) = if kind == "custom" {
        let rules = provider.credential_request_rules().ok_or_else(|| {
            ApiError::internal("provider has no API hosts for its wrapping secret")
        })?;
        (
            Some(json!({"header": "Authorization", "formatter": "Bearer {{ .Value }}"})),
            None,
            rules,
        )
    } else {
        (
            None,
            Some(
                json!({"proxy_value": "GITHUB_TOKEN", "match_headers": ["Authorization"], "require": false}),
            ),
            GITHUB_HOSTS
                .iter()
                .map(|host| RuleSpec {
                    host: (*host).to_owned(),
                    http_methods: Vec::new(),
                    paths: Vec::new(),
                })
                .collect(),
        )
    };
    let secret_id: i64 = sqlx::query_scalar(
        "INSERT INTO static_secrets (broker_credential_id, kind, name, inject_config, replace_config, \
         labels, created_at, updated_at) VALUES ($1, $2, $3, $4, $5, '{}', $6, $6) RETURNING id",
    )
    .bind(credential_id)
    .bind(kind)
    .bind(format!("{} token", credential_name.unwrap_or_default()))
    .bind(inject_config)
    .bind(replace_config)
    .bind(now)
    .fetch_one(&mut *conn)
    .await?;
    sqlx::query(
        "INSERT INTO secret_sources (source_type, broker_credential_id, static_secret_id, config, \
         created_at, updated_at) VALUES ('token_broker', $1, $2, $3, $4, $4)",
    )
    .bind(credential_id)
    .bind(secret_id)
    .bind(json!({"credential_id": credential_oid}))
    .bind(now)
    .execute(&mut *conn)
    .await?;
    for (position, rule) in rules.iter().enumerate() {
        sqlx::query(
            "INSERT INTO request_rules (static_secret_id, host, http_methods, paths, position, \
             created_at, updated_at) VALUES ($1, $2, $3, $4, $5, $6, $6)",
        )
        .bind(secret_id)
        .bind(&rule.host)
        .bind(json!(rule.http_methods))
        .bind(json!(rule.paths))
        .bind(i32::try_from(position).unwrap_or(i32::MAX))
        .bind(now)
        .execute(&mut *conn)
        .await?;
    }
    Ok(true)
}
