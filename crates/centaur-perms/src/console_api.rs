//! The Console's bearer admin API (`/api/v1`), as `centaur-perms` uses it.
//!
//! api-rs writes the Console database in-process ([`centaur_iron_control::IronControlClient`]);
//! this CLI runs outside the cluster, so it goes through the API with an
//! operator key instead. Request bodies use the API's `{ "data": ... }`
//! envelope, and list endpoints are paged to exhaustion.

use centaur_iron_control::{
    AwsAuthSecretInput, BrokerCredentialInput, BrokerCredentialRecord, DataEnvelope,
    EffectiveConfig, GcpAuthSecretInput, GcpIdTokenSecretInput, Grant, GrantSecret, Grantee,
    HmacSecretInput, IdentityInput, OAuthTokenSecretInput, PgDsnSecretInput, Principal,
    PrincipalInput, Role, RoleSecretWriter, SecretRecord, StaticSecretInput,
};
use reqwest::{Client, Method, Response};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

const API_PREFIX: &str = "/api/v1";
const PAGE_LIMIT: usize = 100;

/// A failed Console API call.
#[derive(Debug, thiserror::Error)]
pub enum ConsoleApiError {
    #[error("Console request to {path} failed: {source}")]
    Transport {
        path: String,
        #[source]
        source: reqwest::Error,
    },
    /// A non-success status. `body` carries the Console's
    /// `{ "error": { "message", "details" } }` envelope.
    #[error("Console {method} {path} returned {status}: {body}")]
    Status {
        method: String,
        path: String,
        status: u16,
        body: String,
    },
    #[error("Console {path} response did not match the expected schema: {source}")]
    Decode {
        path: String,
        #[source]
        source: reqwest::Error,
    },
}

impl ConsoleApiError {
    /// Whether the Console answered with `code`.
    pub fn is_status(&self, code: u16) -> bool {
        matches!(self, Self::Status { status, .. } if *status == code)
    }
}

type Result<T> = std::result::Result<T, ConsoleApiError>;

pub struct ConsoleApi {
    http: Client,
    base_url: String,
    api_key: String,
}

impl ConsoleApi {
    pub fn new(base_url: &str, api_key: &str) -> Self {
        Self {
            http: Client::new(),
            base_url: base_url.trim_end_matches('/').to_owned(),
            api_key: api_key.to_owned(),
        }
    }

    // ----- principals & roles ---------------------------------------------

    pub async fn upsert_principal(&self, input: &PrincipalInput) -> Result<Principal> {
        self.write(
            Method::PUT,
            &upsert_path("principals", &input.foreign_id),
            input,
        )
        .await
    }

    pub async fn get_principal(&self, principal: &str) -> Result<Principal> {
        self.get(&resource_path("principals", "prn_", principal, ""))
            .await
    }

    pub async fn list_principals(&self, labels: &[(String, String)]) -> Result<Vec<Principal>> {
        self.paginate(&collection_path_with_labels("principals", labels))
            .await
    }

    pub async fn effective_config(&self, principal: &str) -> Result<EffectiveConfig> {
        self.get(&resource_path(
            "principals",
            "prn_",
            principal,
            "/effective_config",
        ))
        .await
    }

    pub async fn upsert_role(&self, input: &IdentityInput) -> Result<Role> {
        self.write(Method::PUT, &upsert_path("roles", &input.foreign_id), input)
            .await
    }

    pub async fn get_role(&self, role: &str) -> Result<Role> {
        self.get(&resource_path("roles", "role_", role, "")).await
    }

    pub async fn list_roles(&self, labels: &[(String, String)]) -> Result<Vec<Role>> {
        self.paginate(&collection_path_with_labels("roles", labels))
            .await
    }

