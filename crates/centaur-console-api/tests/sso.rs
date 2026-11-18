//! Console SSO against a stub identity provider (debug builds point Google
//! and Slack at `CENTAUR_CONSOLE_SSO_TEST_BASE_URL`).

use std::sync::{Arc, Mutex};

use axum::{Json, Router, extract::State, routing::post};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use centaur_console_api::{
    identity,
    testing::{TestApp, TestClient, TestResponse},
};
use serde_json::{Value, json};

/// A stub token endpoint that answers with the id_token claims the test sets.
type FormFields = Vec<(String, String)>;

#[derive(Clone, Default)]
struct Idp {
    claims: Arc<Mutex<Value>>,
    token_requests: Arc<Mutex<Vec<FormFields>>>,
}

async fn token(State(idp): State<Idp>, body: String) -> Json<Value> {
    idp.token_requests.lock().unwrap().push(
        url::form_urlencoded::parse(body.as_bytes())
            .into_owned()
            .collect(),
    );
    let payload = URL_SAFE_NO_PAD.encode(idp.claims.lock().unwrap().to_string());
    Json(json!({"id_token": format!("h.{payload}.s"), "access_token": "at"}))
}

async fn serve_idp() -> (Idp, String) {
    let idp = Idp::default();
    let router = Router::new()
        .route("/google/token", post(token))
        .route("/slack/token", post(token))
        .with_state(idp.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    (idp, base)
}

async fn app_with(idp_base: &str, extra: &[(&str, &str)]) -> Option<TestApp> {
    let mut env = vec![
        ("CENTAUR_CONSOLE_SSO_TEST_BASE_URL", idp_base),
        ("CENTAUR_CONSOLE_GOOGLE_CLIENT_ID", "google-client"),
        ("CENTAUR_CONSOLE_GOOGLE_CLIENT_SECRET", "google-secret"),
        ("CENTAUR_CONSOLE_SLACK_CLIENT_ID", "slack-client"),
        ("CENTAUR_CONSOLE_SLACK_CLIENT_SECRET", "slack-secret"),
    ];
    env.extend_from_slice(extra);
    TestApp::with_env(identity::routes(), &env).await
}

fn query_param(location: &str, name: &str) -> String {
    url::Url::parse(location)
        .unwrap()
        .query_pairs()
        .find(|(key, _)| key == name)
        .unwrap_or_else(|| panic!("{name} in {location}"))
        .1
        .into_owned()
}

struct Flow {
    browser: TestClient,
    state: String,
    nonce: String,
}

async fn start(app: &TestApp, provider: &str) -> Flow {
    let mut browser = app.client();
    let response = browser
        .send(app.get(&format!("/auth/{provider}/start")))
        .await;
    assert_eq!(response.status(), 302);
    let location = response.location().unwrap().to_owned();
    Flow {
        state: query_param(&location, "state"),
        nonce: query_param(&location, "nonce"),
        browser,
    }
}

async fn callback(app: &TestApp, flow: &mut Flow, provider: &str, extra: &str) -> TestResponse {
    let path = format!(
        "/auth/{provider}/callback?state={}&{extra}",
        url::form_urlencoded::byte_serialize(flow.state.as_bytes()).collect::<String>()
    );
    flow.browser.send(app.get(&path)).await
}

fn google_claims(nonce: &str, extra: Value) -> Value {
    let mut claims = json!({
        "iss": "https://accounts.google.com", "aud": "google-client", "sub": "google-sub-1",
        "exp": centaur_console_api::testing::TEST_NOW + 600, "nonce": nonce,
        "email": "New.Person@acme.example", "email_verified": true, "name": "New Person",
    });
    claims
        .as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    claims
}

async fn flash_alert(app: &TestApp, browser: &mut TestClient) -> String {
    let page = browser.send(app.get("/login").accept_html()).await.text();
    let start = page
        .find("role=\"alert\">")
        .map(|i| i + "role=\"alert\">".len());
    start.map_or_else(String::new, |start| {
        page[start..page[start..].find('<').unwrap() + start].to_owned()
    })
}

#[tokio::test]
async fn start_redirects_to_the_provider_with_state_nonce_and_pkce() {
    let (_, base) = serve_idp().await;
    let Some(app) = app_with(
        &base,
        &[("CENTAUR_CONSOLE_PUBLIC_URL", "https://console.example")],
    )
    .await
    else {
        return;
    };
    let mut browser = app.client();
    let response = browser.send(app.get("/auth/google/start")).await;
    assert_eq!(response.status(), 302);
    let location = response.location().unwrap();
    assert!(
        location.starts_with(&format!("{base}/google/authorize?")),
        "{location}"
    );
    assert_eq!(query_param(location, "client_id"), "google-client");
    assert_eq!(
        query_param(location, "redirect_uri"),
        "https://console.example/auth/google/callback"
    );
    assert_eq!(query_param(location, "response_type"), "code");
    assert_eq!(query_param(location, "scope"), "openid email profile");
    assert_eq!(query_param(location, "code_challenge_method"), "S256");
    assert!(query_param(location, "code_challenge").len() >= 43);
    assert!(browser.has_cookie("console_login_flow"));
    let cookie = response
        .set_cookies()
        .into_iter()
        .find(|c| c.starts_with("console_login_flow="))
        .unwrap();
    assert!(
        cookie.contains("HttpOnly") && cookie.contains("SameSite=Lax") && cookie.contains("Secure")
    );
    // Slack's confidential exchange uses no PKCE.
    let slack = app.get("/auth/slack/start").send().await;
    assert!(!slack.location().unwrap().contains("code_challenge"));
}

#[tokio::test]
async fn unknown_or_unconfigured_providers_go_back_to_login() {
    let (_, base) = serve_idp().await;
    let Some(app) = app_with(&base, &[]).await else {
        return;
    };
    for path in [
        "/auth/okta/start",
        "/auth/github/start",
        "/auth/okta/callback",
        "/auth/github/callback",
    ] {
        let mut browser = app.client();
        let response = browser.send(app.get(path)).await;
        assert_eq!(
            (response.status(), response.location()),
            (302, Some("/login")),
            "{path}"
        );
        assert_eq!(
            flash_alert(&app, &mut browser).await,
            "That sign-in method is not available."
        );
    }
}

#[tokio::test]
async fn a_complete_sign_in_creates_the_user_and_a_session() {
    let (idp, base) = serve_idp().await;
    let Some(app) = app_with(
        &base,
        &[(
            "CENTAUR_CONSOLE_BOOTSTRAP_ADMINS",
            "new.person@acme.example",
        )],
    )
    .await
    else {
        return;
    };
    let mut flow = start(&app, "google").await;
    *idp.claims.lock().unwrap() = google_claims(&flow.nonce, json!({}));

    let response = callback(&app, &mut flow, "google", "code=the-code").await;
    assert_eq!(
        (response.status(), response.location()),
        (302, Some("/console/principals"))
    );
    // The code was exchanged with the client's credentials and the PKCE verifier.
    {
        let requests = idp.token_requests.lock().unwrap();
        let sent = |name: &str| {
            requests[0]
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.as_str())
        };
        assert_eq!(sent("code"), Some("the-code"));
        assert_eq!(sent("client_id"), Some("google-client"));
        assert_eq!(sent("client_secret"), Some("google-secret"));
        assert_eq!(sent("grant_type"), Some("authorization_code"));
        assert!(sent("code_verifier").is_some_and(|verifier| verifier.len() >= 43));
    }
    // The one-shot flow cookie is gone; the session is signed in.
    assert!(!flow.browser.has_cookie("console_login_flow"));
    let me = flow
        .browser
        .send(app.get("/api/v1/console/me"))
        .await
        .json();
    assert_eq!(me["data"]["user"]["email"], "new.person@acme.example");
    assert_eq!(me["data"]["user"]["name"], "New Person");
    assert_eq!(me["data"]["acting_admin"], true, "bootstrap admin");

    let identities: Vec<(String, String, bool)> =
        sqlx::query_as("SELECT provider, subject, email_verified FROM user_identities")
            .fetch_all(&app.db.pool)
            .await
            .unwrap();
    assert_eq!(
        identities,
        [("google".to_owned(), "google-sub-1".to_owned(), true)]
    );
    // The authorization code is single use: the flow cookie and state are spent.
    let again = callback(&app, &mut flow, "google", "code=the-code").await;
    assert_eq!(again.location(), Some("/login"));
    assert_eq!(identities.len(), 1);
}

