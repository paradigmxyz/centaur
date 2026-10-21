//! Token requests for each `broker_credentials.grant`.

use std::{collections::HashMap, sync::LazyLock, time::Duration};

use base64::{Engine, engine::general_purpose::STANDARD};
use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderName, HeaderValue};
use serde::Deserialize;
use tracing::warn;
use url::{Url, form_urlencoded::byte_serialize};

use crate::client::{RefreshError, TokenClient, TokenRequest, TokenResponse};

/// A locked credential with its secrets decrypted. Flow-minted credentials
/// carry their OAuth app's provider and client credentials.
pub(crate) struct Credential {
    pub oid: String,
    pub grant: String,
    pub token_endpoint: String,
    pub provider: Option<String>,
    pub client_id: Option<String>,
    pub client_secret: Option<String>,
    pub refresh_token: Option<String>,
    pub username: Option<String>,
    pub password: Option<String>,
    pub api_key: Option<String>,
    pub token_endpoint_headers: Vec<(String, String)>,
    pub scopes: Vec<String>,
    pub timeout: Duration,
}

impl Credential {
    pub fn provider_label(&self) -> String {
        provider_label(self.provider.as_deref(), &self.token_endpoint)
    }
}

/// The `provider` metrics label: the OAuth app's provider, or for credentials
/// without one, the token endpoint's host.
pub(crate) fn provider_label(provider: Option<&str>, token_endpoint: &str) -> String {
    provider
        .map(str::to_owned)
        .or_else(|| {
            Url::parse(token_endpoint)
                .ok()?
                .host_str()
                .map(str::to_owned)
        })
        .unwrap_or_else(|| "unknown".to_owned())
}

