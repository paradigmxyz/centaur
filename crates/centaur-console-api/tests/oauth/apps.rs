//! `/oauth_apps`, `/oauth_providers`, presets, provider details, and the
//! integrations page.

use centaur_console_api::{auth::Access, testing::TestResponse};
use centaur_console_db::oid;
use http::Method;
use serde_json::{Value, json};

use super::support::*;

fn app_body() -> Value {
    json!({"data": {
        "slug": "google", "description": "Google", "provider": "google",
        "client_id": "cid", "client_secret": "CLIENT-SECRET-VALUE",
        "allowed_scopes": ["openid", "email"], "labels": {"team": "platform"}
    }})
}

async fn bearer_post(f: &Fixture, path: &str, body: &Value) -> TestResponse {
    let key = f.admin_key().await;
    f.app.post(path).bearer(&key).json(body).send().await
}

async fn bearer_patch(f: &Fixture, path: &str, body: &Value) -> TestResponse {
    let key = f.admin_key().await;
    f.app.patch(path).bearer(&key).json(body).send().await
}

async fn bearer_get(f: &Fixture, path: &str) -> TestResponse {
    let key = f.admin_key().await;
    f.app.get(path).bearer(&key).send().await
}

async fn console(f: &Fixture, method: Method, path: &str, body: Option<&Value>) -> TestResponse {
    let browser = f.browser(&f.admin);
    let mut browser = browser;
    let mut request = f.app.request(method, path);
    if let Some(body) = body {
        request = request.json(body);
    }
    let request = request.csrf(&browser);
    browser.send(request).await
}

// --- authorization ------------------------------------------------------------------

#[tokio::test]
async fn bearer_routes_need_an_admin_key() {
    let Some(f) = fixture().await else { return };
    let app = &f.app;
    app.assert_bearer_access(Method::GET, "/api/v1/oauth_apps", None)
        .await;
    app.assert_bearer_access(
        Method::POST,
        "/api/v1/oauth_apps",
        Some(&json!({"data": {}})),
    )
    .await;
    app.assert_bearer_access(Method::GET, "/api/v1/oauth_apps/lookup/google", None)
        .await;
    app.assert_bearer_access(Method::GET, "/api/v1/oauth_apps/oap_x", None)
        .await;
    app.assert_bearer_access(
        Method::PATCH,
        "/api/v1/oauth_apps/google",
        Some(&json!({"data": {}})),
    )
    .await;
    app.assert_bearer_access(Method::DELETE, "/api/v1/oauth_apps/oap_x", None)
        .await;
}

#[tokio::test]
async fn console_routes_need_an_admin_session() {
    let Some(f) = fixture().await else { return };
    let app = &f.app;
    let empty = json!({});
    for (method, path, body) in [
        (Method::GET, "/api/v1/console/oauth_apps", None),
        (Method::POST, "/api/v1/console/oauth_apps", Some(&empty)),
        (Method::GET, "/api/v1/console/oauth_apps/oap_x", None),
        (
            Method::PATCH,
            "/api/v1/console/oauth_apps/oap_x",
            Some(&empty),
        ),
        (Method::DELETE, "/api/v1/console/oauth_apps/oap_x", None),
        (
            Method::GET,
            "/api/v1/console/oauth_apps/oap_x/credentials",
            None,
        ),
        (
            Method::GET,
            "/api/v1/console/oauth_apps/oap_x/provider_details",
            None,
        ),
        (
            Method::POST,
            "/api/v1/console/oauth_apps/presets/mercator",
            None,
        ),
        (Method::GET, "/api/v1/console/oauth_providers", None),
    ] {
        app.assert_console_access(Access::Admin, method, path, body)
            .await;
    }
    app.assert_console_access(
        Access::Active,
        Method::GET,
        "/api/v1/console/integrations",
        None,
    )
    .await;
}

// --- CRUD -----------------------------------------------------------------------------

