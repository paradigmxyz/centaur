//! The broker consent flow: `/oauth/{slug}/start` and `/callback`.

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use centaur_console_api::testing::{TEST_NOW, TestClient, TestResponse};
use centaur_console_db::{Encryptor, oid};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::support::*;

const TOKEN: &str = "oauth2.googleapis.com/token";

fn id_token(claims: &Value) -> String {
    format!("h.{}.s", URL_SAFE_NO_PAD.encode(claims.to_string()))
}

fn google_claims(sub: &str, email: &str) -> Value {
    json!({"aud": "client-id", "iss": "https://accounts.google.com", "sub": sub, "email": email})
}

fn answer_google(f: &Fixture, claims: &Value) {
    f.stub.answer(
        TOKEN,
        200,
        &json!({
            "access_token": "ACCESS-TOKEN-VALUE", "refresh_token": "REFRESH-TOKEN-VALUE", "expires_in": 3600,
            "scope": "scope-a scope-b openid", "id_token": id_token(claims)
        }),
    );
}

fn param(location: &str, key: &str) -> Option<String> {
    url::Url::parse(location)
        .unwrap()
        .query_pairs()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.into_owned())
}

async fn start(f: &Fixture, browser: &mut TestClient, slug: &str, query: &str) -> TestResponse {
    browser
        .send(
            f.app
                .get(&format!("/oauth/{slug}/start{query}"))
                .accept_html(),
        )
        .await
}

async fn callback(
    f: &Fixture,
    browser: &mut TestClient,
    slug: &str,
    state: &str,
    extra: &str,
) -> TestResponse {
    let state = url::form_urlencoded::byte_serialize(state.as_bytes()).collect::<String>();
    browser
        .send(
            f.app
                .get(&format!("/oauth/{slug}/callback?state={state}&{extra}"))
                .accept_html(),
        )
        .await
}

/// Starts a flow and returns its `state`.
async fn started(f: &Fixture, browser: &mut TestClient, slug: &str) -> String {
    let response = start(f, browser, slug, "").await;
    assert_eq!(response.status(), 302, "{}", response.text());
    param(response.location().unwrap(), "state").expect("a state")
}

async fn consent(f: &Fixture, browser: &mut TestClient, slug: &str) -> TestResponse {
    let state = started(f, browser, slug).await;
    callback(f, browser, slug, &state, "code=the-code").await
}

#[derive(sqlx::FromRow)]
struct Credential {
    id: i64,
    oauth_app_id: i64,
    foreign_id: String,
    name: String,
    provider_subject: String,
    provider_email: Option<String>,
    token_endpoint: String,
    scopes: Value,
    dead: bool,
    failure_count: i32,
    created_by_id: i64,
    external_user_key: String,
    expires_at: Option<chrono::NaiveDateTime>,
    next_attempt_at: Option<chrono::NaiveDateTime>,
}

async fn credentials(f: &Fixture) -> Vec<Credential> {
    sqlx::query_as(
        "SELECT id, oauth_app_id, foreign_id, name, provider_subject, provider_email, token_endpoint, scopes, \
         dead, failure_count, created_by_id, external_user_key, expires_at, next_attempt_at \
         FROM broker_credentials ORDER BY id",
    )
    .fetch_all(&f.app.db.pool)
    .await
    .unwrap()
}

// --- start --------------------------------------------------------------------------------

