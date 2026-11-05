//! API clients: machine callers of the session API, such as eval runners.
//!
//! Console stores each client, its hashed secret, scopes, and the principals it
//! may run sessions as. api-rs exchanges the client id and secret for a
//! short-lived access token (OAuth 2.0 `client_credentials`), re-reads the
//! client on every request so disabling it takes effect immediately, and
//! confines it to the `api:<client id>:` thread-key namespace.

use axum::{
    Json,
    extract::{State, rejection::FormRejection},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use base64::{Engine as _, engine::general_purpose};
use centaur_console_db::{ApiClient, ApiClientPrincipal, decode_oid, oid};
use centaur_session_core::ThreadKey;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use subtle::ConstantTimeEq;

use crate::{
    ApiError,
    auth::{API_CLIENT_TOKEN_TTL_SECONDS, AuthenticatedCaller, Capability},
    routes::AppState,
};

const API_CLIENT_OID_PREFIX: &str = "acl";
const PRINCIPAL_OID_PREFIX: &str = "prn";

/// The client a request authenticated as, after its live record was checked.
#[derive(Clone, Debug)]
pub(crate) struct ApiClientContext {
    id: i64,
    client_id: String,
}

impl ApiClientContext {
    /// Every thread key this client may touch starts with this prefix.
    pub(crate) fn thread_key_prefix(&self) -> String {
        thread_key_prefix(&self.client_id)
    }

    /// Whether `thread_key` is in this client's namespace.
    pub(crate) fn owns_thread_key(&self, thread_key: &ThreadKey) -> bool {
        thread_key
            .as_str()
            .strip_prefix(self.thread_key_prefix().as_str())
            .is_some_and(|rest| !rest.is_empty())
    }

    pub(crate) fn client_id(&self) -> &str {
        &self.client_id
    }

    pub(crate) async fn principals(
        &self,
        pool: &PgPool,
    ) -> Result<Vec<ApiClientPrincipal>, ApiError> {
        centaur_console_db::api_client_principals(pool, self.id)
            .await
            .map_err(|error| ApiError::Internal(format!("failed to read API client: {error}")))
    }
}

fn thread_key_prefix(client_id: &str) -> String {
    format!("api:{client_id}:")
}

/// Re-reads the client behind an API client access token. A deleted or
/// disabled client, or a token issued for a secret since rotated, is
/// rejected, and the caller keeps only the capabilities the client still has.
pub(crate) async fn refresh_api_client(
    pool: &PgPool,
    caller: &mut AuthenticatedCaller,
) -> Result<ApiClientContext, ApiError> {
    let client = find_client(pool, caller.identity())
        .await?
        .filter(|client| !client.disabled)
        .ok_or_else(|| ApiError::Unauthorized("API client is disabled or deleted".to_owned()))?;
    let current = secret_fingerprint(&client.secret_digest);
    let presented = caller.api_client_secret_fingerprint().unwrap_or_default();
    if !bool::from(current.as_bytes().ct_eq(presented.as_bytes())) {
        return Err(ApiError::Unauthorized(
            "API client secret was rotated; request a new token".to_owned(),
        ));
    }
    let live: Vec<Capability> = client
        .scopes
        .iter()
        .filter_map(|scope| Capability::from_api_client_scope(scope))
        .collect();
    caller.retain_capabilities(|capability| live.contains(&capability));
    Ok(ApiClientContext {
        id: client.id,
        client_id: caller.identity().to_owned(),
    })
}

async fn find_client(pool: &PgPool, client_id: &str) -> Result<Option<ApiClient>, ApiError> {
    let Some(id) = decode_oid(API_CLIENT_OID_PREFIX, client_id) else {
        return Ok(None);
    };
    centaur_console_db::find_api_client(pool, id)
        .await
        .map_err(|error| ApiError::Internal(format!("failed to read API client: {error}")))
}

/// The principal OID a session should run as.
///
/// `requested` (an OID or foreign ID) must be attached to the client. Without
/// one, an existing session keeps its `bound` principal, which must still be
/// attached; a new session uses the client's default principal, or its only one.
pub(crate) fn select_principal(
    attached: &[ApiClientPrincipal],
    requested: Option<&str>,
    bound: Option<&str>,
) -> Result<String, ApiError> {
    let matching = |ident: &str| {
        attached.iter().find(|principal| {
            principal_oid(principal) == ident || principal.foreign_id.as_deref() == Some(ident)
        })
    };
    let selected = match (requested.map(str::trim), bound) {
        (Some(requested), _) => matching(requested).ok_or_else(|| {
            ApiError::Forbidden("principal is not attached to this API client".to_owned())
        })?,
        (None, Some(bound)) => matching(bound).ok_or_else(|| {
            ApiError::Forbidden(
                "the session's principal is no longer attached to this API client".to_owned(),
            )
        })?,
        (None, None) => match attached.iter().find(|principal| principal.default) {
            Some(principal) => principal,
            None if attached.len() == 1 => &attached[0],
            None => {
                return Err(ApiError::BadRequest(
                    "principal is required: this API client has no default principal".to_owned(),
                ));
            }
        },
    };
    Ok(principal_oid(selected))
}

fn principal_oid(principal: &ApiClientPrincipal) -> String {
    oid(PRINCIPAL_OID_PREFIX, principal.principal_id)
}

/// Caller-supplied metadata from an API client, nested so none of its keys
/// can pose as an identity assertion (Slack user, requester principal, ...)
/// that the runtime trusts from chat ingresses.
pub(crate) fn api_client_metadata(client_id: &str, metadata: Option<Value>) -> Value {
    let mut wrapped = json!({ "api_client_id": client_id });
    if let Some(metadata) = metadata.filter(|metadata| !metadata.is_null()) {
        wrapped["client_metadata"] = metadata;
    }
    wrapped
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct TokenRequest {
    grant_type: Option<String>,
    scope: Option<String>,
    client_id: Option<String>,
    client_secret: Option<String>,
}

/// `POST /oauth/token`: the OAuth 2.0 client credentials grant (RFC 6749
/// section 4.4). The client authenticates with HTTP Basic or form fields.
pub(crate) async fn issue_api_client_token(
    State(state): State<AppState>,
    headers: HeaderMap,
    form: Result<axum::Form<TokenRequest>, FormRejection>,
) -> Response {
    match issue_token(&state, &headers, form).await {
        Ok(response) => response,
        Err(error) => error.into_response(),
    }
}

async fn issue_token(
    state: &AppState,
    headers: &HeaderMap,
    form: Result<axum::Form<TokenRequest>, FormRejection>,
) -> Result<Response, OAuthError> {
    let axum::Form(request) = form.map_err(|_| {
        OAuthError::invalid_request("expected an application/x-www-form-urlencoded body")
    })?;
    if request.grant_type.as_deref() != Some("client_credentials") {
        return Err(OAuthError {
            status: StatusCode::BAD_REQUEST,
            error: "unsupported_grant_type",
            description: "only the client_credentials grant is supported",
        });
    }
    let (client_id, secret) = client_credentials(headers, &request)?;
    let pool = state.console_db().map_err(|_| OAuthError::unavailable())?;
    let client = find_client(&pool, &client_id).await.map_err(|error| {
        tracing::error!(error = %error, "API client lookup failed");
        OAuthError::unavailable()
    })?;
    let Some(client) = client.filter(|client| secret_matches(&client.secret_digest, &secret))
    else {
        tracing::info!(client_id, "API client authentication failed");
        return Err(OAuthError::invalid_client());
    };
    if client.disabled {
        tracing::info!(client_id, "disabled API client requested a token");
        return Err(OAuthError::invalid_client());
    }
    let scopes = granted_scopes(&client.scopes, request.scope.as_deref())?;
    let now = time::OffsetDateTime::now_utc().unix_timestamp();
    let access_token = state
        .auth()
        .mint_api_client_token(
            &client_id,
            &secret_fingerprint(&client.secret_digest),
            &scopes,
            now,
        )
        .map_err(|error| {
            tracing::error!(error = %error, "failed to mint API client token");
            OAuthError::unavailable()
        })?;
    tracing::info!(client_id, scope = %scopes.join(" "), "issued API client token");
    Ok((
        [
            (header::CACHE_CONTROL, "no-store"),
            (header::PRAGMA, "no-cache"),
        ],
        Json(json!({
            "access_token": access_token,
            "token_type": "Bearer",
            "expires_in": API_CLIENT_TOKEN_TTL_SECONDS,
            "scope": scopes.join(" "),
        })),
    )
        .into_response())
}

/// The client id and secret, from HTTP Basic or the form (not both).
fn client_credentials(
    headers: &HeaderMap,
    request: &TokenRequest,
) -> Result<(String, String), OAuthError> {
    let basic = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split_once(' '))
        .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("Basic"))
        .map(|(_, encoded)| encoded.trim());
    let form = request.client_id.is_some() || request.client_secret.is_some();
    let (client_id, secret) = match (basic, form) {
        (Some(_), true) => {
            return Err(OAuthError::invalid_request(
                "use one client authentication method",
            ));
        }
        (Some(encoded), false) => {
            let decoded = general_purpose::STANDARD
                .decode(encoded)
                .ok()
                .and_then(|decoded| String::from_utf8(decoded).ok())
                .ok_or_else(OAuthError::invalid_client)?;
            let (id, secret) = decoded
                .split_once(':')
                .ok_or_else(OAuthError::invalid_client)?;
            // RFC 6749 section 2.3.1 form-encodes both before Basic encoding.
            let decode = |value: &str| {
                urlencoding::decode(&value.replace('+', " "))
                    .map(|value| value.into_owned())
                    .map_err(|_| OAuthError::invalid_client())
            };
            (decode(id)?, decode(secret)?)
        }
        (None, _) => (
            request.client_id.clone().unwrap_or_default(),
            request.client_secret.clone().unwrap_or_default(),
        ),
    };
    if client_id.is_empty() || secret.is_empty() {
        return Err(OAuthError::invalid_client());
    }
    Ok((client_id, secret))
}

