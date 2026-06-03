//! HTTP client for the iron-control admin API.
//!
//! The wire-touching surface is intentionally thin: request bodies are built by
//! pure helpers ([`grant_body`], [`collection_path`], [`upsert_path`]) that are
//! unit-tested without a server, and [`IronControlClient`] just authenticates,
//! wraps the body in the ``{ "data": ... }`` envelope, sends, and unwraps the
//! response ``data`` field.

use reqwest::{Client as HttpClient, Method, Response};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::error::{IronControlError, Result};
use crate::models::{
    DataEnvelope, GcpAuthSecretInput, Grant, GrantSecret, Grantee, IdentityInput,
    EffectiveConfig, OAuthTokenSecretInput, PgDsnSecretInput, Principal, Proxy, ProxyInput, Role,
    SecretRecord, StaticSecretInput,
};

const API_PREFIX: &str = "/api/v1";

/// Admin client for iron-control, authenticated with an ``iak_`` API key.
#[derive(Clone, Debug)]
pub struct IronControlClient {
    http: HttpClient,
    base_url: String,
    api_key: String,
}

impl IronControlClient {
    /// Build a client with a fresh [`reqwest::Client`].
    pub fn new(base_url: impl Into<String>, api_key: impl Into<String>) -> Self {
        Self::with_client(HttpClient::new(), base_url, api_key)
    }

    /// Build a client reusing an existing [`reqwest::Client`] (connection pool,
    /// timeouts, proxy settings, …).
    pub fn with_client(
        http: HttpClient,
        base_url: impl Into<String>,
        api_key: impl Into<String>,
    ) -> Self {
        Self {
            http,
            base_url: base_url.into().trim_end_matches('/').to_owned(),
            api_key: api_key.into(),
        }
    }

    // ----- principals & roles ---------------------------------------------

    /// Upsert a principal by ``foreign_id`` (create if absent, update if not).
    pub async fn upsert_principal(&self, input: &IdentityInput) -> Result<Principal> {
        self.write(Method::PUT, &upsert_path("principals", &input.foreign_id), input)
            .await
    }

    /// Upsert a role by ``foreign_id``.
    pub async fn upsert_role(&self, input: &IdentityInput) -> Result<Role> {
        self.write(Method::PUT, &upsert_path("roles", &input.foreign_id), input)
            .await
    }

    /// Fetch a role by OID or ``foreign_id``. Read-only.
    pub async fn get_role(&self, role: &str) -> Result<Role> {
        let path = format!("{API_PREFIX}/roles/{}", urlencoding::encode(role));
        let resp = self.send(Method::GET, &path, None::<&Value>).await?;
        decode_data(resp, Method::GET, &path).await
    }

    /// List every principal in ``namespace``, optionally filtered to those
    /// carrying all of ``labels`` (JSONB containment). Pages are fetched
    /// transparently, so the full set is returned.
    pub async fn list_principals(
        &self,
        namespace: &str,
        labels: &[(String, String)],
    ) -> Result<Vec<Principal>> {
        self.list_collection("principals", namespace, labels).await
    }

    /// List every role in ``namespace``, optionally filtered by ``labels``.
    pub async fn list_roles(&self, namespace: &str, labels: &[(String, String)]) -> Result<Vec<Role>> {
        self.list_collection("roles", namespace, labels).await
    }

    /// Paginate a namespaced collection (``principals``/``roles``) to exhaustion.
    async fn list_collection<R: DeserializeOwned>(
        &self,
        collection: &str,
        namespace: &str,
        labels: &[(String, String)],
    ) -> Result<Vec<R>> {
        let mut base = format!("{API_PREFIX}/{collection}?namespace={}", urlencoding::encode(namespace));
        for (key, value) in labels {
            base.push_str(&format!(
                "&labels[{}]={}",
                urlencoding::encode(key),
                urlencoding::encode(value)
            ));
        }
        self.paginate(&base).await
    }

    /// Fetch every page of a list endpoint, appending ``page``/``limit`` (with
    /// the right separator depending on whether ``base_path`` already has a
    /// query string) and reading until a short page signals the end.
    async fn paginate<R: DeserializeOwned>(&self, base_path: &str) -> Result<Vec<R>> {
        const LIMIT: usize = 100;
        let sep = if base_path.contains('?') { '&' } else { '?' };
        let mut out = Vec::new();
        let mut page = 1usize;
        loop {
            let path = format!("{base_path}{sep}page={page}&limit={LIMIT}");
            let resp = self.send(Method::GET, &path, None::<&Value>).await?;
            let items: Vec<R> = decode_data(resp, Method::GET, &path).await?;
            let fetched = items.len();
            out.extend(items);
            if fetched < LIMIT {
                break;
            }
            page += 1;
        }
        Ok(out)
    }