#[tokio::test]
async fn sends_the_user_to_the_provider_with_pkce_and_a_bound_state() {
    let Some(f) = fixture().await else { return };
    f.insert_app("google", "google", false).await;
    let mut browser = f.browser(&f.member);
    let response = start(&f, &mut browser, "google", "").await;
    assert_eq!(response.status(), 302);
    let location = response.location().unwrap().to_owned();
    assert!(
        location.starts_with("https://accounts.google.com/o/oauth2/v2/auth?"),
        "{location}"
    );
    assert_eq!(param(&location, "client_id").as_deref(), Some("client-id"));
    assert_eq!(
        param(&location, "redirect_uri").as_deref(),
        Some("http://www.example.com/oauth/google/callback")
    );
    assert_eq!(param(&location, "response_type").as_deref(), Some("code"));
    assert_eq!(
        param(&location, "code_challenge_method").as_deref(),
        Some("S256")
    );
    assert_eq!(param(&location, "access_type").as_deref(), Some("offline"));
    assert_eq!(param(&location, "prompt").as_deref(), Some("consent"));
    let scope = param(&location, "scope").unwrap();
    assert_eq!(
        scope,
        "scope-a scope-b openid https://www.googleapis.com/auth/userinfo.email"
    );
    assert!(param(&location, "state").is_some() && param(&location, "code_challenge").is_some());
    let cookies = response.set_cookies();
    let flow = cookies
        .iter()
        .find(|cookie| cookie.starts_with("oauth_flow="))
        .expect("a flow cookie");
    assert!(
        flow.contains("HttpOnly") && flow.contains("SameSite=Lax"),
        "{flow}"
    );
    assert!(!flow.contains(&param(&location, "code_challenge").unwrap()));

    // A narrower scope request is honoured; one beyond the app's is refused.
    let response = start(&f, &mut browser, "google", "?scopes=scope-a").await;
    assert_eq!(
        param(response.location().unwrap(), "scope").unwrap(),
        "scope-a openid https://www.googleapis.com/auth/userinfo.email"
    );
    let response = start(&f, &mut browser, "google", "?scopes=scope-a,scope-z").await;
    assert_eq!(response.status(), 422);
    assert!(response.text().contains("not allowed for this integration"));
}

#[tokio::test]
async fn start_gates_the_user_and_the_app() {
    let Some(f) = fixture().await else { return };
    f.insert_app("google", "google", false).await;
    let shared = f.insert_app("team", "github", true).await;
    let disabled = f.insert_app("off", "google", false).await;
    sqlx::query("UPDATE oauth_apps SET enabled = FALSE WHERE id = $1")
        .bind(disabled)
        .execute(&f.app.db.pool)
        .await
        .unwrap();

    let mut anonymous = f.app.client();
    let response = start(&f, &mut anonymous, "google", "?scopes=scope-a").await;
    assert_eq!(
        (response.status(), response.location()),
        (302, Some("/login"))
    );
    let pending = f
        .app
        .create_user("pending@example.test", false, "pending")
        .await;
    let mut browser = f.browser(&pending);
    assert_eq!(
        start(&f, &mut browser, "google", "").await.location(),
        Some("/pending")
    );

    let mut member = f.browser(&f.member);
    let response = start(&f, &mut member, "nope", "").await;
    assert_eq!(response.status(), 404);
    assert!(response.text().contains("Unknown integration."));
    let response = start(&f, &mut member, "off", "").await;
    assert_eq!(response.status(), 422);
    assert!(response.text().contains("This integration is disabled."));
    assert!(
        !response.text().contains("Try again"),
        "a disabled app cannot be retried"
    );
    // Shared apps belong to admins.
    let response = start(&f, &mut member, "team", "").await;
    assert_eq!(
        (response.status(), response.location()),
        (302, Some("/console/integrations"))
    );
    let mut admin = f.browser(&f.admin);
    let response = start(&f, &mut admin, "team", "").await;
    assert!(
        response
            .location()
            .unwrap()
            .starts_with("https://github.com/login/oauth/authorize?")
    );
    let _ = shared;
}

// --- callback -----------------------------------------------------------------------------