#[tokio::test]
async fn a_returning_identity_signs_in_the_same_user() {
    let (idp, base) = serve_idp().await;
    let Some(app) = app_with(&base, &[]).await else {
        return;
    };
    for _ in 0..2 {
        let mut flow = start(&app, "google").await;
        *idp.claims.lock().unwrap() =
            google_claims(&flow.nonce, json!({"email": "changed@acme.example"}));
        let response = callback(&app, &mut flow, "google", "code=c").await;
        assert_eq!(response.location(), Some("/console/integrations"));
    }
    let users: i64 = sqlx::query_scalar("SELECT count(*) FROM users")
        .fetch_one(&app.db.pool)
        .await
        .unwrap();
    let identities: i64 = sqlx::query_scalar("SELECT count(*) FROM user_identities")
        .fetch_one(&app.db.pool)
        .await
        .unwrap();
    assert_eq!((users, identities), (1, 1));
}

#[tokio::test]
async fn only_a_verified_email_links_to_an_existing_account() {
    let (idp, base) = serve_idp().await;
    let Some(app) = app_with(&base, &[]).await else {
        return;
    };
    let existing = app
        .create_user("new.person@acme.example", true, "active")
        .await;

    // Unverified email: no linking, and no second account with that email.
    let mut flow = start(&app, "google").await;
    *idp.claims.lock().unwrap() = google_claims(&flow.nonce, json!({"email_verified": false}));
    let response = callback(&app, &mut flow, "google", "code=c").await;
    assert_eq!(response.location(), Some("/login"));
    assert_eq!(
        flash_alert(&app, &mut flow.browser).await,
        "Sign in failed while setting up your account."
    );
    let linked: i64 = sqlx::query_scalar("SELECT count(*) FROM user_identities")
        .fetch_one(&app.db.pool)
        .await
        .unwrap();
    assert_eq!(linked, 0);

    // Verified: the identity joins the existing (admin) account.
    let mut flow = start(&app, "google").await;
    *idp.claims.lock().unwrap() = google_claims(&flow.nonce, json!({}));
    let response = callback(&app, &mut flow, "google", "code=c").await;
    assert_eq!(response.location(), Some("/console/principals"));
    let owner: i64 = sqlx::query_scalar("SELECT user_id FROM user_identities")
        .fetch_one(&app.db.pool)
        .await
        .unwrap();
    assert_eq!(owner, existing.id);
}