/// Identifies a client secret in its access tokens without revealing its
/// digest.
fn secret_fingerprint(secret_digest: &str) -> String {
    let digest = Sha256::digest(format!("centaur-api-client-token:{secret_digest}").as_bytes());
    hex::encode(&digest[..16])
}

fn secret_matches(stored_digest: &str, secret: &str) -> bool {
    let digest = hex::encode(Sha256::digest(secret.as_bytes()));
    bool::from(digest.as_bytes().ct_eq(stored_digest.as_bytes()))
}

/// The requested scopes, which must all belong to the client; all of the
/// client's scopes when none are requested.
fn granted_scopes(
    client_scopes: &[String],
    requested: Option<&str>,
) -> Result<Vec<String>, OAuthError> {
    let requested: Vec<&str> = requested.unwrap_or_default().split_whitespace().collect();
    if requested.is_empty() {
        return Ok(client_scopes.to_vec());
    }
    if requested
        .iter()
        .any(|scope| !client_scopes.iter().any(|allowed| allowed == scope))
    {
        return Err(OAuthError {
            status: StatusCode::BAD_REQUEST,
            error: "invalid_scope",
            description: "a requested scope is not granted to this client",
        });
    }
    let mut scopes: Vec<String> = requested.into_iter().map(str::to_owned).collect();
    scopes.sort();
    scopes.dedup();
    Ok(scopes)
}