#[tokio::test]
async fn a_consent_mints_a_credential_and_its_wrapper() {
    let Some(f) = fixture().await else { return };
    let app_id = f.insert_app("google", "google", false).await;
    answer_google(&f, &google_claims("G-Sub-1", "user@example.test"));
    let mut browser = f.browser(&f.member);
    let start_response = start(&f, &mut browser, "google", "").await;
    let location = start_response.location().unwrap().to_owned();
    let state = param(&location, "state").unwrap();
    let response = callback(&f, &mut browser, "google", &state, "code=the-code").await;
    assert_eq!(
        (response.status(), response.location()),
        (302, Some("/console/integrations")),
        "{}",
        response.text()
    );
    assert!(
        response
            .set_cookies()
            .iter()
            .any(|cookie| cookie.starts_with("oauth_flow=") && cookie.contains("Max-Age=0"))
    );

    let [credential] = &credentials(&f).await[..] else {
        panic!("one credential")
    };
    assert_eq!(credential.oauth_app_id, app_id);
    assert_eq!(credential.provider_subject, "G-Sub-1");
    assert_eq!(credential.foreign_id, "google-google-g-sub-1");
    assert_eq!(credential.name, "Google – user@example.test");
    assert_eq!(
        credential.provider_email.as_deref(),
        Some("user@example.test")
    );
    assert_eq!(
        credential.token_endpoint,
        "https://oauth2.googleapis.com/token"
    );
    assert_eq!(credential.scopes, json!(["scope-a", "scope-b", "openid"]));
    assert_eq!(
        (
            credential.dead,
            credential.failure_count,
            credential.created_by_id
        ),
        (false, 0, f.member.id)
    );
    assert!(!credential.external_user_key.is_empty());
    let now = chrono::DateTime::from_timestamp(TEST_NOW, 0)
        .unwrap()
        .naive_utc();
    assert_eq!(
        credential.expires_at,
        Some(now + chrono::Duration::seconds(3600))
    );
    assert!(
        credential
            .next_attempt_at
            .is_some_and(|at| at < credential.expires_at.unwrap())
    );

    // Tokens are stored encrypted.
    let (access, refresh): (String, String) =
        sqlx::query_as("SELECT access_token, refresh_token FROM broker_credentials")
            .fetch_one(&f.app.db.pool)
            .await
            .unwrap();
    assert_no(
        &format!("{access}{refresh}"),
        &["ACCESS-TOKEN-VALUE", "REFRESH-TOKEN-VALUE"],
    );
    let decrypted = Encryptor::development()
        .decrypt(&centaur_console_db::Ciphertext::from_stored(access))
        .unwrap();
    assert_eq!(decrypted, "ACCESS-TOKEN-VALUE");

    // What was sent upstream: PKCE verifier matching the challenge, the
    // secret in the body, the exact redirect URI.
    let sent = &f.stub.requests_to(TOKEN)[0];
    assert_eq!(
        sent.form_value("grant_type").as_deref(),
        Some("authorization_code")
    );
    assert_eq!(sent.form_value("code").as_deref(), Some("the-code"));
    assert_eq!(
        sent.form_value("client_secret").as_deref(),
        Some("app-secret")
    );
    assert_eq!(
        sent.form_value("redirect_uri").as_deref(),
        Some("http://www.example.com/oauth/google/callback")
    );
    let verifier = sent.form_value("code_verifier").unwrap();
    assert_eq!(
        param(&location, "code_challenge").unwrap(),
        URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
    );

    // The wrapper: a grantable secret serving the credential through a
    // token_broker source, scoped to the provider's API hosts.
    let (secret_id, name, kind, inject): (i64, String, String, Value) = sqlx::query_as(
        "SELECT id, name, kind, inject_config FROM static_secrets WHERE broker_credential_id = $1",
    )
    .bind(credential.id)
    .fetch_one(&f.app.db.pool)
    .await
    .unwrap();
    assert_eq!(
        (name.as_str(), kind.as_str()),
        ("Google – user@example.test token", "custom")
    );
    assert_eq!(
        inject,
        json!({"header": "Authorization", "formatter": "Bearer {{ .Value }}"})
    );
    let source: (String, Value) = sqlx::query_as(
        "SELECT source_type, config FROM secret_sources WHERE static_secret_id = $1",
    )
    .bind(secret_id)
    .fetch_one(&f.app.db.pool)
    .await
    .unwrap();
    assert_eq!(
        source,
        (
            "token_broker".to_owned(),
            json!({"credential_id": oid("bcr", credential.id)})
        )
    );
    let hosts: Vec<String> = sqlx::query_scalar(
        "SELECT host FROM request_rules WHERE static_secret_id = $1 ORDER BY position",
    )
    .bind(secret_id)
    .fetch_all(&f.app.db.pool)
    .await
    .unwrap();
    assert_eq!(hosts, ["*.googleapis.com"]);
}