#[tokio::test]
async fn pending_users_are_activated_and_disabled_users_are_refused() {
    let (idp, base) = serve_idp().await;
    let Some(app) = app_with(&base, &[]).await else {
        return;
    };
    app.create_user("new.person@acme.example", false, "pending")
        .await;
    let mut flow = start(&app, "google").await;
    *idp.claims.lock().unwrap() = google_claims(&flow.nonce, json!({}));
    assert_eq!(
        callback(&app, &mut flow, "google", "code=c")
            .await
            .location(),
        Some("/console/integrations")
    );
    let status: (String, bool) =
        sqlx::query_as("SELECT status, approved_at IS NOT NULL FROM users")
            .fetch_one(&app.db.pool)
            .await
            .unwrap();
    assert_eq!(status, ("active".to_owned(), true));

    sqlx::query("UPDATE users SET status = 'disabled'")
        .execute(&app.db.pool)
        .await
        .unwrap();
    let mut flow = start(&app, "google").await;
    *idp.claims.lock().unwrap() = google_claims(&flow.nonce, json!({}));
    let response = callback(&app, &mut flow, "google", "code=c").await;
    assert_eq!(response.location(), Some("/login"));
    assert_eq!(
        flash_alert(&app, &mut flow.browser).await,
        "Your account is disabled."
    );
    assert_eq!(
        flow.browser
            .send(app.get("/api/v1/console/me"))
            .await
            .status(),
        401
    );
}