#[tokio::test]
async fn creates_and_reads_an_app_without_ever_returning_its_secret() {
    let Some(f) = fixture().await else { return };
    let response = bearer_post(&f, "/api/v1/oauth_apps", &app_body()).await;
    assert_eq!(response.status(), 201, "{}", response.text());
    let created = response.json()["data"].clone();
    let id = created["id"].as_str().unwrap().to_owned();
    assert!(id.starts_with("oap_"));
    assert_eq!(created["slug"], "google");
    assert_eq!(created["client_id"], "cid");
    assert_eq!(created["labels"], json!({"team": "platform"}));
    assert_eq!(created["allowed_scopes"], json!(["openid", "email"]));
    assert_eq!(created["provider_config"], json!({}));
    assert_eq!(
        [
            created["enabled"].clone(),
            created["always_available"].clone(),
            created["shared"].clone()
        ],
        [json!(true), json!(false), json!(false)]
    );
    assert!(created["created_at"].as_str().unwrap().ends_with('Z'));
    assert!(created.get("client_secret").is_none());
    assert_no(&response.text(), &["CLIENT-SECRET-VALUE"]);

    // The secret is stored encrypted, in the ActiveRecord format.
    let stored: String =
        sqlx::query_scalar("SELECT client_secret FROM oauth_apps WHERE slug = 'google'")
            .fetch_one(&f.app.db.pool)
            .await
            .unwrap();
    assert_no(&stored, &["CLIENT-SECRET-VALUE"]);

    let shown = bearer_get(&f, &format!("/api/v1/oauth_apps/{id}")).await;
    assert_eq!(shown.json()["data"], created);
    assert_eq!(
        bearer_get(&f, "/api/v1/oauth_apps/lookup/google")
            .await
            .json()["data"],
        created
    );
    assert_eq!(
        bearer_get(&f, "/api/v1/oauth_apps/lookup/nope")
            .await
            .status(),
        404
    );
    assert_eq!(
        bearer_get(&f, "/api/v1/oauth_apps/oap_nope").await.status(),
        404
    );
    assert_eq!(
        bearer_get(&f, "/api/v1/oauth_apps/google").await.status(),
        404
    );

    let list = bearer_get(&f, "/api/v1/oauth_apps").await.json();
    assert_eq!(list["data"].as_array().unwrap().len(), 1);
    assert_eq!(
        list["meta"],
        json!({"page": 1, "limit": 50, "total": 1, "total_pages": 1})
    );
    let filtered = bearer_get(&f, "/api/v1/oauth_apps?labels[team]=other")
        .await
        .json();
    assert_eq!(filtered["meta"]["total"], 0);
    let filtered = bearer_get(&f, "/api/v1/oauth_apps?labels[team]=platform")
        .await
        .json();
    assert_eq!(filtered["meta"]["total"], 1);
}

#[tokio::test]
async fn the_console_view_adds_the_urls_and_the_secret_flag() {
    let Some(f) = fixture().await else { return };
    let response = console(
        &f,
        Method::POST,
        "/api/v1/console/oauth_apps",
        Some(&app_body()),
    )
    .await;
    assert_eq!(response.status(), 201, "{}", response.text());
    let app = response.json()["data"].clone();
    assert_eq!(app["client_secret_set"], true);
    assert_eq!(
        app["redirect_uri"],
        "http://www.example.com/oauth/google/callback"
    );
    assert_eq!(
        app["start_url"],
        "http://www.example.com/oauth/google/start"
    );
    assert_eq!(app["credentials_count"], 0);
    assert_no(&response.text(), &["CLIENT-SECRET-VALUE"]);

    let id = app["id"].as_str().unwrap();
    f.insert_credential_for("google").await;
    let list = console(&f, Method::GET, "/api/v1/console/oauth_apps", None)
        .await
        .json();
    assert_eq!(list["data"][0]["credentials_count"], 1);
    let shown = console(
        &f,
        Method::GET,
        &format!("/api/v1/console/oauth_apps/{id}"),
        None,
    )
    .await
    .json();
    assert_eq!(shown["data"]["credentials_count"], 1);
}

#[tokio::test]
async fn validation_failures_name_the_fields() {
    let Some(f) = fixture().await else { return };
    let response = bearer_post(&f, "/api/v1/oauth_apps", &json!({"data": {}})).await;
    assert_eq!(response.status(), 422);
    let details = &response.json()["error"]["details"];
    for field in [
        "slug",
        "provider",
        "client_id",
        "client_secret",
        "allowed_scopes",
    ] {
        assert!(details[field].is_array(), "{field}: {details}");
    }
    for (patch, field) in [
        (json!({"slug": "oap_reserved"}), "slug"),
        (json!({"slug": "has space"}), "slug"),
        (json!({"provider": "nope"}), "provider"),
        (json!({"allowed_scopes": []}), "allowed_scopes"),
        (json!({"shared": true, "always_available": true}), "shared"),
        (
            json!({"provider_config": {"resource": "https://x.example"}}),
            "provider_config",
        ),
        (
            json!({"provider": "custom", "provider_config": {}}),
            "provider_config",
        ),
    ] {
        let mut body = app_body();
        body["data"]
            .as_object_mut()
            .unwrap()
            .extend(patch.as_object().unwrap().clone());
        let response = bearer_post(&f, "/api/v1/oauth_apps", &body).await;
        assert_eq!(response.status(), 422, "{field}: {}", response.text());
        assert!(
            response.json()["error"]["details"][field].is_array(),
            "{field}"
        );
    }
    assert_eq!(f.count("oauth_apps").await, 0);

    // A body that is not an object with `data` is a 400, not a 500.
    let key = f.admin_key().await;
    let response = f
        .app
        .post("/api/v1/oauth_apps")
        .bearer(&key)
        .json(&json!({"slug": "x"}))
        .send()
        .await;
    assert_eq!(response.status(), 400);
}