    /// The principal's effective config — the secrets/postgres its proxy would
    /// sync. Accepts the principal OID or foreign_id. api-rs reads this to wire
    /// the sandbox's env for operator-managed secrets.
    pub async fn effective_config(&self, principal: &str) -> Result<EffectiveConfig> {
        let path = format!(
            "{API_PREFIX}/principals/{}/effective_config",
            urlencoding::encode(principal)
        );
        let resp = self.send(Method::GET, &path, None::<&Value>).await?;
        decode_data(resp, Method::GET, &path).await
    }

    /// Fetch a principal by OID or ``foreign_id``. Read-only: unlike
    /// [`Self::upsert_principal`] it never creates the principal.
    pub async fn get_principal(&self, principal: &str) -> Result<Principal> {
        let path = format!("{API_PREFIX}/principals/{}", urlencoding::encode(principal));
        let resp = self.send(Method::GET, &path, None::<&Value>).await?;
        decode_data(resp, Method::GET, &path).await
    }

    /// Assign a role (by OID) to a principal (by OID).
    pub async fn assign_role(&self, principal_id: &str, role_id: &str) -> Result<()> {
        let path = format!(
            "{API_PREFIX}/principals/{}/roles",
            urlencoding::encode(principal_id)
        );
        self.write_unit(Method::POST, &path, &json!({ "role_id": role_id }))
            .await
    }

    /// List the roles assigned to a principal (by OID or ``foreign_id``).
    pub async fn list_principal_roles(&self, principal: &str) -> Result<Vec<Role>> {
        let path = format!(
            "{API_PREFIX}/principals/{}/roles",
            urlencoding::encode(principal)
        );
        let resp = self.send(Method::GET, &path, None::<&Value>).await?;
        decode_data(resp, Method::GET, &path).await
    }

    /// Unassign a role (by OID) from a principal (by OID).
    pub async fn unassign_role(&self, principal_id: &str, role_id: &str) -> Result<()> {
        let path = format!(
            "{API_PREFIX}/principals/{}/roles/{}",
            urlencoding::encode(principal_id),
            urlencoding::encode(role_id)
        );
        let resp = self.send(Method::DELETE, &path, None::<&Value>).await?;
        expect_success(resp, Method::DELETE, &path).await
    }

    // ----- secrets ---------------------------------------------------------

    /// Upsert a static secret by ``foreign_id``.
    pub async fn upsert_static_secret(&self, input: &StaticSecretInput) -> Result<SecretRecord> {
        self.write(
            Method::PUT,
            &upsert_path("static_secrets", &input.foreign_id),
            input,
        )
        .await
    }

    /// Upsert an OAuth token secret by ``foreign_id``.
    pub async fn upsert_oauth_token_secret(
        &self,
        input: &OAuthTokenSecretInput,
    ) -> Result<SecretRecord> {
        self.write(
            Method::PUT,
            &upsert_path("oauth_token_secrets", &input.foreign_id),
            input,
        )
        .await
    }

    /// Upsert a GCP auth secret. Upserts by ``foreign_id`` when one is set;
    /// otherwise creates a new secret (workload-identity secrets need no ref).
    pub async fn upsert_gcp_auth_secret(&self, input: &GcpAuthSecretInput) -> Result<SecretRecord> {
        match &input.foreign_id {
            Some(foreign_id) => {
                self.write(Method::PUT, &upsert_path("gcp_auth_secrets", foreign_id), input)
                    .await
            }
            None => {
                self.write(Method::POST, &collection_path("gcp_auth_secrets"), input)
                    .await
            }
        }
    }

    /// Upsert a Postgres DSN secret by ``foreign_id``.
    pub async fn upsert_pg_dsn_secret(&self, input: &PgDsnSecretInput) -> Result<SecretRecord> {
        self.write(
            Method::PUT,
            &upsert_path("pg_dsn_secrets", &input.foreign_id),
            input,
        )
        .await
    }

    // ----- grants ----------------------------------------------------------

    /// Attach a secret to a grantee (principal or role).
    pub async fn create_grant(&self, grantee: &Grantee, secret: &GrantSecret) -> Result<Grant> {
        self.write(Method::POST, &collection_path("grants"), &grant_body(grantee, secret))
            .await
    }