#[tokio::test]
async fn consenting_again_refreshes_the_tokens_and_leaves_the_wrapper_alone() {
    let Some(f) = fixture().await else { return };
    f.insert_app("google", "google", false).await;
    answer_google(&f, &google_claims("g-sub", "user@example.test"));
    let mut browser = f.browser(&f.member);
    assert_eq!(consent(&f, &mut browser, "google").await.status(), 302);
    let first = credentials(&f).await.remove(0);

    // The credential died and an operator renamed and granted the wrapper.
    sqlx::query("UPDATE broker_credentials SET dead = TRUE, dead_reason = 'invalid_grant', failure_count = 5")
        .execute(&f.app.db.pool)
        .await
        .unwrap();
    let secret_id: i64 =
        sqlx::query_scalar("UPDATE static_secrets SET name = 'Operator name' RETURNING id")
            .fetch_one(&f.app.db.pool)
            .await
            .unwrap();
    let principal = grant(&f, secret_id).await;
    let before = f.version(principal).await;
    f.app
        .clock
        .set(centaur_console_api::testing::at(TEST_NOW + 600));
    f.stub.answer(
        TOKEN,
        200,
        &json!({"access_token": "NEW-ACCESS", "refresh_token": "NEW-REFRESH", "expires_in": 3600, "scope": "scope-a", "id_token": id_token(&google_claims("g-sub", "new@example.test"))}),
    );
    // Another user connects the same account (a different member of the team).
    let mut other = f.browser(&f.admin);
    assert_eq!(consent(&f, &mut other, "google").await.status(), 302);

    let all = credentials(&f).await;
    assert_eq!(all.len(), 1, "one credential per provider account");
    let again = &all[0];
    assert_eq!(again.id, first.id);
    assert_eq!((again.dead, again.failure_count), (false, 0));
    assert_eq!(again.provider_email.as_deref(), Some("new@example.test"));
    assert_eq!(again.scopes, json!(["scope-a"]));
    assert_eq!(
        again.created_by_id, f.member.id,
        "the first connector stays the creator"
    );
    assert_eq!(again.name, first.name);
    let names: Vec<String> = sqlx::query_scalar("SELECT name FROM static_secrets")
        .fetch_all(&f.app.db.pool)
        .await
        .unwrap();
    assert_eq!(names, ["Operator name"]);
    assert_eq!(f.count("secret_sources").await, 1);
    assert_eq!(
        f.version(principal).await,
        before + 1,
        "proxy-sync must re-read the new token"
    );
}