#[tokio::test]
async fn slugs_are_unique() {
    let Some(f) = fixture().await else { return };
    assert_eq!(
        bearer_post(&f, "/api/v1/oauth_apps", &app_body())
            .await
            .status(),
        201
    );
    let response = bearer_post(&f, "/api/v1/oauth_apps", &app_body()).await;
    assert_eq!(response.status(), 422);
    assert_eq!(
        response.json()["error"]["details"]["slug"],
        json!(["has already been taken"])
    );
}

#[tokio::test]
async fn a_custom_app_validates_its_provider_config() {
    let Some(f) = fixture().await else { return };
    let mut body = app_body();
    body["data"]["slug"] = json!("acme");
    body["data"]["provider"] = json!("custom");
    body["data"]["provider_config"] = json!({
        "authorization_endpoint": "https://idp.example/authorize",
        "token_endpoint": "http://idp.example/token",
        "resource": "https://api.example/mcp"
    });
    let response = bearer_post(&f, "/api/v1/oauth_apps", &body).await;
    assert_eq!(response.status(), 422);
    assert_eq!(
        response.json()["error"]["details"]["provider_config"],
        json!(["token_endpoint must be an https URL"])
    );
    body["data"]["provider_config"]["token_endpoint"] = json!("https://idp.example/token");
    assert_eq!(
        bearer_post(&f, "/api/v1/oauth_apps", &body).await.status(),
        201
    );
}

#[tokio::test]
async fn updates_by_oid_or_slug_and_writes_only_what_changed() {
    let Some(f) = fixture().await else { return };
    let created = bearer_post(&f, "/api/v1/oauth_apps", &app_body())
        .await
        .json()["data"]
        .clone();
    let id = created["id"].as_str().unwrap();
    let stored_secret = async || -> String {
        sqlx::query_scalar("SELECT client_secret FROM oauth_apps WHERE slug = 'google'")
            .fetch_one(&f.app.db.pool)
            .await
            .unwrap()
    };
    let secret_before = stored_secret().await;

    // An empty update changes nothing, not even `updated_at`.
    f.app
        .clock
        .set(centaur_console_api::testing::at(1_900_000_000));
    let response = bearer_patch(
        &f,
        &format!("/api/v1/oauth_apps/{id}"),
        &json!({"data": {"client_secret": "", "slug": ""}}),
    )
    .await;
    assert_eq!(response.status(), 200, "{}", response.text());
    assert_eq!(response.json()["data"]["updated_at"], created["updated_at"]);
    assert_eq!(stored_secret().await, secret_before);

    let response = bearer_patch(
        &f,
        "/api/v1/oauth_apps/google",
        &json!({"data": {"description": null, "enabled": false}}),
    )
    .await;
    assert_eq!(response.status(), 200);
    let updated = response.json()["data"].clone();
    assert_eq!(updated["description"], Value::Null);
    assert_eq!(updated["enabled"], false);
    assert_ne!(updated["updated_at"], created["updated_at"]);
    assert_eq!(updated["created_at"], created["created_at"]);
    assert_eq!(stored_secret().await, secret_before);

    let response = bearer_patch(
        &f,
        &format!("/api/v1/oauth_apps/{id}"),
        &json!({"data": {"client_secret": "NEW-SECRET"}}),
    )
    .await;
    assert_eq!(response.status(), 200);
    assert_ne!(stored_secret().await, secret_before);

    // `shared` is fixed at creation.
    let response = bearer_patch(
        &f,
        &format!("/api/v1/oauth_apps/{id}"),
        &json!({"data": {"shared": true}}),
    )
    .await;
    assert_eq!(response.status(), 422);
    assert!(response.json()["error"]["details"]["shared"].is_array());

    // An unknown slug is created; an unknown oid is not.
    let mut body = app_body();
    body["data"].as_object_mut().unwrap().remove("slug");
    let response = bearer_patch(&f, "/api/v1/oauth_apps/linear-team", &body).await;
    assert_eq!(response.status(), 201, "{}", response.text());
    assert_eq!(response.json()["data"]["slug"], "linear-team");
    let response = bearer_patch(&f, "/api/v1/oauth_apps/oap_unknown", &body).await;
    assert_eq!(response.status(), 404);
}

