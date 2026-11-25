//! Post-connect Slack identity enrichment.
//!
//! After a Slack consent, it renames the broker credential and its auto-created
//! wrapper secret from the Slack profile. Its failure can never fail the
//! consent, so the flow runs it inline and logs any failure instead of
//! surfacing it. Live exchanges and both profile calls use the same
//! [`super::http`] client as the rest of the OAuth area.

use std::time::Duration;

use serde_json::Value;
use sqlx::PgPool;

use centaur_console_db::areas::oauth::{
    OauthEncryption, apply_enriched_identity, find_enrichment_credential,
};

use super::{http, providers::SLACK_KEY, slack_api_endpoint};

const AUTH_TEST_PATH: &str = "auth.test";
const USERS_INFO_PATH: &str = "users.info";
const PROFILE_TIMEOUT: Duration = Duration::from_secs(5);
const PROFILE_MAX_BODY_BYTES: usize = 64 * 1024;

#[derive(Debug, Default)]
struct SlackProfile {
    name: Option<String>,
    email: Option<String>,
}

/// Runs enrichment for a freshly connected credential. It
/// returns an error only so the caller can log it; the credential is already
/// connected and the caller must ignore the failure.
pub(super) async fn enrich_after_connect(pool: &PgPool, credential_id: i64) -> Result<(), String> {
    let credential = find_enrichment_credential(pool, credential_id)
        .await
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "credential disappeared before enrichment".to_owned())?;
    if credential.provider != SLACK_KEY {
        return Ok(());
    }
    let Some(subject) = credential
        .provider_subject
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(());
    };
    let Some(ciphertext) = credential
        .access_token
        .as_deref()
        .filter(|value| !value.is_empty())
    else {
        return Ok(());
    };
    let encryption = OauthEncryption::from_env()
        .ok_or_else(|| "console encryption keys are not configured".to_owned())?;
    let access_token = encryption
        .decrypt(ciphertext)
        .map_err(|error| error.to_string())?;

    let profile = slack_profile(&access_token, subject, &credential.scopes).await;
    let Some(display_name) = profile
        .name
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            profile
                .email
                .as_deref()
                .filter(|value| !value.trim().is_empty())
        })
    else {
        return Ok(());
    };
    let name = format!("Slack – {display_name}");
    let provider_email = profile
        .email
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .or(credential.provider_email.as_deref());
    apply_enriched_identity(pool, &credential, &name, provider_email)
        .await
        .map_err(|error| error.to_string())
}

/// The Slack profile lookup is best-effort. A transport or
/// payload failure returns whatever was learned so far.
async fn slack_profile(access_token: &str, subject: &str, scopes: &[String]) -> SlackProfile {
    let mut profile = SlackProfile::default();
    profile.name = match auth_test_user(access_token).await {
        Ok(name) => name,
        Err(error) => {
            tracing::debug!(error = %error, "slack oauth profile lookup failed");
            return profile;
        }
    };
    if !scopes.iter().any(|scope| scope == "users:read") {
        return profile;
    }

    let info = match slack_api(
        USERS_INFO_PATH,
        access_token,
        Some(vec![("user".to_owned(), subject.to_owned())]),
    )
    .await
    {
        Ok(Some(info)) => info,
        Ok(None) => return profile,
        Err(error) => {
            tracing::debug!(error = %error, "slack oauth profile lookup failed");
            return profile;
        }
    };
    if info.get("ok") != Some(&Value::Bool(true)) {
        return profile;
    }
    let user = info.get("user").and_then(Value::as_object);
    let user_profile = user
        .and_then(|user| user.get("profile"))
        .and_then(Value::as_object);
    profile.name = present_string(user_profile.and_then(|profile| profile.get("display_name")))
        .or_else(|| present_string(user_profile.and_then(|profile| profile.get("real_name"))))
        .or_else(|| present_string(user.and_then(|user| user.get("real_name"))))
        .or_else(|| present_string(user.and_then(|user| user.get("name"))))
        .or(profile.name.take());
    if scopes.iter().any(|scope| scope == "users:read.email") {
        profile.email = present_string(user_profile.and_then(|profile| profile.get("email")));
    }
    profile
}

/// Slack's `auth.test` username, or `None` unless the call
/// reported `"ok": true`.
async fn auth_test_user(access_token: &str) -> Result<Option<String>, reqwest::Error> {
    let Some(response) = slack_api(AUTH_TEST_PATH, access_token, None).await? else {
        return Ok(None);
    };
    if response.get("ok") != Some(&Value::Bool(true)) {
        return Ok(None);
    }
    Ok(present_string(response.get("user")))
}

/// A bearer POST whose JSON body is optional. HTTP status is not
/// interpreted here; Slack reports failure in the body.
async fn slack_api(
    path: &str,
    access_token: &str,
    form: Option<Vec<(String, String)>>,
) -> Result<Option<Value>, reqwest::Error> {
    let url = slack_api_endpoint(path);
    let response = http::send(
        reqwest::Method::POST,
        &url,
        &[http::bearer(access_token)],
        form.as_ref(),
        None,
        PROFILE_TIMEOUT,
        PROFILE_MAX_BODY_BYTES,
    )
    .await?;
    Ok(response.json())
}

/// `None` for a missing or blank string.
fn present_string(value: Option<&Value>) -> Option<String> {
    match value {
        Some(Value::String(value)) if !value.trim().is_empty() => Some(value.clone()),
        Some(Value::Number(value)) => Some(value.to_string()),
        _ => None,
    }
}
