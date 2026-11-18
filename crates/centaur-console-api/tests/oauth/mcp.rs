//! The MCP authorization server: metadata, registration, consent, and the
//! token endpoint.

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use centaur_console_api::testing::{
    TEST_JWT_SECRET, TEST_ORIGIN, TestApp, TestClient, TestResponse,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::support::*;

const VERIFIER: &str = "verifier-0123456789-0123456789-0123456789-0123456789";
const REDIRECT: &str = "https://client.example/callback";

fn challenge() -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(VERIFIER.as_bytes()))
}

async fn register(f: &Fixture, redirect_uris: &[&str]) -> String {
    let response = f
        .app
        .post("/mcp/oauth/register")
        .json(&json!({"redirect_uris": redirect_uris, "client_name": "Test client"}))
        .send()
        .await;
    assert_eq!(response.status(), 201, "{}", response.text());
    response.json()["client_id"].as_str().unwrap().to_owned()
}

fn params(client_id: &str, overrides: &[(&str, &str)]) -> Vec<(String, String)> {
    let mut params: Vec<(String, String)> = [
        ("response_type", "code"),
        ("client_id", client_id),
        ("redirect_uri", REDIRECT),
        ("scope", "mcp:tools"),
        ("state", "client-state"),
        ("resource", MCP_RESOURCE),
        ("code_challenge", &challenge()),
        ("code_challenge_method", "S256"),
    ]
    .iter()
    .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
    .collect();
    for (key, value) in overrides {
        params.retain(|(existing, _)| existing != key);
        params.push(((*key).to_owned(), (*value).to_owned()));
    }
    params
}

fn query(params: &[(String, String)]) -> String {
    url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(params)
        .finish()
}

async fn get_authorize(
    f: &Fixture,
    browser: &mut TestClient,
    params: &[(String, String)],
) -> TestResponse {
    browser
        .send(
            f.app
                .get(&format!("/mcp/oauth/authorize?{}", query(params)))
                .accept_html(),
        )
        .await
}

async fn post_authorize(
    f: &Fixture,
    browser: &mut TestClient,
    params: &[(String, String)],
    decision: &str,
) -> TestResponse {
    let token = browser.csrf_token().unwrap().to_owned();
    let mut form: Vec<(&str, &str)> = params
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    form.push(("decision", decision));
    form.push(("authenticity_token", &token));
    browser
        .send(
            f.app
                .post("/mcp/oauth/authorize")
                .form(&form)
                .header("origin", TEST_ORIGIN),
        )
        .await
}

fn redirect_params(response: &TestResponse) -> Vec<(String, String)> {
    let location = response.location().expect("a redirect");
    url::Url::parse(location)
        .unwrap()
        .query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect()
}

fn param(response: &TestResponse, key: &str) -> Option<String> {
    redirect_params(response)
        .into_iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v)
}

/// Approves a fresh authorization for `browser` and returns the code.
async fn approved_code(f: &Fixture, browser: &mut TestClient, client_id: &str) -> String {
    let response = post_authorize(f, browser, &params(client_id, &[]), "approve").await;
    assert_eq!(response.status(), 302, "{}", response.text());
    assert!(
        response.location().unwrap().starts_with(REDIRECT),
        "{:?}",
        response.location()
    );
    assert_eq!(param(&response, "state").as_deref(), Some("client-state"));
    param(&response, "code").expect("a code")
}

async fn token(f: &Fixture, pairs: &[(&str, &str)]) -> TestResponse {
    f.app.post("/mcp/oauth/token").form(pairs).send().await
}

async fn exchange(f: &Fixture, client_id: &str, code: &str) -> TestResponse {
    token(
        f,
        &[
            ("grant_type", "authorization_code"),
            ("client_id", client_id),
            ("code", code),
            ("redirect_uri", REDIRECT),
            ("code_verifier", VERIFIER),
        ],
    )
    .await
}

async fn refresh(f: &Fixture, client_id: &str, refresh_token: &str) -> TestResponse {
    token(
        f,
        &[
            ("grant_type", "refresh_token"),
            ("client_id", client_id),
            ("refresh_token", refresh_token),
        ],
    )
    .await
}

fn claims(access_token: &str) -> Value {
    let mut validation = jsonwebtoken::Validation::new(jsonwebtoken::Algorithm::HS256);
    validation.set_audience(&[MCP_RESOURCE]);
    validation.validate_exp = false;
    validation.validate_nbf = false;
    jsonwebtoken::decode::<Value>(
        access_token,
        &jsonwebtoken::DecodingKey::from_secret(TEST_JWT_SECRET.as_bytes()),
        &validation,
    )
    .expect("a valid access token")
    .claims
}