    /// Assign a role (by OID) to a principal (by OID).
    pub async fn assign_role(&self, principal_id: &str, role_id: &str) -> Result<()> {
        let path = format!(
            "{API_PREFIX}/principals/{}/roles",
            urlencoding::encode(principal_id)
        );
        let body = DataEnvelope::new(json!({ "role_id": role_id }));
        let response = self.send(Method::POST, &path, Some(&body)).await?;
        ensure_success(response, Method::POST, &path)
            .await
            .map(drop)
    }

    /// The roles assigned to a principal (by OID).
    pub async fn list_principal_roles(&self, principal: &str) -> Result<Vec<Role>> {
        self.get(&format!(
            "{API_PREFIX}/principals/{}/roles",
            urlencoding::encode(principal)
        ))
        .await
    }

    /// Unassign a role (by OID) from a principal (by OID).
    pub async fn unassign_role(&self, principal_id: &str, role_id: &str) -> Result<()> {
        self.delete(&format!(
            "{API_PREFIX}/principals/{}/roles/{}",
            urlencoding::encode(principal_id),
            urlencoding::encode(role_id)
        ))
        .await
    }

    // ----- secrets and grants ---------------------------------------------

    /// A secret's identity by OID; `collection` is its `*_secrets` segment.
    pub async fn get_secret(&self, collection: &str, oid: &str) -> Result<SecretRecord> {
        self.get(&format!(
            "{API_PREFIX}/{collection}/{}",
            urlencoding::encode(oid)
        ))
        .await
    }

    pub async fn list_secrets(
        &self,
        collection: &str,
        labels: &[(String, String)],
    ) -> Result<Vec<SecretRecord>> {
        self.paginate(&collection_path_with_labels(collection, labels))
            .await
    }

    /// A secret's full resource (credential values are never echoed) by OID
    /// or `foreign_id`, as raw JSON for type-specific rendering.
    pub async fn get_secret_detail(
        &self,
        collection: &str,
        oid_prefix: &str,
        ident: &str,
    ) -> Result<Value> {
        self.get(&resource_path(collection, oid_prefix, ident, ""))
            .await
    }

    /// The grants made directly to a principal (by OID).
    pub async fn list_principal_grants(&self, principal: &str) -> Result<Vec<Grant>> {
        self.paginate(&format!(
            "{API_PREFIX}/principals/{}/grants",
            urlencoding::encode(principal)
        ))
        .await
    }

    pub async fn delete_grant(&self, id: &str) -> Result<()> {
        self.delete(&format!("{API_PREFIX}/grants/{}", urlencoding::encode(id)))
            .await
    }

    // ----- broker credentials ---------------------------------------------

    /// Upsert by `foreign_id`; re-supplying `refresh_token` re-bootstraps it.
    pub async fn upsert_broker_credential(
        &self,
        input: &BrokerCredentialInput,
    ) -> Result<BrokerCredentialRecord> {
        self.write(
            Method::PUT,
            &upsert_path("broker_credentials", &input.foreign_id),
            input,
        )
        .await
    }

    pub async fn list_broker_credentials(
        &self,
        labels: &[(String, String)],
    ) -> Result<Vec<BrokerCredentialRecord>> {
        self.paginate(&collection_path_with_labels("broker_credentials", labels))
            .await
    }

    /// A broker credential's full resource (secret material is never echoed).
    pub async fn get_broker_credential_detail(&self, ident: &str) -> Result<Value> {
        self.get(&resource_path("broker_credentials", "bcr_", ident, ""))
            .await
    }

    pub async fn delete_broker_credential(&self, ident: &str) -> Result<()> {
        self.delete(&resource_path("broker_credentials", "bcr_", ident, ""))
            .await
    }

    // ----- transport -------------------------------------------------------

    async fn get<R: DeserializeOwned>(&self, path: &str) -> Result<R> {
        let response = self.send(Method::GET, path, None::<&Value>).await?;
        decode_data(response, Method::GET, path).await
    }

    async fn delete(&self, path: &str) -> Result<()> {
        let response = self.send(Method::DELETE, path, None::<&Value>).await?;
        ensure_success(response, Method::DELETE, path)
            .await
            .map(drop)
    }