#[tokio::test]
async fn deleting_is_refused_while_credentials_exist() {
    let Some(f) = fixture().await else { return };
    let created = bearer_post(&f, "/api/v1/oauth_apps", &app_body())
        .await
        .json()["data"]
        .clone();
    let id = created["id"].as_str().unwrap();
    f.insert_credential_for("google").await;
    let key = f.admin_key().await;
    let response = f
        .app
        .delete(&format!("/api/v1/oauth_apps/{id}"))
        .bearer(&key)
        .send()
        .await;
    assert_eq!(response.status(), 409);
    assert_eq!(
        response.error_message().unwrap(),
        "Cannot delete record because dependent broker credentials exist"
    );
    sqlx::query("DELETE FROM broker_credentials")
        .execute(&f.app.db.pool)
        .await
        .unwrap();
    let response = f
        .app
        .delete(&format!("/api/v1/oauth_apps/{id}"))
        .bearer(&key)
        .send()
        .await;
    assert_eq!(response.status(), 204);
    assert_eq!(
        f.app
            .delete(&format!("/api/v1/oauth_apps/{id}"))
            .bearer(&key)
            .send()
            .await
            .status(),
        404
    );
}

#[tokio::test]
async fn lists_an_apps_minted_credentials_without_token_material() {
    let Some(f) = fixture().await else { return };
    let created = bearer_post(&f, "/api/v1/oauth_apps", &app_body())
        .await
        .json()["data"]
        .clone();
    let id = created["id"].as_str().unwrap();
    f.insert_credential_for("google").await;
    let response = console(
        &f,
        Method::GET,
        &format!("/api/v1/console/oauth_apps/{id}/credentials"),
        None,
    )
    .await;
    assert_eq!(response.status(), 200);
    let body = response.json();
    let row = &body["data"][0];
    assert!(row["id"].as_str().unwrap().starts_with("bcr_"));
    assert_eq!(row["status"], "bootstrapping");
    assert_eq!(row["provider_email"], "someone@example.test");
    assert_eq!(body["meta"]["total"], 1);
    assert_no(&response.text(), &["access", "refresh_token"]);
    let missing = console(
        &f,
        Method::GET,
        "/api/v1/console/oauth_apps/oap_nope/credentials",
        None,
    )
    .await;
    assert_eq!(missing.status(), 404);
}

// --- providers, presets, details -------------------------------------------------------

#[tokio::test]
async fn lists_the_registered_providers() {
    let Some(f) = fixture().await else { return };
    let response = console(&f, Method::GET, "/api/v1/console/oauth_providers", None).await;
    let providers = response.json()["data"].clone();
    let keys: Vec<&str> = providers
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["key"].as_str().unwrap())
        .collect();
    assert_eq!(
        keys,
        [
            "attio",
            "custom",
            "github",
            "google",
            "granola",
            "linear",
            "mercator",
            "microsoft",
            "slack",
            "zoom"
        ]
    );
    let custom = &providers[1];
    assert_eq!(
        custom["config_keys"],
        json!([
            "authorization_endpoint",
            "token_endpoint",
            "userinfo_endpoint",
            "resource"
        ])
    );
    assert_eq!(custom["optional_config_keys"], json!(["userinfo_endpoint"]));
    assert_eq!(providers[0]["config_keys"], json!([]));
    let presettable: Vec<&str> = providers
        .as_array()
        .unwrap()
        .iter()
        .filter(|p| p["presettable"] == true)
        .map(|p| p["key"].as_str().unwrap())
        .collect();
    assert_eq!(presettable, ["mercator"]);
}