pub(crate) enum Outcome {
    Refreshed {
        token: TokenResponse,
        /// Drop the stored refresh token: the endpoint rejected it and the
        /// fallback grant that succeeded did not issue a new one.
        clear_refresh_token: bool,
    },
    Dead(&'static str),
}

/// How a Console OAuth provider refreshes: whether the refresh re-requests the
/// granted scopes, and how the client authenticates. `providers.json` lists
/// every provider in Console's `Oauth::Providers`, which a Console test checks.
#[derive(Clone, Copy, Deserialize)]
struct Provider {
    refresh_scopes: bool,
    token_endpoint_auth_method: ClientAuthentication,
}

#[derive(Clone, Copy, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
enum ClientAuthentication {
    ClientSecretPost,
    ClientSecretBasic,
}

static PROVIDERS: LazyLock<HashMap<String, Provider>> = LazyLock::new(|| {
    serde_json::from_str(include_str!("../providers.json")).expect("valid providers.json")
});

/// Standalone credentials re-request their scopes and post their client.
const STANDALONE: Provider = Provider {
    refresh_scopes: true,
    token_endpoint_auth_method: ClientAuthentication::ClientSecretPost,
};

fn provider(key: Option<&str>) -> Result<Provider, RefreshError> {
    match key {
        None => Ok(STANDALONE),
        Some(key) => PROVIDERS
            .get(key)
            .copied()
            .ok_or_else(|| RefreshError::config("unknown_provider")),
    }
}

pub(crate) async fn refresh(
    credential: &Credential,
    client: &TokenClient,
) -> Result<Outcome, RefreshError> {
    match credential.grant.as_str() {
        "client_credentials" => {
            required(&credential.client_secret, "client_secret")?;
            let form = oauth_form(credential, "client_credentials", vec![])?;
            refreshed(post(credential, client, &credential.token_endpoint, form, false).await?)
        }
        "password" => {
            refresh_with_fallback(
                credential,
                client,
                || refresh_token_request(credential).map(|(url, form)| (url.to_owned(), form)),
                || {
                    let (Some(username), Some(password)) =
                        (present(&credential.username), present(&credential.password))
                    else {
                        return Ok(None);
                    };
                    let form = oauth_form(
                        credential,
                        "password",
                        vec![
                            ("username", username.to_owned()),
                            ("password", password.to_owned()),
                        ],
                    )?;
                    Ok(Some((credential.token_endpoint.as_str(), form)))
                },
                "password_grant_missing_initial_values",
                false,
            )
            .await
        }
        "preqin" => {
            refresh_with_fallback(
                credential,
                client,
                || {
                    let refresh_token = required(&credential.refresh_token, "refresh_token")?;
                    // Console pins Preqin's token endpoint; its refresh
                    // endpoint is the sibling `refresh_token`.
                    let url = Url::parse(&credential.token_endpoint)
                        .and_then(|url| url.join("refresh_token"))
                        .map_err(|_| RefreshError::config("invalid_token_endpoint"))?;
                    Ok((url.into(), vec![("refresh_token", refresh_token)]))
                },
                || {
                    let (Some(username), Some(api_key)) =
                        (present(&credential.username), present(&credential.api_key))
                    else {
                        return Ok(None);
                    };
                    Ok(Some((
                        credential.token_endpoint.as_str(),
                        vec![
                            ("username", username.to_owned()),
                            ("apikey", api_key.to_owned()),
                        ],
                    )))
                },
                "preqin_missing_initial_values",
                true,
            )
            .await
        }
        _ => {
            if present(&credential.refresh_token).is_none() {
                return Ok(Outcome::Dead("missing_initial_refresh_token"));
            }
            let (url, form) = refresh_token_request(credential)?;
            refreshed(post(credential, client, url, form, false).await?)
        }
    }
}

type Form = Vec<(&'static str, String)>;

/// Uses the stored refresh token when there is one. When the endpoint rejects
/// it, falls back to the grant's initial values, which are also used when no
/// refresh token is stored. Preqin's endpoints use multipart forms and treat a
/// bare 4xx as a rejection.
async fn refresh_with_fallback<'a>(
    credential: &'a Credential,
    client: &TokenClient,
    refresh_request: impl FnOnce() -> Result<(String, Form), RefreshError>,
    initial_request: impl FnOnce() -> Result<Option<(&'a str, Form)>, RefreshError>,
    missing_initial_values: &'static str,
    preqin: bool,
) -> Result<Outcome, RefreshError> {
    let mut stale_refresh_token = false;
    if present(&credential.refresh_token).is_some() {
        let (url, form) = refresh_request()?;
        match post(credential, client, &url, form, preqin).await {
            Ok(token) => return refreshed(token),
            Err(error) if error.retryable => return Err(error),
            Err(error) => {
                warn!(
                    credential_id = %credential.oid,
                    grant = %credential.grant,
                    reason = error.reason(),
                    "stored refresh token was rejected; falling back to the initial grant"
                );
                stale_refresh_token = true;
            }
        }
    }
    let Some((url, form)) = initial_request()? else {
        return Ok(Outcome::Dead(missing_initial_values));
    };
    let token = post(credential, client, url, form, preqin).await?;
    Ok(Outcome::Refreshed {
        clear_refresh_token: stale_refresh_token && token.refresh_token.is_none(),
        token,
    })
}

fn refreshed(token: TokenResponse) -> Result<Outcome, RefreshError> {
    Ok(Outcome::Refreshed {
        token,
        clear_refresh_token: false,
    })
}

fn refresh_token_request(credential: &Credential) -> Result<(&str, Form), RefreshError> {
    let refresh_token = required(&credential.refresh_token, "refresh_token")?;
    let form = oauth_form(
        credential,
        "refresh_token",
        vec![("refresh_token", refresh_token)],
    )?;
    Ok((credential.token_endpoint.as_str(), form))
}

/// An RFC 6749 token request form with the client credentials and scopes.
fn oauth_form(
    credential: &Credential,
    grant_type: &str,
    fields: Form,
) -> Result<Form, RefreshError> {
    let mut form = vec![("grant_type", grant_type.to_owned())];
    form.extend(fields);
    form.push(("client_id", required(&credential.client_id, "client_id")?));
    if let Some(secret) = present(&credential.client_secret) {
        form.push(("client_secret", secret.to_owned()));
    }
    if provider(credential.provider.as_deref())?.refresh_scopes && !credential.scopes.is_empty() {
        form.push(("scope", credential.scopes.join(" ")));
    }
    Ok(form)
}

async fn post(
    credential: &Credential,
    client: &TokenClient,
    url: &str,
    mut form: Form,
    preqin: bool,
) -> Result<TokenResponse, RefreshError> {
    let mut headers = HeaderMap::new();
    for (name, value) in &credential.token_endpoint_headers {
        headers.insert(
            HeaderName::try_from(name.as_str())
                .map_err(|_| RefreshError::config("invalid_token_endpoint_header"))?,
            HeaderValue::try_from(value.as_str())
                .map_err(|_| RefreshError::config("invalid_token_endpoint_header"))?,
        );
    }
    if provider(credential.provider.as_deref())?.token_endpoint_auth_method
        == ClientAuthentication::ClientSecretBasic
    {
        let client_id = required(&credential.client_id, "client_id")?;
        let client_secret = required(&credential.client_secret, "client_secret")?;
        let encode = |value: &str| byte_serialize(value.as_bytes()).collect::<String>();
        let basic = STANDARD.encode(format!("{}:{}", encode(&client_id), encode(&client_secret)));
        headers.insert(
            AUTHORIZATION,
            HeaderValue::try_from(format!("Basic {basic}"))
                .map_err(|_| RefreshError::config("invalid_client_credentials"))?,
        );
        form.retain(|(name, _)| !matches!(*name, "client_id" | "client_secret"));
    }
    client
        .post(TokenRequest {
            provider: &credential.provider_label(),
            url,
            form,
            multipart: preqin,
            headers,
            strict_4xx: preqin,
            timeout: credential.timeout,
        })
        .await
}

fn present(value: &Option<String>) -> Option<&str> {
    value.as_deref().filter(|value| !value.trim().is_empty())
}

fn required(value: &Option<String>, name: &'static str) -> Result<String, RefreshError> {
    present(value)
        .map(str::to_owned)
        .ok_or_else(|| RefreshError::config(&format!("missing_{name}")))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{Credential, oauth_form, provider_label};

    fn credential(provider: Option<&str>) -> Credential {
        Credential {
            oid: "bcr_test".into(),
            grant: "refresh_token".into(),
            token_endpoint: "https://idp.example/token".into(),
            provider: provider.map(str::to_owned),
            client_id: Some("cid".into()),
            client_secret: None,
            refresh_token: Some("RT".into()),
            username: None,
            password: None,
            api_key: None,
            token_endpoint_headers: vec![],
            scopes: vec!["a".into(), "b".into()],
            timeout: Duration::from_secs(30),
        }
    }

    #[test]
    fn credentials_without_an_oauth_app_are_labeled_by_token_endpoint_host() {
        assert_eq!(
            provider_label(Some("google"), "https://oauth2.googleapis.com/token"),
            "google"
        );
        assert_eq!(
            provider_label(None, "https://api.preqin.com/connect/token"),
            "api.preqin.com"
        );
        assert_eq!(provider_label(None, "not a url"), "unknown");
    }

    #[test]
    fn providers_decide_whether_refreshes_re_request_scopes() {
        for (provider, scope) in [
            (None, Some("a b")),
            (Some("google"), Some("a b")),
            (Some("microsoft"), Some("a b")),
            (Some("slack"), None),
            (Some("github"), None),
        ] {
            let form = oauth_form(&credential(provider), "refresh_token", vec![]).unwrap();
            let sent = form
                .iter()
                .find(|(name, _)| *name == "scope")
                .map(|(_, value)| value.as_str());
            assert_eq!(sent, scope, "{provider:?}");
        }
        assert_eq!(
            oauth_form(&credential(Some("unlisted")), "refresh_token", vec![])
                .unwrap_err()
                .reason(),
            "unknown_provider"
        );
    }
}