    /// Send `data` in the `{ "data": ... }` envelope and unwrap the response's.
    async fn write<B: Serialize, R: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        data: &B,
    ) -> Result<R> {
        let body = DataEnvelope::new(data);
        let response = self.send(method.clone(), path, Some(&body)).await?;
        decode_data(response, method, path).await
    }

    /// Every page of a list endpoint, until a short page.
    async fn paginate<R: DeserializeOwned>(&self, base_path: &str) -> Result<Vec<R>> {
        let separator = if base_path.contains('?') { '&' } else { '?' };
        let mut rows = Vec::new();
        for page in 1.. {
            let page: Vec<R> = self
                .get(&format!(
                    "{base_path}{separator}page={page}&limit={PAGE_LIMIT}"
                ))
                .await?;
            let fetched = page.len();
            rows.extend(page);
            if fetched < PAGE_LIMIT {
                break;
            }
        }
        Ok(rows)
    }

    async fn send<B: Serialize>(
        &self,
        method: Method,
        path: &str,
        body: Option<&B>,
    ) -> Result<Response> {
        let mut request = self
            .http
            .request(method, format!("{}{path}", self.base_url))
            .bearer_auth(&self.api_key);
        if let Some(body) = body {
            request = request.json(body);
        }
        request
            .send()
            .await
            .map_err(|source| ConsoleApiError::Transport {
                path: path.to_owned(),
                source,
            })
    }
}

impl RoleSecretWriter for ConsoleApi {
    type Error = ConsoleApiError;