#[tokio::test]
async fn sets_up_the_mercator_preset_once() {
    let Some(f) = fixture().await else { return };
    f.stub.answer(
        "mercator.sh/oauth/register",
        201,
        &json!({"client_id": "reg-id", "client_secret": "REG-SECRET-VALUE"}),
    );
    let response = console(
        &f,
        Method::POST,
        "/api/v1/console/oauth_apps/presets/mercator",
        None,
    )
    .await;
    assert_eq!(response.status(), 200, "{}", response.text());
    let data = response.json()["data"].clone();
    assert_eq!(data["slug"], "mercator");
    assert_eq!(
        data["start_url"],
        "http://www.example.com/oauth/mercator/start"
    );
    assert_no(&response.text(), &["REG-SECRET-VALUE"]);
    let registered: Value =
        serde_json::from_str(&f.stub.requests_to("mercator.sh/oauth/register")[0].body).unwrap();
    assert_eq!(
        registered["redirect_uris"],
        json!(["http://www.example.com/oauth/mercator/callback"])
    );
    assert_eq!(registered["scope"], "mercator:tools");

    let (shared, client_id): (bool, String) =
        sqlx::query_as("SELECT shared, client_id FROM oauth_apps WHERE slug = 'mercator'")
            .fetch_one(&f.app.db.pool)
            .await
            .unwrap();
    assert_eq!((shared, client_id.as_str()), (true, "reg-id"));

    // Asking again reuses the app without registering another client.
    let again = console(
        &f,
        Method::POST,
        "/api/v1/console/oauth_apps/presets/mercator",
        None,
    )
    .await;
    assert_eq!(again.status(), 200);
    assert_eq!(again.json()["data"]["id"], data["id"]);
    assert_eq!(f.stub.requests_to("mercator.sh/oauth/register").len(), 1);

    // Presets are unknown for other providers.
    let response = console(
        &f,
        Method::POST,
        "/api/v1/console/oauth_apps/presets/google",
        None,
    )
    .await;
    assert_eq!(response.status(), 404);
}

#[tokio::test]
async fn a_failed_registration_creates_nothing() {
    let Some(f) = fixture().await else { return };
    f.stub.answer(
        "mercator.sh/oauth/register",
        200,
        &json!({"client_id": "id", "client_secret": " "}),
    );
    let response = console(
        &f,
        Method::POST,
        "/api/v1/console/oauth_apps/presets/mercator",
        None,
    )
    .await;
    assert_eq!(response.status(), 502);
    assert_eq!(f.count("oauth_apps").await, 0);
}

#[tokio::test]
async fn a_conflicting_app_blocks_the_preset() {
    let Some(f) = fixture().await else { return };
    f.insert_app("mercator", "google", false).await;
    let response = console(
        &f,
        Method::POST,
        "/api/v1/console/oauth_apps/presets/mercator",
        None,
    )
    .await;
    assert_eq!(response.status(), 409);
    assert!(f.stub.requests_to("mercator.sh/oauth/register").is_empty());
}

#[tokio::test]
async fn provider_details_ask_mercator_about_the_shared_wallet() {
    let Some(f) = fixture().await else { return };
    let wallet = "0xabcdef0123456789abcdef0123456789abcdef01";
    let app_id = f.insert_app("mercator", "mercator", true).await;
    let created = f.app.now().naive_utc();
    let token = centaur_console_db::Encryptor::development()
        .encrypt("ACCESS-TOKEN-VALUE")
        .unwrap();
    let credential_id: i64 = sqlx::query_scalar(
        "INSERT INTO broker_credentials (oauth_app_id, provider_subject, token_endpoint, access_token, \
         created_at, updated_at) VALUES ($1, $2, 'https://mercator.sh/oauth/token', $3, $4, $4) RETURNING id",
    )
    .bind(app_id)
    .bind(wallet)
    .bind(token)
    .bind(created)
    .fetch_one(&f.app.db.pool)
    .await
    .unwrap();
    f.stub.answer(
        "mercator.sh/v1/claims/config",
        200,
        &json!({"wallet_address": wallet.to_uppercase().replacen("0X", "0x", 1), "claimed": true}),
    );
    let path = format!(
        "/api/v1/console/oauth_apps/{}/provider_details",
        oid("oap", app_id)
    );
    let response = console(&f, Method::GET, &path, None).await;
    assert_eq!(response.status(), 200, "{}", response.text());
    let data = response.json()["data"].clone();
    assert_eq!(data["provider"], "mercator");
    assert_eq!(data["details"], json!({"claimed": true}));
    assert_eq!(data["credential"]["id"], oid("bcr", credential_id));
    assert_eq!(data["credential"]["provider_subject"], wallet);
    assert_eq!(
        f.stub.requests_to("mercator.sh/v1/claims/config")[0]
            .authorization
            .as_deref(),
        Some("Bearer ACCESS-TOKEN-VALUE")
    );

    // A different wallet is "unknown"; a dead credential asks nothing.
    f.stub.answer(
        "mercator.sh/v1/claims/config",
        200,
        &json!({"wallet_address": "0xother", "claimed": true}),
    );
    assert_eq!(
        console(&f, Method::GET, &path, None).await.json()["data"]["details"],
        json!({"claimed": null})
    );
    sqlx::query("UPDATE broker_credentials SET dead = TRUE")
        .execute(&f.app.db.pool)
        .await
        .unwrap();
    let calls = f.stub.requests_to("mercator.sh/v1/claims/config").len();
    assert_eq!(
        console(&f, Method::GET, &path, None).await.json()["data"]["details"],
        json!({})
    );
    assert_eq!(
        f.stub.requests_to("mercator.sh/v1/claims/config").len(),
        calls
    );

    // A personal app has no details.
    let personal = f.insert_app("personal", "google", false).await;
    let response = console(
        &f,
        Method::GET,
        &format!(
            "/api/v1/console/oauth_apps/{}/provider_details",
            oid("oap", personal)
        ),
        None,
    )
    .await;
    assert_eq!(
        response.json()["data"],
        json!({"provider": "google", "credential": null, "details": {}})
    );
}