    /// List the grants made directly to a principal (by OID or ``foreign_id``).
    pub async fn list_principal_grants(&self, principal: &str) -> Result<Vec<Grant>> {
        let base = format!("{API_PREFIX}/principals/{}/grants", urlencoding::encode(principal));
        self.paginate(&base).await
    }

    /// List the grants attached to a role (by OID or ``foreign_id``).
    pub async fn list_role_grants(&self, role: &str) -> Result<Vec<Grant>> {
        let base = format!("{API_PREFIX}/roles/{}/grants", urlencoding::encode(role));
        self.paginate(&base).await
    }

    /// Revoke a grant by OID.
    pub async fn delete_grant(&self, id: &str) -> Result<()> {
        let path = format!("{API_PREFIX}/grants/{}", urlencoding::encode(id));
        let resp = self.send(Method::DELETE, &path, None::<&Value>).await?;
        expect_success(resp, Method::DELETE, &path).await
    }

    // ----- proxies ---------------------------------------------------------

    /// Register a proxy owned by ``principal_id``. The returned [`Proxy::token`]
    /// is the plaintext ``iprx_`` bearer and is only available here.
    pub async fn create_proxy(
        &self,
        name: impl Into<String>,
        principal_id: impl Into<String>,
    ) -> Result<Proxy> {
        let input = ProxyInput {
            name: name.into(),
            principal_id: principal_id.into(),
        };
        self.write(Method::POST, &collection_path("proxies"), &input)
            .await
    }

    /// Reassign a proxy to a different principal. The ``iprx_`` token is
    /// unchanged; the proxy picks up the new principal's grants on its next
    /// `/proxy/sync` (the config hash changes). This is how a warm-pool proxy,
    /// booted under a bootstrap principal, is bound to a session's principal at
    /// checkout without a restart or token swap.
    pub async fn assign_proxy_principal(&self, id: &str, principal_id: &str) -> Result<Proxy> {
        let path = format!("{API_PREFIX}/proxies/{}", urlencoding::encode(id));
        self.write(Method::PATCH, &path, &json!({ "principal_id": principal_id }))
            .await
    }

    /// Deregister a proxy by OID.
    pub async fn delete_proxy(&self, id: &str) -> Result<()> {
        let path = format!("{API_PREFIX}/proxies/{}", urlencoding::encode(id));
        let resp = self.send(Method::DELETE, &path, None::<&Value>).await?;
        expect_success(resp, Method::DELETE, &path).await
    }

    // ----- transport -------------------------------------------------------

    /// Wrap ``data`` in the ``{ "data": ... }`` envelope, send, and unwrap the
    /// response ``data`` field into ``R``.
    async fn write<B: Serialize, R: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        data: &B,
    ) -> Result<R> {
        let body = DataEnvelope::new(data);
        let resp = self.send(method.clone(), path, Some(&body)).await?;
        decode_data(resp, method, path).await
    }

    /// Like [`Self::write`] but discards the response body (assignment POSTs).
    async fn write_unit<B: Serialize>(&self, method: Method, path: &str, data: &B) -> Result<()> {
        let body = DataEnvelope::new(data);
        let resp = self.send(method.clone(), path, Some(&body)).await?;
        expect_success(resp, method, path).await
    }

    async fn send<B: Serialize>(
        &self,
        method: Method,
        path: &str,
        body: Option<&B>,
    ) -> Result<Response> {
        let url = format!("{}{path}", self.base_url);
        let mut request = self
            .http
            .request(method, url.as_str())
            .bearer_auth(&self.api_key);
        if let Some(body) = body {
            request = request.json(body);
        }
        request
            .send()
            .await
            .map_err(|source| IronControlError::Transport {
                path: path.to_owned(),
                source,
            })
    }
}

fn grant_body(grantee: &Grantee, secret: &GrantSecret) -> Value {
    let mut map = serde_json::Map::new();
    match grantee {
        Grantee::Principal(id) => map.insert("principal_id".to_owned(), json!(id)),
        Grantee::Role(id) => map.insert("role_id".to_owned(), json!(id)),
    };
    let (key, id) = match secret {
        GrantSecret::Static(id) => ("static_secret_id", id),
        GrantSecret::GcpAuth(id) => ("gcp_auth_secret_id", id),
        GrantSecret::OAuthToken(id) => ("oauth_token_secret_id", id),
        GrantSecret::PgDsn(id) => ("pg_dsn_secret_id", id),
    };
    map.insert(key.to_owned(), json!(id));
    Value::Object(map)
}