fn assert_oauth_error(response: &TestResponse, status: u16, error: &str) {
    assert_eq!(
        (response.status(), response.json()["error"].as_str()),
        (status, Some(error)),
        "{}",
        response.text()
    );
}

// --- metadata and registration ------------------------------------------------------------

#[tokio::test]
async fn metadata_advertises_the_endpoints() {
    let Some(f) = fixture().await else { return };
    for path in [
        "/.well-known/oauth-authorization-server",
        "/.well-known/openid-configuration",
    ] {
        let body = f.app.get(path).send().await.json();
        assert_eq!(body["issuer"], "http://www.example.com", "{path}");
        assert_eq!(
            body["authorization_endpoint"],
            "http://www.example.com/mcp/oauth/authorize"
        );
        assert_eq!(
            body["token_endpoint"],
            "http://www.example.com/mcp/oauth/token"
        );
        assert_eq!(
            body["registration_endpoint"],
            "http://www.example.com/mcp/oauth/register"
        );
        assert_eq!(body["code_challenge_methods_supported"], json!(["S256"]));
        assert_eq!(
            body["token_endpoint_auth_methods_supported"],
            json!(["none"])
        );
        assert_eq!(
            body["grant_types_supported"],
            json!(["authorization_code", "refresh_token"])
        );
    }
}

#[tokio::test]
async fn metadata_trusts_only_allowed_forwarded_hosts() {
    let env = [
        (
            "CENTAUR_CONSOLE_ALLOWED_HOSTS",
            "www.example.com,proxy.internal",
        ),
        ("CENTAUR_MCP_PUBLIC_URL", MCP_RESOURCE),
    ];
    let Some(app) = TestApp::build(centaur_console_api::oauth::routes(), &env, |_| {}).await else {
        return;
    };
    async fn issuer(app: &TestApp, forwarded: Option<&str>) -> Value {
        let mut request = app.get("/.well-known/oauth-authorization-server");
        if let Some(host) = forwarded {
            request = request.header("x-forwarded-host", host);
        }
        request.send().await.json()["issuer"].clone()
    }
    assert_eq!(issuer(&app, None).await, "http://www.example.com");
    // A proxy's host we answer to is believed as sent.
    assert_eq!(
        issuer(&app, Some("PROXY.internal:8443")).await,
        "http://PROXY.internal:8443"
    );
    // One we do not answer to is ignored in favor of `Host`.
    assert_eq!(
        issuer(&app, Some("evil.example")).await,
        "http://www.example.com"
    );
    assert_eq!(
        issuer(&app, Some("proxy.internal.evil.example")).await,
        "http://www.example.com"
    );
}

#[tokio::test]
async fn mcp_metadata_registration_and_consent_are_never_cached() {
    let Some(f) = fixture().await else { return };
    for path in [
        "/.well-known/oauth-authorization-server",
        "/.well-known/openid-configuration",
    ] {
        let response = f.app.get(path).send().await;
        assert_eq!(response.status(), 200, "{path}");
        assert_eq!(response.header("cache-control"), Some("no-store"), "{path}");
    }
    let registered = f
        .app
        .post("/mcp/oauth/register")
        .json(&json!({"redirect_uris": [REDIRECT]}))
        .send()
        .await;
    assert_eq!(registered.status(), 201);
    assert_eq!(registered.header("cache-control"), Some("no-store"));
    let refused = f
        .app
        .post("/mcp/oauth/register")
        .json(&json!({"redirect_uris": []}))
        .send()
        .await;
    assert_eq!(refused.status(), 400);
    assert_eq!(refused.header("cache-control"), Some("no-store"));

    // Both the consent page and its answers.
    let client = registered.json()["client_id"].as_str().unwrap().to_owned();
    let mut browser = f.browser(&f.admin);
    let page = get_authorize(&f, &mut browser, &params(&client, &[])).await;
    assert_eq!(page.status(), 200, "{}", page.text());
    assert_eq!(page.header("cache-control"), Some("no-store"));
}