#[tokio::test]
async fn a_github_consent_wraps_the_token_with_the_placeholder_replacement() {
    let Some(f) = fixture().await else { return };
    f.insert_app("github", "github", false).await;
    f.stub.answer(
        "github.com/login/oauth/access_token",
        200,
        &json!({"access_token": "gho_secret", "scope": "repo,read:org"}),
    );
    f.stub.answer(
        "api.github.com/user",
        200,
        &json!({"id": 4242, "login": "Octo", "name": "Octo Cat", "email": null}),
    );
    let mut browser = f.browser(&f.member);
    let response = consent(&f, &mut browser, "github").await;
    assert_eq!(response.status(), 302, "{}", response.text());
    let credential = credentials(&f).await.remove(0);
    assert_eq!(credential.provider_subject, "4242");
    assert_eq!(credential.name, "GitHub – Octo Cat");
    assert_eq!(credential.scopes, json!(["repo", "read:org"]));
    assert!(
        credential.next_attempt_at.is_none() && credential.expires_at.is_none(),
        "GitHub tokens do not expire"
    );
    assert_eq!(
        f.stub.requests_to("api.github.com/user")[0]
            .authorization
            .as_deref(),
        Some("Bearer gho_secret")
    );
    let (kind, replace): (String, Value) =
        sqlx::query_as("SELECT kind, replace_config FROM static_secrets")
            .fetch_one(&f.app.db.pool)
            .await
            .unwrap();
    assert_eq!(kind, "github_token");
    assert_eq!(replace["proxy_value"], "GITHUB_TOKEN");
    let hosts: Vec<String> = sqlx::query_scalar("SELECT host FROM request_rules ORDER BY position")
        .fetch_all(&f.app.db.pool)
        .await
        .unwrap();
    assert_eq!(
        hosts,
        ["api.github.com", "github.com", "api.githubcopilot.com"]
    );
    let labels: Value = sqlx::query_scalar("SELECT labels FROM broker_credentials")
        .fetch_one(&f.app.db.pool)
        .await
        .unwrap();
    assert_eq!(labels, json!({"github_login": "Octo"}));
}

#[tokio::test]
async fn a_shared_app_stays_with_one_account() {
    let Some(f) = fixture().await else { return };
    f.insert_app("team", "github", true).await;
    f.stub.answer(
        "github.com/login/oauth/access_token",
        200,
        &json!({"access_token": "t"}),
    );
    f.stub.answer(
        "api.github.com/user",
        200,
        &json!({"id": 1, "login": "first"}),
    );
    let mut admin = f.browser(&f.admin);
    let response = consent(&f, &mut admin, "team").await;
    assert_eq!(
        response
            .location()
            .map(|l| l.starts_with("/console/oauth_apps/oap_")),
        Some(true),
        "{:?}",
        response.location()
    );
    f.stub.answer(
        "api.github.com/user",
        200,
        &json!({"id": 2, "login": "second"}),
    );
    let response = consent(&f, &mut admin, "team").await;
    assert_eq!(response.status(), 422);
    assert!(
        response.text().contains("account_mismatch"),
        "{}",
        response.text()
    );
    assert_eq!(f.count("broker_credentials").await, 1);
}

#[tokio::test]
async fn the_callback_refuses_what_is_not_this_browsers_fresh_flow() {
    let Some(f) = fixture().await else { return };
    f.insert_app("google", "google", false).await;
    f.insert_app("other", "google", false).await;
    answer_google(&f, &google_claims("g-sub", "user@example.test"));
    let mut browser = f.browser(&f.member);
    let state = started(&f, &mut browser, "google").await;

    let page = |response: &TestResponse, status: u16, text: &str| {
        assert_eq!(response.status(), status, "{}", response.text());
        assert!(response.text().contains(text), "{}", response.text());
    };
    // A tampered or foreign state.
    let response = callback(&f, &mut browser, "google", "garbage", "code=c").await;
    page(&response, 400, "invalid or has expired");
    // The state of one app on another's callback.
    let mut again = f.browser(&f.member);
    let state_for_google = started(&f, &mut again, "google").await;
    let response = callback(&f, &mut again, "other", &state_for_google, "code=c").await;
    page(&response, 400, "no longer available");
    // Expired.
    let mut late = f.browser(&f.member);
    let late_state = started(&f, &mut late, "google").await;
    f.app
        .clock
        .set(centaur_console_api::testing::at(TEST_NOW + 11 * 60));
    page(
        &callback(&f, &mut late, "google", &late_state, "code=c").await,
        400,
        "invalid or has expired",
    );
    f.app.clock.set(centaur_console_api::testing::at(TEST_NOW));
    // A browser that did not start the flow has no cookie.
    let mut stranger = f.browser(&f.member);
    page(
        &callback(&f, &mut stranger, "google", &state, "code=c").await,
        400,
        "another browser",
    );
    // A disabled app.
    sqlx::query("UPDATE oauth_apps SET enabled = FALSE WHERE slug = 'google'")
        .execute(&f.app.db.pool)
        .await
        .unwrap();
    page(
        &callback(&f, &mut browser, "google", &state, "code=c").await,
        400,
        "no longer available",
    );
    assert!(
        f.stub.requests_to(TOKEN).is_empty(),
        "nothing reached the token endpoint"
    );
    assert_eq!(f.count("broker_credentials").await, 0);
}