    async fn list_role_grants(&self, role: &str) -> Result<Vec<Grant>> {
        self.paginate(&format!(
            "{API_PREFIX}/roles/{}/grants",
            urlencoding::encode(role)
        ))
        .await
    }
    async fn upsert_static_secret(&self, input: &StaticSecretInput) -> Result<SecretRecord> {
        self.write(
            Method::PUT,
            &upsert_path("static_secrets", &input.foreign_id),
            input,
        )
        .await
    }
    async fn upsert_oauth_token_secret(
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
    async fn upsert_gcp_auth_secret(&self, input: &GcpAuthSecretInput) -> Result<SecretRecord> {
        match &input.foreign_id {
            Some(foreign_id) => {
                self.write(
                    Method::PUT,
                    &upsert_path("gcp_auth_secrets", foreign_id),
                    input,
                )
                .await
            }
            None => {
                self.write(Method::POST, &collection_path("gcp_auth_secrets"), input)
                    .await
            }
        }
    }
    async fn upsert_gcp_id_token_secret(
        &self,
        input: &GcpIdTokenSecretInput,
    ) -> Result<SecretRecord> {
        self.write(
            Method::PUT,
            &upsert_path("gcp_id_token_secrets", &input.foreign_id),
            input,
        )
        .await
    }
    async fn upsert_pg_dsn_secret(&self, input: &PgDsnSecretInput) -> Result<SecretRecord> {
        self.write(
            Method::PUT,
            &upsert_path("pg_dsn_secrets", &input.foreign_id),
            input,
        )
        .await
    }
    async fn upsert_hmac_secret(&self, input: &HmacSecretInput) -> Result<SecretRecord> {
        self.write(
            Method::PUT,
            &upsert_path("hmac_secrets", &input.foreign_id),
            input,
        )
        .await
    }
    async fn upsert_aws_auth_secret(&self, input: &AwsAuthSecretInput) -> Result<SecretRecord> {
        self.write(
            Method::PUT,
            &upsert_path("aws_auth_secrets", &input.foreign_id),
            input,
        )
        .await
    }
    async fn create_grant(&self, grantee: &Grantee, secret: &GrantSecret) -> Result<Grant> {
        self.write(
            Method::POST,
            &collection_path("grants"),
            &grant_body(grantee, secret),
        )
        .await
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
        GrantSecret::GcpIdToken(id) => ("gcp_id_token_secret_id", id),
        GrantSecret::OAuthToken(id) => ("oauth_token_secret_id", id),
        GrantSecret::PgDsn(id) => ("pg_dsn_secret_id", id),
        GrantSecret::Hmac(id) => ("hmac_secret_id", id),
        GrantSecret::AwsAuth(id) => ("aws_auth_secret_id", id),
        GrantSecret::SshKey(id) => ("ssh_key_secret_id", id),
    };
    map.insert(key.to_owned(), json!(id));
    Value::Object(map)
}

fn collection_path(collection: &str) -> String {
    format!("{API_PREFIX}/{collection}")
}

fn collection_path_with_labels(collection: &str, labels: &[(String, String)]) -> String {
    let mut path = collection_path(collection);
    for (index, (key, value)) in labels.iter().enumerate() {
        path.push_str(if index == 0 { "?" } else { "&" });
        path.push_str(&format!(
            "labels[{}]={}",
            urlencoding::encode(key),
            urlencoding::encode(value)
        ));
    }
    path
}

fn upsert_path(collection: &str, foreign_id: &str) -> String {
    format!(
        "{API_PREFIX}/{collection}/{}",
        urlencoding::encode(foreign_id)
    )
}

/// `/:id` for an OID (it carries `oid_prefix`), else `/lookup/:foreign_id`;
/// `suffix` addresses a sub-resource.
fn resource_path(collection: &str, oid_prefix: &str, ident: &str, suffix: &str) -> String {
    let lookup = if ident.starts_with(oid_prefix) {
        ""
    } else {
        "lookup/"
    };
    format!(
        "{API_PREFIX}/{collection}/{lookup}{}{suffix}",
        urlencoding::encode(ident)
    )
}

async fn decode_data<R: DeserializeOwned>(
    response: Response,
    method: Method,
    path: &str,
) -> Result<R> {
    let response = ensure_success(response, method, path).await?;
    let envelope: DataEnvelope<R> =
        response
            .json()
            .await
            .map_err(|source| ConsoleApiError::Decode {
                path: path.to_owned(),
                source,
            })?;
    Ok(envelope.data)
}

async fn ensure_success(response: Response, method: Method, path: &str) -> Result<Response> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }
    Err(ConsoleApiError::Status {
        method: method.to_string(),
        path: path.to_owned(),
        status: status.as_u16(),
        body: response.text().await.unwrap_or_default(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grant_body_keys_grantee_and_secret_by_type() {
        assert_eq!(
            grant_body(
                &Grantee::Principal("prn_abc".to_owned()),
                &GrantSecret::Static("ssr_xyz".to_owned()),
            ),
            json!({ "principal_id": "prn_abc", "static_secret_id": "ssr_xyz" })
        );
        assert_eq!(
            grant_body(
                &Grantee::Role("role_dev".to_owned()),
                &GrantSecret::SshKey("sks_team".to_owned()),
            ),
            json!({ "role_id": "role_dev", "ssh_key_secret_id": "sks_team" })
        );
    }

    #[test]
    fn paths_encode_identifiers_and_route_oids() {
        assert_eq!(
            upsert_path("static_secrets", "github/token"),
            "/api/v1/static_secrets/github%2Ftoken"
        );
        assert_eq!(
            resource_path("principals", "prn_", "prn_abc", ""),
            "/api/v1/principals/prn_abc"
        );
        assert_eq!(
            resource_path(
                "principals",
                "prn_",
                "slack-channel-c9",
                "/effective_config"
            ),
            "/api/v1/principals/lookup/slack-channel-c9/effective_config"
        );
        assert_eq!(
            collection_path_with_labels(
                "roles",
                &[
                    ("team".to_owned(), "a b".to_owned()),
                    ("tier".to_owned(), "1".to_owned())
                ]
            ),
            "/api/v1/roles?labels[team]=a%20b&labels[tier]=1"
        );
    }
}