#[tokio::test]
async fn registers_public_clients() {
    let Some(f) = fixture().await else { return };
    let response = f
        .app
        .post("/mcp/oauth/register")
        .json(&json!({
            "redirect_uris": [REDIRECT, "http://127.0.0.1/cb", "cursor://anysphere.cursor-mcp/oauth/callback"],
            "client_name": "Cursor", "scope": "mcp:tools", "software_id": "cursor", "logo_uri": null
        }))
        .send()
        .await;
    assert_eq!(response.status(), 201, "{}", response.text());
    let body = response.json();
    assert!(body["client_id"].as_str().unwrap().starts_with("moc_"));
    assert_eq!(body["client_name"], "Cursor");
    assert_eq!(
        body["grant_types"],
        json!(["authorization_code", "refresh_token"])
    );
    assert_eq!(body["response_types"], json!(["code"]));
    assert_eq!(body["scope"], "mcp:tools");
    assert_eq!(body["token_endpoint_auth_method"], "none");
    let metadata: Value = sqlx::query_scalar("SELECT metadata FROM mcp_oauth_clients")
        .fetch_one(&f.app.db.pool)
        .await
        .unwrap();
    assert_eq!(metadata, json!({"software_id": "cursor"}));

    // A client with no name is called "MCP client".
    let response = f
        .app
        .post("/mcp/oauth/register")
        .json(&json!({"redirect_uris": [REDIRECT]}))
        .send()
        .await;
    assert_eq!(response.json()["client_name"], "MCP client");
}

#[tokio::test]
async fn refuses_unsafe_or_unsupported_registrations() {
    let Some(f) = fixture().await else { return };
    for body in [
        json!({}),
        json!({"redirect_uris": []}),
        json!({"redirect_uris": ["http://example.com/cb"]}),
        json!({"redirect_uris": ["https://example.com/*"]}),
        json!({"redirect_uris": ["https://example.com/cb#frag"]}),
        json!({"redirect_uris": ["javascript:alert(1)"]}),
        json!({"redirect_uris": [REDIRECT], "grant_types": ["password"]}),
        json!({"redirect_uris": [REDIRECT], "response_types": ["token"]}),
        json!({"redirect_uris": [REDIRECT], "scope": "mcp:tools admin"}),
        json!({"redirect_uris": "not-a-list"}),
    ] {
        let response = f.app.post("/mcp/oauth/register").json(&body).send().await;
        assert_oauth_error(&response, 400, "invalid_client_metadata");
        assert!(response.json()["error_description"].is_string());
    }
    let response = f
        .app
        .post("/mcp/oauth/register")
        .raw_body("application/json", b"{not json")
        .send()
        .await;
    assert_oauth_error(&response, 400, "invalid_client_metadata");
    assert_eq!(f.count("mcp_oauth_clients").await, 0);
}

// --- consent -----------------------------------------------------------------------------

#[tokio::test]
async fn signed_out_pending_and_disabled_users_are_sent_elsewhere() {
    let Some(f) = fixture().await else { return };
    let client_id = register(&f, &[REDIRECT]).await;
    let mut anonymous = f.app.client();
    let response = get_authorize(&f, &mut anonymous, &params(&client_id, &[])).await;
    assert_eq!(
        (response.status(), response.location()),
        (302, Some("/login"))
    );

    let pending = f
        .app
        .create_user("pending@example.test", false, "pending")
        .await;
    let mut browser = f.browser(&pending);
    let response = get_authorize(&f, &mut browser, &params(&client_id, &[])).await;
    assert_eq!(
        (response.status(), response.location()),
        (302, Some("/pending"))
    );

    let disabled = f
        .app
        .create_user("disabled@example.test", false, "disabled")
        .await;
    let mut browser = f.browser(&disabled);
    let response = get_authorize(&f, &mut browser, &params(&client_id, &[])).await;
    assert_eq!(
        (response.status(), response.location()),
        (302, Some("/login"))
    );
    assert_eq!(f.count("mcp_oauth_authorization_codes").await, 0);
}

#[tokio::test]
async fn the_consent_page_shows_the_client_and_carries_the_request() {
    let Some(f) = fixture().await else { return };
    let client_id = register(&f, &[REDIRECT]).await;
    let mut browser = f.browser(&f.member);
    let response = get_authorize(&f, &mut browser, &params(&client_id, &[])).await;
    assert_eq!(response.status(), 200, "{}", response.text());
    let html = response.text();
    assert!(
        html.contains("Test client") && html.contains("client.example"),
        "{html}"
    );
    assert!(html.contains(MCP_RESOURCE) && html.contains("member@example.test"));
    for name in [
        "response_type",
        "client_id",
        "redirect_uri",
        "scope",
        "state",
        "resource",
        "code_challenge",
        "code_challenge_method",
        "authenticity_token",
    ] {
        assert!(html.contains(&format!("name=\"{name}\"")), "{name}");
    }
    assert!(html.contains(&format!("value=\"{}\"", browser.csrf_token().unwrap())));
    assert!(html.contains("value=\"approve\"") && html.contains("value=\"deny\""));
    assert!(
        response
            .header("content-security-policy")
            .unwrap()
            .contains("script-src 'none'")
    );
}