#[tokio::test]
async fn a_denied_or_failed_consent_shows_why_and_stores_nothing() {
    let Some(f) = fixture().await else { return };
    f.insert_app("google", "google", false).await;
    let mut browser = f.browser(&f.member);
    let state = started(&f, &mut browser, "google").await;
    let response = callback(&f, &mut browser, "google", &state, "error=access_denied").await;
    assert_eq!(response.status(), 422);
    assert!(
        response.text().contains("Not connected")
            && response
                .text()
                .contains("Consent was not granted (access_denied).")
    );
    assert!(
        response.text().contains("/oauth/google/start"),
        "offers to try again"
    );

    // The flow cookie is single use: replaying the state needs a new start.
    let response = callback(&f, &mut browser, "google", &state, "code=c").await;
    assert_eq!(response.status(), 400);

    // The IdP refuses the code.
    f.stub.answer(
        TOKEN,
        400,
        &json!({"error": "invalid_grant", "error_description": "SECRET-DETAIL"}),
    );
    let response = consent(&f, &mut browser, "google").await;
    assert_eq!(response.status(), 422);
    assert!(
        response
            .text()
            .contains("Connecting the integration failed (invalid_grant).")
    );
    assert_no(&response.text(), &["SECRET-DETAIL"]);

    // A mismatched id_token audience is an identity failure.
    answer_google(
        &f,
        &json!({"aud": "someone-else", "iss": "https://accounts.google.com", "sub": "s"}),
    );
    let response = consent(&f, &mut browser, "google").await;
    assert!(
        response.text().contains("id_token_aud_mismatch"),
        "{}",
        response.text()
    );
    // No code at all.
    let state = started(&f, &mut browser, "google").await;
    let response = callback(&f, &mut browser, "google", &state, "").await;
    assert!(response.text().contains("missing_code"));
    assert_eq!(f.count("broker_credentials").await, 0);
}

#[tokio::test]
async fn tokens_never_reach_a_page_or_a_redirect() {
    let Some(f) = fixture().await else { return };
    f.insert_app("google", "google", false).await;
    answer_google(&f, &google_claims("g-sub", "user@example.test"));
    let mut browser = f.browser(&f.member);
    let response = consent(&f, &mut browser, "google").await;
    assert_no(
        &format!("{:?}{}", response.location(), response.text()),
        &["ACCESS-TOKEN-VALUE", "REFRESH-TOKEN-VALUE", "app-secret"],
    );
}