#[tokio::test]
async fn the_email_domain_allow_list_applies_before_anything_is_created() {
    let (idp, base) = serve_idp().await;
    let Some(app) = app_with(
        &base,
        &[("CENTAUR_CONSOLE_SSO_EMAIL_DOMAINS", "other.example")],
    )
    .await
    else {
        return;
    };
    let mut flow = start(&app, "google").await;
    *idp.claims.lock().unwrap() = google_claims(&flow.nonce, json!({}));
    let response = callback(&app, &mut flow, "google", "code=c").await;
    assert_eq!(response.location(), Some("/login"));
    assert_eq!(
        flash_alert(&app, &mut flow.browser).await,
        "That email domain is not allowed to access the console."
    );
    let users: i64 = sqlx::query_scalar("SELECT count(*) FROM users")
        .fetch_one(&app.db.pool)
        .await
        .unwrap();
    assert_eq!(users, 0);
}

#[tokio::test]
async fn callbacks_without_the_browsers_own_flow_are_refused() {
    let (idp, base) = serve_idp().await;
    let Some(app) = app_with(&base, &[]).await else {
        return;
    };
    let mut victim = start(&app, "google").await;
    *idp.claims.lock().unwrap() = google_claims(&victim.nonce, json!({}));

    // A different browser (no flow cookie) presenting the victim's state.
    let mut attacker = app.client();
    let path = format!(
        "/auth/google/callback?code=c&state={}",
        url::form_urlencoded::byte_serialize(victim.state.as_bytes()).collect::<String>()
    );
    assert_eq!(
        attacker.send(app.get(&path)).await.location(),
        Some("/login")
    );
    assert_eq!(
        flash_alert(&app, &mut attacker).await,
        "This sign-in link is invalid or expired. Start again."
    );

    // Forged, truncated, and wrong-provider states.
    for state in ["forged", "", &victim.state[..victim.state.len() - 2]] {
        let mut browser = app.client();
        browser.send(app.get("/auth/google/start")).await;
        let path = format!("/auth/google/callback?code=c&state={state}");
        assert_eq!(
            browser.send(app.get(&path)).await.location(),
            Some("/login"),
            "{state:?}"
        );
    }
    let path = format!(
        "/auth/slack/callback?code=c&state={}",
        url::form_urlencoded::byte_serialize(victim.state.as_bytes()).collect::<String>()
    );
    assert_eq!(
        victim.browser.send(app.get(&path)).await.location(),
        Some("/login")
    );
    let users: i64 = sqlx::query_scalar("SELECT count(*) FROM users")
        .fetch_one(&app.db.pool)
        .await
        .unwrap();
    assert_eq!(users, 0);
    assert!(
        idp.token_requests.lock().unwrap().is_empty(),
        "no code was ever exchanged"
    );
}

#[tokio::test]
async fn tampered_id_tokens_and_provider_errors_do_not_sign_in() {
    let (idp, base) = serve_idp().await;
    let Some(app) = app_with(&base, &[]).await else {
        return;
    };
    for (extra, alert) in [
        (
            json!({"aud": "someone-else"}),
            "Sign in failed. Please try again.",
        ),
        (
            json!({"iss": "https://evil.example"}),
            "Sign in failed. Please try again.",
        ),
        (json!({"exp": 1}), "Sign in failed. Please try again."),
        (
            json!({"nonce": "other"}),
            "Sign in failed. Please try again.",
        ),
        (json!({"sub": ""}), "Sign in failed. Please try again."),
    ] {
        let mut flow = start(&app, "google").await;
        *idp.claims.lock().unwrap() = google_claims(&flow.nonce, extra.clone());
        let response = callback(&app, &mut flow, "google", "code=c").await;
        assert_eq!(response.location(), Some("/login"), "{extra}");
        assert_eq!(flash_alert(&app, &mut flow.browser).await, alert, "{extra}");
    }
    let mut flow = start(&app, "google").await;
    let response = callback(&app, &mut flow, "google", "error=access_denied").await;
    assert_eq!(response.location(), Some("/login"));
    assert_eq!(
        flash_alert(&app, &mut flow.browser).await,
        "Sign in was canceled."
    );
    let users: i64 = sqlx::query_scalar("SELECT count(*) FROM users")
        .fetch_one(&app.db.pool)
        .await
        .unwrap();
    assert_eq!(users, 0);
}