#[tokio::test]
async fn bad_requests_redirect_only_to_a_registered_uri() {
    let Some(f) = fixture().await else { return };
    let client_id = register(&f, &[REDIRECT]).await;
    let mut browser = f.browser(&f.member);

    // Unknown client, or an unregistered redirect: a plain 400, no redirect.
    for params in [
        params("moc_unknown", &[]),
        params(
            &client_id,
            &[("redirect_uri", "https://evil.example/callback")],
        ),
        params(
            &client_id,
            &[("redirect_uri", "https://client.example/callback/extra")],
        ),
    ] {
        let response = get_authorize(&f, &mut browser, &params).await;
        assert_eq!(response.status(), 400, "{}", response.text());
        assert_eq!(response.location(), None);
    }

    // Everything else goes back to the client with the error and its state.
    for (overrides, error) in [
        (
            vec![("response_type", "token")],
            "unsupported_response_type",
        ),
        (vec![("code_challenge_method", "plain")], "invalid_request"),
        (vec![("code_challenge", "")], "invalid_request"),
        (vec![("scope", "mcp:tools admin")], "invalid_scope"),
        (
            vec![("resource", "https://other.example/mcp")],
            "invalid_target",
        ),
    ] {
        let response = get_authorize(&f, &mut browser, &params(&client_id, &overrides)).await;
        assert_eq!(response.status(), 302, "{overrides:?}: {}", response.text());
        assert!(response.location().unwrap().starts_with(REDIRECT));
        assert_eq!(
            param(&response, "error").as_deref(),
            Some(error),
            "{overrides:?}"
        );
        assert_eq!(param(&response, "state").as_deref(), Some("client-state"));
        assert!(param(&response, "error_description").is_some());
    }
}

#[tokio::test]
async fn the_resource_must_be_the_configured_one_and_must_be_configured() {
    let Some(f) = fixture().await else { return };
    let client_id = register(&f, &[REDIRECT]).await;
    let mut browser = f.browser(&f.member);
    // The configured resource is used when none is requested; a differently
    // spelled equal URL is accepted.
    let response = get_authorize(
        &f,
        &mut browser,
        &params(
            &client_id,
            &[("resource", "https://console.example.test/mcp/")],
        ),
    )
    .await;
    assert_eq!(response.status(), 200);
    let response = get_authorize(&f, &mut browser, &params(&client_id, &[("resource", "")])).await;
    assert_eq!(response.status(), 200);

    let Some(unconfigured) =
        centaur_console_api::testing::TestApp::new(centaur_console_api::oauth::routes()).await
    else {
        return;
    };
    let response = unconfigured
        .post("/mcp/oauth/register")
        .json(&json!({"redirect_uris": [REDIRECT]}))
        .send()
        .await;
    let client = response.json()["client_id"].as_str().unwrap().to_owned();
    let user = unconfigured
        .create_user("u@example.test", false, "active")
        .await;
    let mut browser = unconfigured.client();
    browser.sign_in(user.id);
    let response = browser
        .send(unconfigured.get(&format!(
            "/mcp/oauth/authorize?{}",
            query(&params(&client, &[("resource", "")]))
        )))
        .await;
    assert_eq!(param(&response, "error").as_deref(), Some("invalid_target"));
}