#[tokio::test]
async fn a_mercator_consent_identifies_the_wallet_from_a_server_sent_event() {
    let Some(f) = fixture().await else { return };
    f.insert_app("mercator", "mercator", true).await;
    f.stub.answer(
        "mercator.sh/oauth/token",
        200,
        &json!({"access_token": "AT", "refresh_token": "RT", "expires_in": 3600, "scope": "mercator:tools"}),
    );
    let status = json!({"result": {"structuredContent": {
        "oauthAuthenticated": true,
        "account": {"walletAddress": "0xAbCdEf0123456789aBcDeF0123456789abcdef01"}
    }}});
    f.stub.answer_raw(
        "mercator.sh/mcp/auth",
        200,
        "text/event-stream",
        &format!("event: message\ndata: {status}\n\n"),
    );
    let mut admin = f.browser(&f.admin);
    let response = consent(&f, &mut admin, "mercator").await;
    assert_eq!(response.status(), 302, "{}", response.text());
    let credential = credentials(&f).await.remove(0);
    assert_eq!(
        credential.provider_subject,
        "0xabcdef0123456789abcdef0123456789abcdef01"
    );
    assert_eq!(
        credential.name,
        "Mercator – 0xAbCdEf0123456789aBcDeF0123456789abcdef01"
    );
    let identity = &f.stub.requests_to("mercator.sh/mcp/auth")[0];
    assert_eq!(identity.method, http::Method::POST);
    assert_eq!(identity.authorization.as_deref(), Some("Bearer AT"));
    let (host, methods, paths): (String, Value, Value) =
        sqlx::query_as("SELECT host, http_methods, paths FROM request_rules")
            .fetch_one(&f.app.db.pool)
            .await
            .unwrap();
    assert_eq!(
        (host.as_str(), methods, paths),
        ("mercator.sh", json!(["POST"]), json!(["/mcp/auth"]))
    );

    // Without the Mercator scope nothing is stored.
    for table in [
        "request_rules",
        "secret_sources",
        "static_secrets",
        "broker_credentials",
    ] {
        sqlx::query(&format!("DELETE FROM {table}"))
            .execute(&f.app.db.pool)
            .await
            .unwrap();
    }
    f.stub.answer(
        "mercator.sh/oauth/token",
        200,
        &json!({"access_token": "AT", "refresh_token": "RT", "scope": "other"}),
    );
    let response = consent(&f, &mut admin, "mercator").await;
    assert!(
        response.text().contains("missing_scope"),
        "{}",
        response.text()
    );
    assert_eq!(f.count("broker_credentials").await, 0);
}

#[tokio::test]
async fn a_custom_provider_uses_its_endpoints_resource_and_userinfo() {
    let Some(f) = fixture().await else { return };
    let app_id = f.insert_app("acme", "custom", false).await;
    sqlx::query("UPDATE oauth_apps SET provider_config = $2 WHERE id = $1")
        .bind(app_id)
        .bind(json!({
            "authorization_endpoint": "https://idp.example/authorize?old=1",
            "token_endpoint": "https://idp.example/token",
            "userinfo_endpoint": "https://idp.example/userinfo",
            "resource": "https://api.example/v1/mcp"
        }))
        .execute(&f.app.db.pool)
        .await
        .unwrap();
    f.stub.answer(
        "idp.example/token",
        200,
        &json!({"access_token": "AT", "refresh_token": "RT", "expires_in": 600}),
    );
    f.stub.answer(
        "idp.example/userinfo",
        200,
        &json!({"sub": "u-1", "email": "x@example.test", "name": "X"}),
    );
    let mut browser = f.browser(&f.member);
    let response = start(&f, &mut browser, "acme", "").await;
    let location = response.location().unwrap().to_owned();
    assert!(
        location.starts_with("https://idp.example/authorize?client_id="),
        "the endpoint's own query is replaced: {location}"
    );
    assert_eq!(
        param(&location, "resource").as_deref(),
        Some("https://api.example/v1/mcp")
    );
    let state = param(&location, "state").unwrap();
    let response = callback(&f, &mut browser, "acme", &state, "code=c").await;
    assert_eq!(response.status(), 302, "{}", response.text());

    let sent = &f.stub.requests_to("idp.example/token")[0];
    assert_eq!(sent.query, None);
    assert_eq!(
        sent.form_value("resource").as_deref(),
        Some("https://api.example/v1/mcp")
    );
    assert_eq!(
        f.stub.requests_to("idp.example/userinfo")[0]
            .authorization
            .as_deref(),
        Some("Bearer AT")
    );
    let credential = credentials(&f).await.remove(0);
    assert_eq!(
        (
            credential.provider_subject.as_str(),
            credential.name.as_str()
        ),
        ("u-1", "acme – X")
    );
    assert_eq!(credential.token_endpoint, "https://idp.example/token");
    let (host, paths): (String, Value) = sqlx::query_as("SELECT host, paths FROM request_rules")
        .fetch_one(&f.app.db.pool)
        .await
        .unwrap();
    assert_eq!((host.as_str(), paths), ("api.example", json!(["/v1/mcp"])));
}