#[tokio::test]
async fn slack_identities_record_their_workspace() {
    let (idp, base) = serve_idp().await;
    let Some(app) = app_with(&base, &[]).await else {
        return;
    };
    let mut flow = start(&app, "slack").await;
    *idp.claims.lock().unwrap() = json!({
        "iss": "https://slack.com", "aud": "slack-client", "sub": "U123",
        "exp": centaur_console_api::testing::TEST_NOW + 600, "nonce": flow.nonce,
        "email": "s@acme.example", "email_verified": true,
        "https://slack.com/team_id": " T999 ",
    });
    let response = callback(&app, &mut flow, "slack", "code=c").await;
    assert_eq!(response.location(), Some("/console/integrations"));
    let team: Option<String> =
        sqlx::query_scalar("SELECT team_id FROM user_identities WHERE provider = 'slack'")
            .fetch_one(&app.db.pool)
            .await
            .unwrap();
    assert_eq!(team.as_deref(), Some("T999"));
}

/// Regression (bc32ec #4): two first sign-ins of one person at the same
/// moment must both succeed; the loser links to the winner's user instead of
/// failing with "Sign in failed while setting up your account.".
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_first_sign_ins_link_to_one_user() {
    let (_idp, base) = serve_idp().await;
    let Some(app) = app_with(&base, &[]).await else {
        return;
    };
    let policy = centaur_console_api::identity::settings::SsoPolicy::default();
    let now = app.now();
    fn identity(subject: &str) -> centaur_console_api::identity::provision::SsoIdentity {
        centaur_console_api::identity::provision::SsoIdentity {
            subject: subject.to_owned(),
            email: Some("race@acme.example".into()),
            email_verified: true,
            name: None,
            team_id: None,
        }
    }
    async fn attempt(
        pool: sqlx::PgPool,
        now: chrono::DateTime<chrono::Utc>,
        policy: centaur_console_api::identity::settings::SsoPolicy,
        subject: &'static str,
    ) -> Result<
        centaur_console_api::identity::provision::Provisioned,
        centaur_console_api::identity::provision::ProvisionError,
    > {
        centaur_console_api::identity::provision::link_or_provision(
            &pool,
            now,
            "google",
            &identity(subject),
            &policy,
        )
        .await
    }
    // Hold the users table so both attempts take their first lookup before
    // either inserts: the race is then deterministic rather than lucky.
    let mut lock = app.db.pool.begin().await.unwrap();
    sqlx::query("LOCK TABLE users IN ACCESS EXCLUSIVE MODE")
        .execute(&mut *lock)
        .await
        .unwrap();
    let first = tokio::spawn(attempt(
        app.db.pool.clone(),
        now,
        policy.clone(),
        "google-sub-a",
    ));
    let second = tokio::spawn(attempt(
        app.db.pool.clone(),
        now,
        policy.clone(),
        "google-sub-b",
    ));
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    lock.commit().await.unwrap();
    let (first, second) = (
        first.await.unwrap().expect("first sign-in"),
        second.await.unwrap().expect("second sign-in"),
    );
    assert_eq!(first.id, second.id, "both sign-ins reached one user");
    let users: i64 = sqlx::query_scalar("SELECT count(*) FROM users")
        .fetch_one(&app.db.pool)
        .await
        .unwrap();
    let identities: i64 = sqlx::query_scalar("SELECT count(*) FROM user_identities")
        .fetch_one(&app.db.pool)
        .await
        .unwrap();
    assert_eq!((users, identities), (1, 2));
}