#[tokio::test]
async fn approval_needs_the_forms_csrf_token_and_a_signed_in_active_user() {
    let Some(f) = fixture().await else { return };
    let client_id = register(&f, &[REDIRECT]).await;
    let mut browser = f.browser(&f.member);
    let form = params(&client_id, &[("decision", "approve")]);
    let pairs: Vec<(&str, &str)> = form.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let response = browser
        .send(f.app.post("/mcp/oauth/authorize").form(&pairs))
        .await;
    assert_eq!(
        (response.status(), response.error_reason().as_deref()),
        (403, Some("csrf"))
    );
    let mut wrong = pairs.clone();
    wrong.push(("authenticity_token", "wrong"));
    let response = browser
        .send(f.app.post("/mcp/oauth/authorize").form(&wrong))
        .await;
    assert_eq!(response.status(), 403);
    let token = browser.csrf_token().unwrap().to_owned();
    let mut foreign = pairs.clone();
    foreign.push(("authenticity_token", &token));
    let response = browser
        .send(
            f.app
                .post("/mcp/oauth/authorize")
                .header("origin", "https://evil.example")
                .form(&foreign),
        )
        .await;
    assert_eq!(response.status(), 403);

    // Signed out: to the login page. Pending: to the holding page.
    let mut anonymous = f.app.client();
    anonymous.start_anonymous_session();
    let response = post_authorize(&f, &mut anonymous, &params(&client_id, &[]), "approve").await;
    assert_eq!(
        (response.status(), response.location()),
        (302, Some("/login"))
    );
    let pending = f
        .app
        .create_user("pending@example.test", false, "pending")
        .await;
    let mut browser = f.browser(&pending);
    let response = post_authorize(&f, &mut browser, &params(&client_id, &[]), "approve").await;
    assert_eq!(
        (response.status(), response.location()),
        (302, Some("/pending"))
    );
    assert_eq!(f.count("mcp_oauth_authorization_codes").await, 0);
}

#[tokio::test]
async fn denying_redirects_with_access_denied_and_grants_nothing() {
    let Some(f) = fixture().await else { return };
    let client_id = register(&f, &[REDIRECT]).await;
    let mut browser = f.browser(&f.member);
    let response = post_authorize(&f, &mut browser, &params(&client_id, &[]), "deny").await;
    assert_eq!(response.status(), 302);
    assert_eq!(param(&response, "error").as_deref(), Some("access_denied"));
    assert_eq!(param(&response, "state").as_deref(), Some("client-state"));
    assert_eq!(f.count("mcp_oauth_authorization_codes").await, 0);
}

#[tokio::test]
async fn approving_stores_only_the_hash_of_a_single_use_code() {
    let Some(f) = fixture().await else { return };
    let client_id = register(&f, &[REDIRECT]).await;
    let mut browser = f.browser(&f.member);
    let code = approved_code(&f, &mut browser, &client_id).await;
    assert!(code.starts_with("mcpauth_"));
    let row: (String, String, String, String, i64) = sqlx::query_as(
        "SELECT code_hash, redirect_uri, resource, code_challenge, user_id FROM mcp_oauth_authorization_codes",
    )
    .fetch_one(&f.app.db.pool)
    .await
    .unwrap();
    assert_no(&format!("{row:?}"), &[&code]);
    assert_eq!(row.0, hex::encode(Sha256::digest(code.as_bytes())));
    assert_eq!(
        (row.1.as_str(), row.2.as_str(), row.3, row.4),
        (REDIRECT, MCP_RESOURCE, challenge(), f.member.id)
    );
    let used: Option<chrono::NaiveDateTime> =
        sqlx::query_scalar("SELECT last_used_at FROM mcp_oauth_clients")
            .fetch_one(&f.app.db.pool)
            .await
            .unwrap();
    assert!(used.is_some());
}

#[tokio::test]
async fn a_loopback_client_may_use_any_port() {
    let Some(f) = fixture().await else { return };
    let client_id = register(&f, &["http://127.0.0.1/callback"]).await;
    let mut browser = f.browser(&f.member);
    let overrides = [("redirect_uri", "http://127.0.0.1:53682/callback")];
    let response =
        post_authorize(&f, &mut browser, &params(&client_id, &overrides), "approve").await;
    assert_eq!(response.status(), 302, "{}", response.text());
    assert!(
        response
            .location()
            .unwrap()
            .starts_with("http://127.0.0.1:53682/callback?")
    );
    let response = post_authorize(
        &f,
        &mut browser,
        &params(
            &client_id,
            &[("redirect_uri", "http://127.0.0.1:53682/other")],
        ),
        "approve",
    )
    .await;
    assert_eq!(response.status(), 400);
}

// --- tokens ----------------------------------------------------------------------------