fn collection_path(collection: &str) -> String {
    format!("{API_PREFIX}/{collection}")
}

fn upsert_path(collection: &str, foreign_id: &str) -> String {
    format!("{API_PREFIX}/{collection}/{}", urlencoding::encode(foreign_id))
}

async fn decode_data<R: DeserializeOwned>(resp: Response, method: Method, path: &str) -> Result<R> {
    let resp = ensure_success(resp, method, path).await?;
    let envelope: DataEnvelope<R> =
        resp.json()
            .await
            .map_err(|source| IronControlError::Decode {
                path: path.to_owned(),
                source,
            })?;
    Ok(envelope.data)
}

async fn expect_success(resp: Response, method: Method, path: &str) -> Result<()> {
    ensure_success(resp, method, path).await.map(|_| ())
}

async fn ensure_success(resp: Response, method: Method, path: &str) -> Result<Response> {
    let status = resp.status();
    if status.is_success() {
        return Ok(resp);
    }
    let body = resp.text().await.unwrap_or_default();
    Err(IronControlError::Status {
        method: method.to_string(),
        path: path.to_owned(),
        status: status.as_u16(),
        body,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{InjectConfig, ReplaceConfig, RequestRule, SecretSource};

    #[test]
    fn grant_body_principal_static() {
        let body = grant_body(
            &Grantee::Principal("prn_abc".to_owned()),
            &GrantSecret::Static("ssr_xyz".to_owned()),
        );
        assert_eq!(
            body,
            json!({ "principal_id": "prn_abc", "static_secret_id": "ssr_xyz" })
        );
    }

    #[test]
    fn grant_body_role_oauth() {
        let body = grant_body(
            &Grantee::Role("role_infra".to_owned()),
            &GrantSecret::OAuthToken("ots_slack".to_owned()),
        );
        assert_eq!(
            body,
            json!({ "role_id": "role_infra", "oauth_token_secret_id": "ots_slack" })
        );
    }

    #[test]
    fn upsert_path_encodes_foreign_id() {
        assert_eq!(
            upsert_path("static_secrets", "github/token"),
            "/api/v1/static_secrets/github%2Ftoken"
        );
        assert_eq!(collection_path("grants"), "/api/v1/grants");
    }

    #[test]
    fn static_secret_serializes_with_envelope() {
        let input = StaticSecretInput {
            namespace: "default".to_owned(),
            foreign_id: "github-token".to_owned(),
            name: "GitHub Token".to_owned(),
            description: None,
            labels: Default::default(),
            inject_config: None,
            replace_config: Some(ReplaceConfig {
                proxy_value: "GITHUB_TOKEN".to_owned(),
                match_headers: vec!["Authorization".to_owned()],
                match_body: false,
                match_path: false,
                match_query: false,
                require: false,
            }),
            source: SecretSource::env("GITHUB_TOKEN"),
            rules: vec![RequestRule::host("api.github.com")],
        };
        let body = serde_json::to_value(DataEnvelope::new(&input)).unwrap();
        assert_eq!(
            body,
            json!({
                "data": {
                    "namespace": "default",
                    "foreign_id": "github-token",
                    "name": "GitHub Token",
                    "replace_config": {
                        "proxy_value": "GITHUB_TOKEN",
                        "match_headers": ["Authorization"]
                    },
                    "source": { "source_type": "env", "config": { "var": "GITHUB_TOKEN" } },
                    "rules": [{ "host": "api.github.com" }]
                }
            })
        );
    }

    #[test]
    fn inject_config_omits_unset_fields() {
        let inject = InjectConfig {
            header: Some("Authorization".to_owned()),
            query_param: None,
            formatter: Some("Bearer {{ .Value }}".to_owned()),
        };
        assert_eq!(
            serde_json::to_value(inject).unwrap(),
            json!({ "header": "Authorization", "formatter": "Bearer {{ .Value }}" })
        );
    }

    #[test]
    fn proxy_token_only_present_when_returned() {
        let created: Proxy = serde_json::from_value(json!({
            "id": "prx_1",
            "name": "edge",
            "principal_id": "prn_1",
            "token": "iprx_secret"
        }))
        .unwrap();
        assert_eq!(created.token.as_deref(), Some("iprx_secret"));

        let listed: Proxy = serde_json::from_value(json!({
            "id": "prx_1",
            "name": "edge",
            "principal_id": "prn_1"
        }))
        .unwrap();
        assert_eq!(listed.token, None);
    }
}