// --- integrations -------------------------------------------------------------------------

#[tokio::test]
async fn integrations_show_each_user_their_own_connections() {
    let Some(f) = fixture().await else { return };
    let personal = f.insert_app("slack", "slack", false).await;
    let shared = f.insert_app("mercator-shared", "mercator", true).await;
    let disabled = f.insert_app("off", "google", false).await;
    sqlx::query("UPDATE oauth_apps SET enabled = FALSE WHERE id = $1")
        .bind(disabled)
        .execute(&f.app.db.pool)
        .await
        .unwrap();
    let now = f.app.now().naive_utc();
    // The member connected Slack, but without the scope the provider needs.
    sqlx::query(
        "INSERT INTO broker_credentials (oauth_app_id, provider_subject, token_endpoint, scopes, created_by_id, \
         created_at, updated_at) VALUES ($1, 'U1', 'https://slack.com/api/oauth.v2.access', '[\"im:history\"]', $2, $3, $3)",
    )
    .bind(personal).bind(f.member.id).bind(now)
    .execute(&f.app.db.pool).await.unwrap();

    let mut member = f.browser(&f.member);
    let response = member.send(f.app.get("/api/v1/console/integrations")).await;
    assert_eq!(response.status(), 200);
    let data = response.json()["data"].clone();
    let slugs: Vec<&str> = data["apps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["slug"].as_str().unwrap())
        .collect();
    assert_eq!(slugs, ["slack"], "members see personal, enabled apps only");
    assert_eq!(
        data["apps"][0]["connection"],
        json!({"status": "needs_reconnect"})
    );
    assert_eq!(data["apps"][0]["usage"], "direct_messages");
    assert_eq!(
        data["apps"][0]["start_url"],
        "http://www.example.com/oauth/slack/start"
    );
    assert_eq!(data["presets"], json!([]));
    assert_eq!(data["ssh_key"], json!({"configured": false}));

    let mut admin = f.browser(&f.admin);
    let data = admin
        .send(f.app.get("/api/v1/console/integrations"))
        .await
        .json()["data"]
        .clone();
    let slugs: Vec<&str> = data["apps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["slug"].as_str().unwrap())
        .collect();
    assert_eq!(slugs, ["mercator-shared", "slack"]);
    assert_eq!(data["apps"][0]["usage"], "shared");
    assert_eq!(
        data["apps"][1]["connection"],
        Value::Null,
        "the admin has no credential of their own"
    );
    assert_eq!(
        data["presets"],
        json!([]),
        "the mercator app exists, so there is nothing to preset"
    );
    let _ = shared;
}

#[tokio::test]
async fn admins_are_offered_the_presets_that_are_not_set_up() {
    let Some(f) = fixture().await else { return };
    let mut admin = f.browser(&f.admin);
    let data = admin
        .send(f.app.get("/api/v1/console/integrations"))
        .await
        .json()["data"]
        .clone();
    assert_eq!(data["presets"][0]["provider"], "mercator");
    assert_eq!(data["presets"][0]["display_name"], "Mercator");
}