#[tokio::test]
async fn exchanges_a_code_for_a_signed_access_token_and_a_refresh_token() {
    let Some(f) = fixture().await else { return };
    let client_id = register(&f, &[REDIRECT]).await;
    let mut browser = f.browser(&f.member);
    let code = approved_code(&f, &mut browser, &client_id).await;
    let response = exchange(&f, &client_id, &code).await;
    assert_eq!(response.status(), 200, "{}", response.text());
    assert_eq!(response.header("cache-control"), Some("no-store"));
    assert_eq!(response.header("pragma"), Some("no-cache"));
    let body = response.json();
    assert_eq!(body["token_type"], "Bearer");
    assert_eq!(body["expires_in"], 3600);
    assert_eq!(body["scope"], "mcp:tools");
    let refresh_token = body["refresh_token"].as_str().unwrap();
    assert!(refresh_token.starts_with("mcprt_"));

    let claims = claims(body["access_token"].as_str().unwrap());
    let principal: (i64, String) =
        sqlx::query_as("SELECT id, foreign_id FROM principals WHERE console_user_id = $1")
            .bind(f.member.id)
            .fetch_one(&f.app.db.pool)
            .await
            .unwrap();
    assert_eq!(claims["iss"], "http://www.example.com");
    assert_eq!(claims["sub"], centaur_console_db::oid("usr", f.member.id));
    assert_eq!(claims["aud"], MCP_RESOURCE);
    assert_eq!(claims["scope"], "mcp:tools");
    assert_eq!(claims["client_id"], client_id);
    assert_eq!(
        claims["principal_id"],
        centaur_console_db::oid("prn", principal.0)
    );
    assert_eq!(claims["principal_foreign_id"], principal.1);
    assert_eq!(claims["email"], "member@example.test");
    assert_eq!(
        claims["name"], "member@example.test",
        "the email stands in for a missing name"
    );
    assert!(claims["jti"].as_str().unwrap().starts_with("mcpjwt_"));
    let (iat, exp, nbf) = (
        claims["iat"].as_i64().unwrap(),
        claims["exp"].as_i64().unwrap(),
        claims["nbf"].as_i64().unwrap(),
    );
    assert_eq!(
        (iat, exp, nbf),
        (f.app.now().timestamp(), iat + 3600, iat - 5)
    );

    // The refresh token is stored hashed.
    let hash: String = sqlx::query_scalar("SELECT token_hash FROM mcp_oauth_refresh_tokens")
        .fetch_one(&f.app.db.pool)
        .await
        .unwrap();
    assert_eq!(hash, hex::encode(Sha256::digest(refresh_token.as_bytes())));
}

#[tokio::test]
async fn a_code_works_once() {
    let Some(f) = fixture().await else { return };
    let client_id = register(&f, &[REDIRECT]).await;
    let mut browser = f.browser(&f.member);
    let code = approved_code(&f, &mut browser, &client_id).await;
    assert_eq!(exchange(&f, &client_id, &code).await.status(), 200);
    let replay = exchange(&f, &client_id, &code).await;
    assert_oauth_error(&replay, 400, "invalid_grant");
    assert_eq!(f.count("mcp_oauth_refresh_tokens").await, 1);
}

#[tokio::test]
async fn concurrent_exchanges_of_one_code_issue_one_token() {
    let Some(f) = fixture().await else { return };
    let client_id = register(&f, &[REDIRECT]).await;
    let mut browser = f.browser(&f.member);
    let code = approved_code(&f, &mut browser, &client_id).await;
    let (a, b) = tokio::join!(
        exchange(&f, &client_id, &code),
        exchange(&f, &client_id, &code)
    );
    let mut statuses = [a.status(), b.status()];
    statuses.sort_unstable();
    assert_eq!(statuses, [200, 400]);
    assert_eq!(f.count("mcp_oauth_refresh_tokens").await, 1);
}

#[tokio::test]
async fn the_code_exchange_checks_everything_the_code_was_bound_to() {
    let Some(f) = fixture().await else { return };
    let client_id = register(&f, &[REDIRECT]).await;
    let other = register(&f, &[REDIRECT]).await;
    let mut browser = f.browser(&f.member);
    let code = approved_code(&f, &mut browser, &client_id).await;
    let good = [
        ("grant_type", "authorization_code"),
        ("client_id", client_id.as_str()),
        ("code", code.as_str()),
        ("redirect_uri", REDIRECT),
        ("code_verifier", VERIFIER),
    ];
    let with = |key: &str, value: &'static str| -> Vec<(&str, &str)> {
        good.iter()
            .map(|&(k, v)| if k == key { (k, value) } else { (k, v) })
            .collect()
    };
    assert_oauth_error(
        &token(&f, &with("code_verifier", "wrong-verifier")).await,
        400,
        "invalid_grant",
    );
    assert_oauth_error(
        &token(&f, &with("code_verifier", "")).await,
        400,
        "invalid_grant",
    );
    assert_oauth_error(
        &token(&f, &with("redirect_uri", "https://client.example/other")).await,
        400,
        "invalid_grant",
    );
    assert_oauth_error(
        &token(&f, &with("code", "mcpauth_unknown")).await,
        400,
        "invalid_grant",
    );
    let mut as_other = good.to_vec();
    as_other[1] = ("client_id", other.as_str());
    assert_oauth_error(&token(&f, &as_other).await, 400, "invalid_grant");
    assert_oauth_error(
        &token(&f, &with("client_id", "moc_unknown")).await,
        401,
        "invalid_client",
    );
    assert_oauth_error(
        &token(&f, &with("grant_type", "password")).await,
        400,
        "unsupported_grant_type",
    );
    // None of the failures consumed the code.
    assert_eq!(token(&f, &good).await.status(), 200);
}

