//! The sandbox API's credentials: the entitlement JWT proxy-sync mints
//! (HS256, `iss`/`aud` from configuration) bound to the database rows it
//! names, and sandbox admin (a sandbox acting from an active admin's Slack
//! DM in the bot's own workspace).

use chrono::{DateTime, Utc};
use http::HeaderMap;
use jsonwebtoken::{Algorithm, DecodingKey, Validation};
use serde::Deserialize;
use sqlx::PgPool;

use super::{
    AuthError, AuthUser, Denial, NotConfigured, bearer::bearer_token, tokens::SandboxJwtConfig,
};

/// The verified claims of an entitlement token.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct SandboxClaims {
    #[serde(default)]
    pub sandbox_id: String,
    #[serde(default)]
    pub proxy_id: String,
    #[serde(default)]
    pub principal_id: String,
    exp: i64,
}

/// Verifies signature, `iss`, `aud`, and `exp` (no database).
pub fn verify_token(
    token: &str,
    secret: &str,
    config: &SandboxJwtConfig,
    now: DateTime<Utc>,
) -> Result<Option<SandboxClaims>, NotConfigured> {
    if secret.trim().is_empty() {
        return Err(NotConfigured);
    }
    let mut validation = Validation::new(Algorithm::HS256);
    validation.set_issuer(&[&config.issuer]);
    validation.set_audience(&[&config.audience]);
    validation.validate_exp = false;
    validation.required_spec_claims.clear();
    Ok(jsonwebtoken::decode::<SandboxClaims>(
        token,
        &DecodingKey::from_secret(secret.as_bytes()),
        &validation,
    )
    .ok()
    .map(|data| data.claims)
    .filter(|claims| claims.exp > now.timestamp()))
}

/// A sandbox authenticated by its entitlement token: the proxy it runs
/// behind and the principal that proxy is assigned to (re-read from the
/// database on every request).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SandboxAuth {
    pub proxy_id: i64,
    pub proxy_name: String,
    pub principal_id: i64,
    pub principal_kind: String,
    pub console_user_id: Option<i64>,
    pub slack_team_id: Option<String>,
}

/// A sandbox acting for an active admin from that admin's Slack DM.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SandboxAdminAuth {
    pub sandbox: SandboxAuth,
    pub admin: AuthUser,
}

#[derive(sqlx::FromRow)]
struct Binding {
    proxy_id: i64,
    proxy_name: String,
    principal_id: i64,
    kind: String,
    console_user_id: Option<i64>,
    slack_team_id: Option<String>,
}

/// The token must verify, and the proxy and principal it names must exist,
/// be assigned to each other, and carry the sandbox name it names.
pub async fn authenticate(
    pool: &PgPool,
    headers: &HeaderMap,
    secret: &str,
    config: &SandboxJwtConfig,
    now: DateTime<Utc>,
) -> Result<Result<SandboxAuth, Denial>, AuthError> {
    let Some(token) = bearer_token(headers) else {
        return Ok(Err(Denial::SANDBOX_TOKEN_MISSING));
    };
    let Some(claims) = verify_token(token, secret, config, now)? else {
        return Ok(Err(Denial::SANDBOX_TOKEN_MISSING));
    };
    let ids = (
        centaur_console_db::decode_oid("prx", &claims.proxy_id),
        centaur_console_db::decode_oid("prn", &claims.principal_id),
    );
    let (Some(proxy_id), Some(principal_id)) = ids else {
        return Ok(Err(Denial::SANDBOX_TOKEN_INVALID));
    };
    let binding: Option<Binding> = sqlx::query_as(
        "SELECT px.id AS proxy_id, px.name AS proxy_name, pr.id AS principal_id, pr.kind, \
                pr.console_user_id, pr.slack_team_id \
         FROM proxies px JOIN principals pr ON pr.id = px.principal_id \
         WHERE px.id = $1 AND pr.id = $2 AND px.name = $3",
    )
    .bind(proxy_id)
    .bind(principal_id)
    .bind(&claims.sandbox_id)
    .fetch_optional(pool)
    .await?;
    Ok(match binding {
        Some(binding) => Ok(SandboxAuth {
            proxy_id: binding.proxy_id,
            proxy_name: binding.proxy_name,
            principal_id: binding.principal_id,
            principal_kind: binding.kind,
            console_user_id: binding.console_user_id,
            slack_team_id: binding.slack_team_id,
        }),
        None => Err(Denial::SANDBOX_TOKEN_INVALID),
    })
}

/// A valid sandbox token whose principal is a Slack DM principal in the
/// bot's own workspace, linked to an active admin.
pub async fn authenticate_admin(
    pool: &PgPool,
    headers: &HeaderMap,
    secret: &str,
    config: &SandboxJwtConfig,
    now: DateTime<Utc>,
) -> Result<Result<SandboxAdminAuth, Denial>, AuthError> {
    let sandbox = match authenticate(pool, headers, secret, config, now).await? {
        Ok(sandbox) => sandbox,
        Err(denial) => return Ok(Err(denial)),
    };
    let admin = match (sandbox.console_user_id, sandbox.slack_team_id.as_deref()) {
        (Some(user_id), Some(team)) if sandbox.principal_kind == "slack_dm" => {
            let home_team: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM slack_bot_channels WHERE team_id = $1)",
            )
            .bind(team)
            .fetch_one(pool)
            .await?;
            if home_team {
                AuthUser::find(pool, user_id)
                    .await?
                    .filter(|user| user.is_active() && user.admin)
            } else {
                None
            }
        }
        _ => None,
    };
    Ok(match admin {
        Some(admin) => Ok(SandboxAdminAuth { sandbox, admin }),
        None => Err(Denial::SANDBOX_ADMIN_REQUIRED),
    })
}

#[cfg(test)]
mod tests {
    use chrono::Duration;

    use super::*;
    use crate::tokens::{sandbox_entitlement, sign};

    const SECRET: &str = "sandbox-secret";

    #[test]
    fn verifies_what_proxy_sync_mints() {
        let config = SandboxJwtConfig::default();
        let now = Utc::now();
        let token =
            sandbox_entitlement(SECRET, &config, "prx_abc", "sbx-1", "prn_def", now).unwrap();
        let claims = verify_token(&token, SECRET, &config, now).unwrap().unwrap();
        assert_eq!(
            (
                claims.sandbox_id.as_str(),
                claims.proxy_id.as_str(),
                claims.principal_id.as_str()
            ),
            ("sbx-1", "prx_abc", "prn_def")
        );
        // Wrong secret, audience, issuer, or an expired token: no claims.
        assert_eq!(verify_token(&token, "other", &config, now).unwrap(), None);
        let other = SandboxJwtConfig::new(Some("someone-else"), None);
        assert_eq!(verify_token(&token, SECRET, &other, now).unwrap(), None);
        let other = SandboxJwtConfig::new(None, Some("someone-else"));
        assert_eq!(verify_token(&token, SECRET, &other, now).unwrap(), None);
        let later = now + Duration::days(4);
        assert_eq!(verify_token(&token, SECRET, &config, later).unwrap(), None);
        assert_eq!(verify_token("junk", SECRET, &config, now).unwrap(), None);
        // Without `exp` it never verifies.
        let no_exp = sign(
            SECRET,
            &serde_json::json!({"iss": config.issuer, "aud": config.audience, "proxy_id": "x"}),
        )
        .unwrap();
        assert_eq!(verify_token(&no_exp, SECRET, &config, now).unwrap(), None);
        // A missing secret is a configuration error, not a bad token.
        assert!(verify_token(&token, " ", &config, now).is_err());
    }
}
