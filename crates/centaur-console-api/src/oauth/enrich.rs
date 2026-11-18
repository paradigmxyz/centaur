//! Slack identity enrichment, a best-effort task spawned from the consent
//! callback: renames a Slack credential (and its untouched wrapping secret)
//! after the account's Slack profile and records the account's email.
//! Failures are logged at debug level and swallowed; the credential keeps its
//! consent-time name.
//!
//! The upstream calls take time, so nothing read before them is written
//! back: the update touches only `name`, `provider_email`, and `updated_at`
//! of the credential row, so a token refresh that lands meanwhile survives.

use centaur_console_db::Ciphertext;
use serde_json::Value;

use super::{
    credentials,
    exchange::{send, text},
    upstream_url,
};
use crate::{ApiError, AppState};

const AUTH_TEST: &str = "https://slack.com/api/auth.test";
const USERS_INFO: &str = "https://slack.com/api/users.info";

/// Runs [`run`] in the background.
pub fn spawn(state: AppState, credential_id: i64) {
    tokio::spawn(async move {
        if let Err(error) = run(&state, credential_id).await {
            tracing::debug!(credential_id, %error, "slack oauth identity enrichment failed");
        }
    });
}

#[derive(Default)]
struct Profile {
    name: Option<String>,
    email: Option<String>,
}

async fn slack_call(state: &AppState, url: &str, token: &str, user: Option<&str>) -> Option<Value> {
    let mut request = state.http.post(upstream_url(state, url)).bearer_auth(token);
    if let Some(user) = user {
        request = request.form(&[("user", user)]);
    }
    send(request).await.ok()?.json()
}

/// What Slack says about the account; whatever was learned before a call
/// failed is kept.
async fn profile(state: &AppState, token: &str, user_id: &str, scopes: &[String]) -> Profile {
    let mut profile = Profile::default();
    let Some(auth) = slack_call(state, AUTH_TEST, token, None).await else {
        return profile;
    };
    if auth.get("ok") == Some(&Value::Bool(true)) {
        profile.name = text(auth.get("user"));
    }
    if !scopes.iter().any(|scope| scope == "users:read") {
        return profile;
    }
    let Some(info) = slack_call(state, USERS_INFO, token, Some(user_id)).await else {
        return profile;
    };
    if info.get("ok") != Some(&Value::Bool(true)) {
        return profile;
    }
    let user = info.get("user").unwrap_or(&Value::Null);
    let user_profile = user.get("profile").unwrap_or(&Value::Null);
    profile.name = text(user_profile.get("display_name"))
        .or_else(|| text(user_profile.get("real_name")))
        .or_else(|| text(user.get("real_name")))
        .or_else(|| text(user.get("name")))
        .or(profile.name);
    if scopes.iter().any(|scope| scope == "users:read.email") {
        profile.email = text(user_profile.get("email"));
    }
    profile
}

#[derive(sqlx::FromRow)]
struct Credential {
    name: Option<String>,
    access_token: Option<Ciphertext>,
    provider_subject: Option<String>,
    scopes: Value,
}

/// Enriches one credential. Anything but a Slack credential with an access
/// token and an account is left alone.
pub async fn run(state: &AppState, credential_id: i64) -> Result<(), ApiError> {
    let credential: Option<Credential> = sqlx::query_as(
        "SELECT bc.name, bc.access_token, bc.provider_subject, bc.scopes \
         FROM broker_credentials bc JOIN oauth_apps oa ON oa.id = bc.oauth_app_id \
         WHERE bc.id = $1 AND oa.provider = 'slack'",
    )
    .bind(credential_id)
    .fetch_optional(&state.db)
    .await?;
    let Some(credential) = credential else {
        return Ok(());
    };
    let token = state
        .encryptor
        .decrypt_optional(credential.access_token.as_ref())?
        .filter(|token| !token.trim().is_empty());
    let (Some(token), Some(subject)) = (
        token,
        credential
            .provider_subject
            .filter(|subject| !subject.trim().is_empty()),
    ) else {
        return Ok(());
    };
    let scopes: Vec<String> = serde_json::from_value(credential.scopes).unwrap_or_default();
    let profile = profile(state, &token, &subject, &scopes).await;
    let Some(display_name) = profile.name.clone().or_else(|| profile.email.clone()) else {
        return Ok(());
    };
    let new_name = format!("Slack – {display_name}");
    let now = state.now().naive_utc();
    let old_name = credential.name.filter(|name| !name.trim().is_empty());

    let mut tx = state.db.begin().await?;
    sqlx::query(
        "UPDATE broker_credentials SET name = $2, provider_email = COALESCE($3, provider_email), \
         updated_at = $4 WHERE id = $1",
    )
    .bind(credential_id)
    .bind(&new_name)
    .bind(&profile.email)
    .bind(now)
    .execute(&mut *tx)
    .await?;
    // The wrapping secret follows the credential's name unless an operator
    // renamed it.
    let renamed: Option<i64> = sqlx::query_scalar(
        "UPDATE static_secrets SET name = $3, updated_at = $4 \
         WHERE broker_credential_id = $1 AND ($2::text IS NULL OR name = $2) \
           AND name IS DISTINCT FROM $3 RETURNING id",
    )
    .bind(credential_id)
    .bind(old_name.map(|old| format!("{old} token")))
    .bind(format!("{new_name} token"))
    .bind(now)
    .fetch_optional(&mut *tx)
    .await?;
    if renamed.is_some() {
        credentials::bump_grantees(&mut tx, credential_id).await?;
    }
    tx.commit().await?;
    crate::principals::effects::credential_changed(state, credential_id).await;
    Ok(())
}