#[tokio::test]
async fn an_expired_code_is_refused() {
    let Some(f) = fixture().await else { return };
    let client_id = register(&f, &[REDIRECT]).await;
    let mut browser = f.browser(&f.member);
    let code = approved_code(&f, &mut browser, &client_id).await;
    f.app.clock.set(centaur_console_api::testing::at(
        centaur_console_api::testing::TEST_NOW + 601,
    ));
    assert_oauth_error(&exchange(&f, &client_id, &code).await, 400, "invalid_grant");
}

#[tokio::test]
async fn refresh_tokens_rotate_and_a_replay_is_refused() {
    let Some(f) = fixture().await else { return };
    let client_id = register(&f, &[REDIRECT]).await;
    let mut browser = f.browser(&f.member);
    let code = approved_code(&f, &mut browser, &client_id).await;
    let first = exchange(&f, &client_id, &code).await.json()["refresh_token"]
        .as_str()
        .unwrap()
        .to_owned();

    let response = refresh(&f, &client_id, &first).await;
    assert_eq!(response.status(), 200, "{}", response.text());
    assert_eq!(response.header("cache-control"), Some("no-store"));
    let body = response.json();
    let second = body["refresh_token"].as_str().unwrap().to_owned();
    assert_ne!(first, second);
    assert_eq!(
        claims(body["access_token"].as_str().unwrap())["sub"],
        centaur_console_db::oid("usr", f.member.id)
    );

    // The old token is revoked; the new one works.
    assert_oauth_error(&refresh(&f, &client_id, &first).await, 400, "invalid_grant");
    assert_eq!(refresh(&f, &client_id, &second).await.status(), 200);
    let revoked: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM mcp_oauth_refresh_tokens WHERE revoked_at IS NOT NULL",
    )
    .fetch_one(&f.app.db.pool)
    .await
    .unwrap();
    assert_eq!(revoked, 2);

    // Another client cannot use it; an unknown one is refused outright.
    let other = register(&f, &[REDIRECT]).await;
    assert_oauth_error(
        &refresh(&f, "moc_unknown", &second).await,
        401,
        "invalid_client",
    );
    assert_oauth_error(
        &refresh(&f, &other, "mcprt_unknown").await,
        400,
        "invalid_grant",
    );
}

#[tokio::test]
async fn concurrent_refreshes_of_one_token_rotate_it_once() {
    let Some(f) = fixture().await else { return };
    let client_id = register(&f, &[REDIRECT]).await;
    let mut browser = f.browser(&f.member);
    let code = approved_code(&f, &mut browser, &client_id).await;
    let token_value = exchange(&f, &client_id, &code).await.json()["refresh_token"]
        .as_str()
        .unwrap()
        .to_owned();
    let (a, b) = tokio::join!(
        refresh(&f, &client_id, &token_value),
        refresh(&f, &client_id, &token_value)
    );
    let mut statuses = [a.status(), b.status()];
    statuses.sort_unstable();
    assert_eq!(statuses, [200, 400]);
    let live: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM mcp_oauth_refresh_tokens WHERE revoked_at IS NULL",
    )
    .fetch_one(&f.app.db.pool)
    .await
    .unwrap();
    assert_eq!(live, 1);
}

#[tokio::test]
async fn an_expired_refresh_token_is_refused() {
    let Some(f) = fixture().await else { return };
    let client_id = register(&f, &[REDIRECT]).await;
    let mut browser = f.browser(&f.member);
    let code = approved_code(&f, &mut browser, &client_id).await;
    let refresh_token = exchange(&f, &client_id, &code).await.json()["refresh_token"]
        .as_str()
        .unwrap()
        .to_owned();
    f.app.clock.set(centaur_console_api::testing::at(
        centaur_console_api::testing::TEST_NOW + 91 * 86_400,
    ));
    assert_oauth_error(
        &refresh(&f, &client_id, &refresh_token).await,
        400,
        "invalid_grant",
    );
}