/// An RFC 6749 section 5.2 error response.
#[derive(Debug)]
struct OAuthError {
    status: StatusCode,
    error: &'static str,
    description: &'static str,
}

impl OAuthError {
    fn invalid_request(description: &'static str) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            error: "invalid_request",
            description,
        }
    }

    fn invalid_client() -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            error: "invalid_client",
            description: "client authentication failed",
        }
    }

    fn unavailable() -> Self {
        Self {
            status: StatusCode::SERVICE_UNAVAILABLE,
            error: "temporarily_unavailable",
            description: "the token endpoint is unavailable",
        }
    }
}

impl IntoResponse for OAuthError {
    fn into_response(self) -> Response {
        let mut response = (
            self.status,
            [(header::CACHE_CONTROL, "no-store")],
            Json(json!({ "error": self.error, "error_description": self.description })),
        )
            .into_response();
        if self.status == StatusCode::UNAUTHORIZED {
            response.headers_mut().insert(
                header::WWW_AUTHENTICATE,
                header::HeaderValue::from_static("Basic realm=\"centaur-api\""),
            );
        }
        response
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn principal(id: i64, foreign_id: Option<&str>, default: bool) -> ApiClientPrincipal {
        ApiClientPrincipal {
            principal_id: id,
            foreign_id: foreign_id.map(str::to_owned),
            default,
        }
    }

    #[test]
    fn select_principal_honors_request_binding_and_default() {
        let attached = [
            principal(1, Some("eval-default"), true),
            principal(2, Some("eval-other"), false),
        ];
        let default = oid(PRINCIPAL_OID_PREFIX, 1);
        let other = oid(PRINCIPAL_OID_PREFIX, 2);

        assert_eq!(select_principal(&attached, None, None).unwrap(), default);
        assert_eq!(
            select_principal(&attached, Some("eval-other"), None).unwrap(),
            other
        );
        assert_eq!(
            select_principal(&attached, Some(&other), Some(&default)).unwrap(),
            other,
            "an explicit request wins so the runtime can report the principal conflict"
        );
        assert_eq!(
            select_principal(&attached, None, Some(&other)).unwrap(),
            other
        );
    }

    #[test]
    fn select_principal_rejects_unattached_principals() {
        let attached = [principal(1, Some("eval-default"), true)];
        let unattached = oid(PRINCIPAL_OID_PREFIX, 9);

        for (requested, bound) in [
            (Some("slack-user-t1-u1"), None),
            (Some(unattached.as_str()), None),
            (None, Some(unattached.as_str())),
        ] {
            assert!(matches!(
                select_principal(&attached, requested, bound),
                Err(ApiError::Forbidden(_))
            ));
        }
    }

    #[test]
    fn select_principal_requires_a_choice_without_a_default() {
        let single = [principal(1, None, false)];
        assert_eq!(
            select_principal(&single, None, None).unwrap(),
            oid(PRINCIPAL_OID_PREFIX, 1)
        );

        let several = [principal(1, None, false), principal(2, None, false)];
        assert!(matches!(
            select_principal(&several, None, None),
            Err(ApiError::BadRequest(_))
        ));
        assert!(matches!(
            select_principal(&[], None, None),
            Err(ApiError::BadRequest(_))
        ));
    }

    #[test]
    fn clients_own_only_their_thread_key_namespace() {
        let client = ApiClientContext {
            id: 1,
            client_id: "acl_abc".to_owned(),
        };
        let owns = |key: &str| client.owns_thread_key(&ThreadKey::parse(key).unwrap());

        assert!(owns("api:acl_abc:eval-1"));
        for key in [
            "api:acl_abc:",
            "api:acl_abcd:eval-1",
            "api:acl_ab:c:eval-1",
            "api:eval-1",
            "slack:T1:C1:1.2",
        ] {
            assert!(!owns(key), "{key}");
        }
    }

    #[test]
    fn api_client_metadata_nests_caller_keys() {
        assert_eq!(
            api_client_metadata(
                "acl_x",
                Some(json!({ "slack_user_id": "U1", "requester_principal_foreign_id": "p" }))
            ),
            json!({
                "api_client_id": "acl_x",
                "client_metadata": { "slack_user_id": "U1", "requester_principal_foreign_id": "p" }
            })
        );
        assert_eq!(
            api_client_metadata("acl_x", None),
            json!({ "api_client_id": "acl_x" })
        );
    }

    #[test]
    fn client_credentials_accepts_basic_or_form_but_not_both() {
        let mut headers = HeaderMap::new();
        let encoded = general_purpose::STANDARD.encode("acl_x:acs_s%2Bcret");
        headers.insert(
            header::AUTHORIZATION,
            format!("Basic {encoded}").parse().unwrap(),
        );
        assert_eq!(
            client_credentials(&headers, &TokenRequest::default()).unwrap(),
            ("acl_x".to_owned(), "acs_s+cret".to_owned())
        );

        let form = TokenRequest {
            client_id: Some("acl_x".to_owned()),
            client_secret: Some("acs_secret".to_owned()),
            ..TokenRequest::default()
        };
        assert_eq!(
            client_credentials(&HeaderMap::new(), &form).unwrap(),
            ("acl_x".to_owned(), "acs_secret".to_owned())
        );
        assert_eq!(
            client_credentials(&headers, &form).unwrap_err().error,
            "invalid_request"
        );
        assert_eq!(
            client_credentials(&HeaderMap::new(), &TokenRequest::default())
                .unwrap_err()
                .error,
            "invalid_client"
        );
    }

    #[test]
    fn rotating_the_secret_changes_its_fingerprint() {
        let original = hex::encode(Sha256::digest(b"acs_original"));
        let rotated = hex::encode(Sha256::digest(b"acs_rotated"));
        assert_eq!(secret_fingerprint(&original), secret_fingerprint(&original));
        assert_ne!(secret_fingerprint(&original), secret_fingerprint(&rotated));
        assert!(!original.contains(&secret_fingerprint(&original)));
    }

    #[test]
    fn secret_matches_only_the_stored_digest() {
        let digest = hex::encode(Sha256::digest(b"acs_secret"));
        assert!(secret_matches(&digest, "acs_secret"));
        assert!(!secret_matches(&digest, "acs_other"));
        assert!(!secret_matches("", "acs_secret"));
    }

    #[test]
    fn granted_scopes_must_belong_to_the_client() {
        let client = vec!["sessions:read".to_owned(), "sessions:write".to_owned()];
        assert_eq!(granted_scopes(&client, None).unwrap(), client);
        assert_eq!(
            granted_scopes(&client, Some(" sessions:read sessions:read ")).unwrap(),
            vec!["sessions:read".to_owned()]
        );
        assert_eq!(
            granted_scopes(&client[..1], Some("sessions:write"))
                .unwrap_err()
                .error,
            "invalid_scope"
        );
    }
}