#[tokio::test]
async fn a_user_who_is_no_longer_active_loses_every_token() {
    let Some(f) = fixture().await else { return };
    let client_id = register(&f, &[REDIRECT]).await;
    let mut browser = f.browser(&f.member);
    let first_code = approved_code(&f, &mut browser, &client_id).await;
    let refresh_token = exchange(&f, &client_id, &first_code).await.json()["refresh_token"]
        .as_str()
        .unwrap()
        .to_owned();
    let second_code = approved_code(&f, &mut browser, &client_id).await;
    sqlx::query("UPDATE users SET status = 'disabled' WHERE id = $1")
        .bind(f.member.id)
        .execute(&f.app.db.pool)
        .await
        .unwrap();

    let response = refresh(&f, &client_id, &refresh_token).await;
    assert_oauth_error(&response, 400, "invalid_grant");
    assert_eq!(
        response.json()["error_description"],
        "User account is not active."
    );
    let response = exchange(&f, &client_id, &second_code).await;
    assert_eq!(
        response.json()["error_description"],
        "User account is not active."
    );
    let live: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM mcp_oauth_refresh_tokens WHERE revoked_at IS NULL",
    )
    .fetch_one(&f.app.db.pool)
    .await
    .unwrap();
    assert_eq!(live, 0);
}

#[tokio::test]
async fn token_endpoint_errors_are_never_cached_and_malformed_bodies_are_oauth_errors() {
    let Some(f) = fixture().await else { return };
    let response = token(&f, &[("grant_type", "nope")]).await;
    assert_eq!(response.header("cache-control"), Some("no-store"));
    assert_oauth_error(&response, 400, "unsupported_grant_type");
    let response = f
        .app
        .post("/mcp/oauth/token")
        .raw_body("application/x-www-form-urlencoded", b"%ff=%zz")
        .send()
        .await;
    assert_eq!(response.header("cache-control"), Some("no-store"));
    assert_eq!(response.status(), 400);
}

#[tokio::test]
async fn without_a_signing_secret_the_grant_is_not_consumed() {
    let Some(app) = centaur_console_api::testing::TestApp::build(
        centaur_console_api::oauth::routes(),
        &[("CENTAUR_MCP_PUBLIC_URL", MCP_RESOURCE)],
        |config| config.jwt_signing_secret.clear(),
    )
    .await
    else {
        return;
    };
    let client = app
        .post("/mcp/oauth/register")
        .json(&json!({"redirect_uris": [REDIRECT]}))
        .send()
        .await
        .json()["client_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let user = app.create_user("u@example.test", false, "active").await;
    // Provisioning is a hook of another area; seed the grant directly.
    let principal = app.create_principal("console_user", "u", user.id).await;
    let client_row = centaur_console_db::decode_oid("moc", &client).unwrap();
    sqlx::query(
        "INSERT INTO mcp_oauth_authorization_codes (code_hash, code_challenge, mcp_oauth_client_id, user_id, \
         principal_id, redirect_uri, resource, scopes, expires_at, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, '[\"mcp:tools\"]', now() + interval '1 hour', now(), now())",
    )
    .bind(hex::encode(Sha256::digest(b"mcpauth_seeded")))
    .bind(challenge())
    .bind(client_row)
    .bind(user.id)
    .bind(principal)
    .bind(REDIRECT)
    .bind(MCP_RESOURCE)
    .execute(&app.db.pool)
    .await
    .unwrap();
    let response = app
        .post("/mcp/oauth/token")
        .form(&[
            ("grant_type", "authorization_code"),
            ("client_id", &client),
            ("code", "mcpauth_seeded"),
            ("redirect_uri", REDIRECT),
            ("code_verifier", VERIFIER),
        ])
        .send()
        .await;
    assert_eq!(response.status(), 500);
    let consumed: Option<chrono::NaiveDateTime> =
        sqlx::query_scalar("SELECT consumed_at FROM mcp_oauth_authorization_codes")
            .fetch_one(&app.db.pool)
            .await
            .unwrap();
    assert!(
        consumed.is_none(),
        "the code is still usable once the secret is configured"
    );
}
