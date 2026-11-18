//! `/broker_credentials` (bearer) and the console's view of them.

use centaur_console_api::{auth::Access, testing::TestResponse};
use centaur_console_db::oid;
use http::Method;
use serde_json::{Value, json};

use super::support::*;

fn body() -> Value {
    json!({"data": {
        "foreign_id": "svc-token", "name": "Service token", "grant": "refresh_token",
        "token_endpoint": "https://idp.example/token", "client_id": "cid", "client_secret": "CLIENT-SECRET-VALUE",
        "refresh_token": "REFRESH-TOKEN-VALUE", "scopes": ["read"], "labels": {"team": "data"},
        "token_endpoint_headers": {"X-Api-Key": "HEADER-VALUE"}
    }})
}

async fn bearer(f: &Fixture, method: Method, path: &str, body: Option<&Value>) -> TestResponse {
    let key = f.admin_key().await;
    let mut request = f.app.request(method, path).bearer(&key);
    if let Some(body) = body {
        request = request.json(body);
    }
    request.send().await
}

async fn console(f: &Fixture, method: Method, path: &str, body: Option<&Value>) -> TestResponse {
    let mut browser = f.browser(&f.admin);
    let mut request = f.app.request(method, path);
    if let Some(body) = body {
        request = request.json(body);
    }
    let request = request.csrf(&browser);
    browser.send(request).await
}

const SECRETS: [&str; 3] = ["CLIENT-SECRET-VALUE", "REFRESH-TOKEN-VALUE", "HEADER-VALUE"];

#[tokio::test]
async fn routes_are_admin_only() {
    let Some(f) = fixture().await else { return };
    let app = &f.app;
    let empty = json!({"data": {}});
    for (method, path, body) in [
        (Method::GET, "/api/v1/broker_credentials", None),
        (Method::POST, "/api/v1/broker_credentials", Some(&empty)),
        (Method::GET, "/api/v1/broker_credentials/lookup/x", None),
        (
            Method::GET,
            "/api/v1/broker_credentials/lookup/default/x",
            None,
        ),
        (Method::GET, "/api/v1/broker_credentials/bcr_x", None),
        (Method::PATCH, "/api/v1/broker_credentials/x", Some(&empty)),
        (Method::DELETE, "/api/v1/broker_credentials/bcr_x", None),
    ] {
        app.assert_bearer_access(method, path, body).await;
    }
    let empty = json!({});
    for (method, path, body) in [
        (Method::GET, "/api/v1/console/broker_credentials", None),
        (
            Method::POST,
            "/api/v1/console/broker_credentials",
            Some(&empty),
        ),
        (
            Method::GET,
            "/api/v1/console/broker_credentials/bcr_x",
            None,
        ),
        (
            Method::PATCH,
            "/api/v1/console/broker_credentials/bcr_x",
            Some(&empty),
        ),
        (
            Method::DELETE,
            "/api/v1/console/broker_credentials/bcr_x",
            None,
        ),
    ] {
        app.assert_console_access(Access::Admin, method, path, body)
            .await;
    }
}

#[tokio::test]
async fn creates_a_credential_and_never_returns_its_secrets() {
    let Some(f) = fixture().await else { return };
    let response = bearer(
        &f,
        Method::POST,
        "/api/v1/broker_credentials",
        Some(&body()),
    )
    .await;
    assert_eq!(response.status(), 201, "{}", response.text());
    assert_no(&response.text(), &SECRETS);
    let data = response.json()["data"].clone();
    assert!(data["id"].as_str().unwrap().starts_with("bcr_"));
    assert_eq!(data["foreign_id"], "svc-token");
    assert_eq!(data["grant"], "refresh_token");
    assert_eq!(data["scopes"], json!(["read"]));
    assert_eq!(data["status"], "bootstrapping");
    assert_eq!(data["early_refresh_slack_seconds"], 300);
    assert_eq!(data["early_refresh_fraction"], 0.2);
    assert_eq!(data["max_refresh_interval_seconds"], 86_400);
    assert_eq!(data["refresh_timeout_seconds"], 30);
    assert_eq!(data["token_endpoint_header_names"], json!(["X-Api-Key"]));
    assert_eq!(data["dead"], false);
    assert_eq!(data["oauth_app_id"], Value::Null);
    assert!(
        data.get("has_client_secret").is_none(),
        "presence flags are console-only"
    );

    // Stored encrypted; the lookups find it by foreign id.
    let stored: (String, String) =
        sqlx::query_as("SELECT client_secret, token_endpoint_headers FROM broker_credentials")
            .fetch_one(&f.app.db.pool)
            .await
            .unwrap();
    assert_no(&format!("{stored:?}"), &SECRETS);
    for path in ["lookup/svc-token", "lookup/default/svc-token"] {
        let found = bearer(
            &f,
            Method::GET,
            &format!("/api/v1/broker_credentials/{path}"),
            None,
        )
        .await;
        assert_eq!(found.json()["data"]["id"], data["id"], "{path}");
    }
    assert_eq!(
        bearer(
            &f,
            Method::GET,
            "/api/v1/broker_credentials/lookup/nope",
            None
        )
        .await
        .status(),
        404
    );
    assert_eq!(
        bearer(&f, Method::GET, "/api/v1/broker_credentials/bcr_nope", None)
            .await
            .status(),
        404
    );
    let list = bearer(
        &f,
        Method::GET,
        "/api/v1/broker_credentials?labels[team]=data",
        None,
    )
    .await
    .json();
    assert_eq!(list["meta"]["total"], 1);
    assert_eq!(
        bearer(
            &f,
            Method::GET,
            "/api/v1/broker_credentials?labels[team]=x",
            None
        )
        .await
        .json()["meta"]["total"],
        0
    );
}

#[tokio::test]
async fn the_console_view_reports_what_is_set_without_revealing_it() {
    let Some(f) = fixture().await else { return };
    let response = console(
        &f,
        Method::POST,
        "/api/v1/console/broker_credentials",
        Some(&body()),
    )
    .await;
    assert_eq!(response.status(), 201, "{}", response.text());
    assert_no(&response.text(), &SECRETS);
    let data = response.json()["data"].clone();
    for (flag, expected) in [
        ("has_client_secret", true),
        ("has_refresh_token", true),
        ("has_username", false),
        ("has_password", false),
        ("has_api_key", false),
    ] {
        assert_eq!(data[flag], expected, "{flag}");
    }
    assert_eq!(data["effective_client_id"], "cid");
    assert_eq!(data["oauth_app"], Value::Null);
    assert_eq!(data["static_secret"], Value::Null);
    let id = data["id"].as_str().unwrap();
    let shown = console(
        &f,
        Method::GET,
        &format!("/api/v1/console/broker_credentials/{id}"),
        None,
    )
    .await;
    assert_eq!(shown.json()["data"], data);
}

#[tokio::test]
async fn validation_is_per_field() {
    let Some(f) = fixture().await else { return };
    let response = bearer(
        &f,
        Method::POST,
        "/api/v1/broker_credentials",
        Some(&json!({"data": {}})),
    )
    .await;
    assert_eq!(response.status(), 422);
    let details = response.json()["error"]["details"].clone();
    assert!(
        details["token_endpoint"].is_array() && details["client_id"].is_array(),
        "{details}"
    );
    for (patch, field) in [
        (json!({"grant": "implicit"}), "grant"),
        (json!({"foreign_id": "bcr_reserved"}), "foreign_id"),
        (json!({"foreign_id": "not url safe"}), "foreign_id"),
        (
            json!({"early_refresh_fraction": "1"}),
            "early_refresh_fraction",
        ),
        (
            json!({"early_refresh_fraction": -0.5}),
            "early_refresh_fraction",
        ),
        (
            json!({"early_refresh_slack_seconds": 0}),
            "early_refresh_slack_seconds",
        ),
        (
            json!({"max_refresh_interval_seconds": "1.5"}),
            "max_refresh_interval_seconds",
        ),
        (
            json!({"refresh_timeout_seconds": "soon"}),
            "refresh_timeout_seconds",
        ),
        (
            json!({"token_endpoint_headers": {"X": 1}}),
            "token_endpoint_headers",
        ),
        (
            json!({"grant": "client_credentials", "client_secret": ""}),
            "client_secret",
        ),
        (json!({"grant": "password"}), "username"),
        (json!({"grant": "preqin"}), "api_key"),
    ] {
        let mut request = body();
        request["data"]
            .as_object_mut()
            .unwrap()
            .extend(patch.as_object().unwrap().clone());
        if patch.get("client_secret").is_some() {
            request["data"]
                .as_object_mut()
                .unwrap()
                .remove("client_secret");
        }
        let response = bearer(
            &f,
            Method::POST,
            "/api/v1/broker_credentials",
            Some(&request),
        )
        .await;
        assert_eq!(response.status(), 422, "{field}: {}", response.text());
        assert!(
            response.json()["error"]["details"][field].is_array(),
            "{field}: {}",
            response.text()
        );
    }
    assert_eq!(f.count("broker_credentials").await, 0);
}

#[tokio::test]
async fn numbers_may_be_json_numbers_or_text() {
    let Some(f) = fixture().await else { return };
    let mut request = body();
    request["data"]["early_refresh_slack_seconds"] = json!("120");
    request["data"]["early_refresh_fraction"] = json!("0.5");
    request["data"]["max_refresh_interval_seconds"] = json!(7200);
    request["data"]["refresh_timeout_seconds"] = json!(15.0);
    let response = bearer(
        &f,
        Method::POST,
        "/api/v1/broker_credentials",
        Some(&request),
    )
    .await;
    assert_eq!(response.status(), 201, "{}", response.text());
    let data = response.json()["data"].clone();
    assert_eq!(
        [
            data["early_refresh_slack_seconds"].clone(),
            data["early_refresh_fraction"].clone(),
            data["max_refresh_interval_seconds"].clone(),
            data["refresh_timeout_seconds"].clone()
        ],
        [json!(120), json!(0.5), json!(7200), json!(15)]
    );
}

#[tokio::test]
async fn preqin_credentials_use_the_preqin_endpoint() {
    let Some(f) = fixture().await else { return };
    let request = json!({"data": {"foreign_id": "preqin", "grant": "preqin", "token_endpoint": "https://elsewhere.example", "username": "u", "api_key": "k"}});
    let response = bearer(
        &f,
        Method::POST,
        "/api/v1/broker_credentials",
        Some(&request),
    )
    .await;
    assert_eq!(response.status(), 201, "{}", response.text());
    assert_eq!(
        response.json()["data"]["token_endpoint"],
        "https://api.preqin.com/connect/token"
    );
}

#[tokio::test]
async fn updating_by_foreign_id_creates_then_updates() {
    let Some(f) = fixture().await else { return };
    let response = bearer(
        &f,
        Method::PATCH,
        "/api/v1/broker_credentials/svc-token",
        Some(&body()),
    )
    .await;
    assert_eq!(response.status(), 201, "{}", response.text());
    let id = response.json()["data"]["id"].clone();
    let response = bearer(
        &f,
        Method::PATCH,
        "/api/v1/broker_credentials/svc-token",
        Some(&json!({"data": {"name": "Renamed", "foreign_id": ""}})),
    )
    .await;
    assert_eq!(response.status(), 200, "{}", response.text());
    assert_eq!(response.json()["data"]["id"], id);
    assert_eq!(response.json()["data"]["name"], "Renamed");
    assert_eq!(
        response.json()["data"]["foreign_id"],
        "svc-token",
        "a blank foreign id never clears it"
    );
    // An opaque id must exist, and the foreign id cannot change once set.
    assert_eq!(
        bearer(
            &f,
            Method::PATCH,
            "/api/v1/broker_credentials/bcr_unknown",
            Some(&body())
        )
        .await
        .status(),
        404
    );
    let path = format!("/api/v1/broker_credentials/{}", id.as_str().unwrap());
    let response = bearer(
        &f,
        Method::PATCH,
        &path,
        Some(&json!({"data": {"foreign_id": "other"}})),
    )
    .await;
    assert_eq!(response.status(), 422);
    assert!(response.json()["error"]["details"]["foreign_id"].is_array());
    // Foreign ids are unique.
    bearer(
        &f,
        Method::PATCH,
        "/api/v1/broker_credentials/second",
        Some(&body()),
    )
    .await;
    let response = bearer(
        &f,
        Method::PATCH,
        "/api/v1/broker_credentials/second",
        Some(&json!({"data": {}})),
    )
    .await;
    assert_eq!(response.status(), 200);
}

#[tokio::test]
async fn a_duplicate_foreign_id_is_a_validation_error() {
    let Some(f) = fixture().await else { return };
    assert_eq!(
        bearer(
            &f,
            Method::POST,
            "/api/v1/broker_credentials",
            Some(&body())
        )
        .await
        .status(),
        201
    );
    let response = bearer(
        &f,
        Method::POST,
        "/api/v1/broker_credentials",
        Some(&body()),
    )
    .await;
    assert_eq!(response.status(), 422);
    assert_eq!(
        response.json()["error"]["details"]["foreign_id"],
        json!(["has already been taken"])
    );
}

/// Regression (review finding 66a99e): an update writes only the columns it
/// changes, so token state a refresher wrote since is not restored.
#[tokio::test]
async fn an_update_does_not_restore_stale_token_state() {
    let Some(f) = fixture().await else { return };
    let created = bearer(
        &f,
        Method::POST,
        "/api/v1/broker_credentials",
        Some(&body()),
    )
    .await
    .json()["data"]
        .clone();
    let id = created["id"].as_str().unwrap();
    // token-broker refreshes the credential after the console read it.
    let fresh_access = centaur_console_db::Encryptor::development()
        .encrypt("FRESH-ACCESS")
        .unwrap();
    sqlx::query(
        "UPDATE broker_credentials SET access_token = $1, dead = TRUE, dead_reason = 'invalid_grant', \
         failure_count = 4, last_refresh = now(), expires_at = now() + interval '1 hour'",
    )
    .bind(&fresh_access)
    .execute(&f.app.db.pool)
    .await
    .unwrap();
    let response = console(
        &f,
        Method::PATCH,
        &format!("/api/v1/console/broker_credentials/{id}"),
        Some(&json!({"data": {"name": "Renamed"}})),
    )
    .await;
    assert_eq!(response.status(), 200, "{}", response.text());
    let row: (String, bool, Option<String>, i32, String) = sqlx::query_as(
        "SELECT access_token, dead, dead_reason, failure_count, name FROM broker_credentials",
    )
    .fetch_one(&f.app.db.pool)
    .await
    .unwrap();
    assert_eq!(row.0, fresh_access.as_stored());
    assert_eq!(
        (row.1, row.2.as_deref(), row.3, row.4.as_str()),
        (true, Some("invalid_grant"), 4, "Renamed")
    );
}

#[tokio::test]
async fn new_credential_material_revives_a_dead_credential_and_invalidates_grantees() {
    let Some(f) = fixture().await else { return };
    let created = bearer(
        &f,
        Method::POST,
        "/api/v1/broker_credentials",
        Some(&body()),
    )
    .await
    .json()["data"]
        .clone();
    let id = created["id"].as_str().unwrap().to_owned();
    let credential_id = centaur_console_db::decode_oid("bcr", &id).unwrap();
    let (secret_id,): (i64,) = sqlx::query_as(
        "INSERT INTO static_secrets (name, kind, labels, broker_credential_id, created_at, updated_at) \
         VALUES ('wrapper', 'custom', '{}', $1, now(), now()) RETURNING id",
    )
    .bind(credential_id)
    .fetch_one(&f.app.db.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO secret_sources (source_type, broker_credential_id, static_secret_id, config, created_at, updated_at) \
         VALUES ('token_broker', $1, $2, '{}', now(), now())",
    )
    .bind(credential_id)
    .bind(secret_id)
    .execute(&f.app.db.pool)
    .await
    .unwrap();
    let principal = grant(&f, secret_id).await;
    sqlx::query("UPDATE broker_credentials SET dead = TRUE, dead_reason = 'invalid_grant', failure_count = 3")
        .execute(&f.app.db.pool)
        .await
        .unwrap();
    let before = f.version(principal).await;

    // Editing metadata leaves it dead; a new refresh token revives it.
    let path = format!("/api/v1/broker_credentials/{id}");
    bearer(
        &f,
        Method::PATCH,
        &path,
        Some(&json!({"data": {"description": "x"}})),
    )
    .await;
    assert_eq!(f.version(principal).await, before);
    let response = bearer(
        &f,
        Method::PATCH,
        &path,
        Some(&json!({"data": {"refresh_token": "NEW-REFRESH"}})),
    )
    .await;
    let data = response.json()["data"].clone();
    assert_eq!(
        (
            data["dead"].clone(),
            data["dead_reason"].clone(),
            data["failure_count"].clone()
        ),
        (json!(false), Value::Null, json!(0))
    );
    assert!(data["next_attempt_at"].is_string(), "due now");
    assert_eq!(f.version(principal).await, before + 1);
    assert_no(&response.text(), &["NEW-REFRESH"]);
}

#[tokio::test]
async fn the_console_keeps_a_stored_header_whose_value_is_null() {
    let Some(f) = fixture().await else { return };
    let created = console(
        &f,
        Method::POST,
        "/api/v1/console/broker_credentials",
        Some(&body()),
    )
    .await
    .json()["data"]
        .clone();
    let path = format!(
        "/api/v1/console/broker_credentials/{}",
        created["id"].as_str().unwrap()
    );
    let update =
        json!({"data": {"token_endpoint_headers": {"X-Api-Key": null, "X-Other": "OTHER-VALUE"}}});
    let response = console(&f, Method::PATCH, &path, Some(&update)).await;
    assert_eq!(response.status(), 200, "{}", response.text());
    assert_eq!(
        response.json()["data"]["token_endpoint_header_names"],
        json!(["X-Api-Key", "X-Other"])
    );
    let (cipher,): (String,) =
        sqlx::query_as("SELECT token_endpoint_headers FROM broker_credentials")
            .fetch_one(&f.app.db.pool)
            .await
            .unwrap();
    let headers: Value = serde_json::from_str(
        &centaur_console_db::Encryptor::development()
            .decrypt(&centaur_console_db::Ciphertext::from_stored(cipher))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        headers,
        json!({"X-Api-Key": "HEADER-VALUE", "X-Other": "OTHER-VALUE"})
    );
    // `null` clears all headers.
    let response = console(
        &f,
        Method::PATCH,
        &path,
        Some(&json!({"data": {"token_endpoint_headers": null}})),
    )
    .await;
    assert_eq!(
        response.json()["data"]["token_endpoint_header_names"],
        json!([])
    );
}

#[tokio::test]
async fn deleting_unlinks_the_wrapper_but_not_while_a_source_references_it() {
    let Some(f) = fixture().await else { return };
    let created = bearer(
        &f,
        Method::POST,
        "/api/v1/broker_credentials",
        Some(&body()),
    )
    .await
    .json()["data"]
        .clone();
    let id = created["id"].as_str().unwrap().to_owned();
    let credential_id = centaur_console_db::decode_oid("bcr", &id).unwrap();
    let secret_id: i64 = sqlx::query_scalar(
        "INSERT INTO static_secrets (name, kind, labels, broker_credential_id, created_at, updated_at) \
         VALUES ('wrapper', 'custom', '{}', $1, now(), now()) RETURNING id",
    )
    .bind(credential_id)
    .fetch_one(&f.app.db.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO secret_sources (source_type, broker_credential_id, static_secret_id, config, created_at, updated_at) \
         VALUES ('token_broker', $1, $2, '{}', now(), now())",
    )
    .bind(credential_id)
    .bind(secret_id)
    .execute(&f.app.db.pool)
    .await
    .unwrap();
    let path = format!("/api/v1/broker_credentials/{id}");
    let response = bearer(&f, Method::DELETE, &path, None).await;
    assert_eq!(response.status(), 409);
    assert!(
        response
            .error_message()
            .unwrap()
            .contains("token_broker secret sources")
    );
    assert_eq!(f.count("broker_credentials").await, 1);

    sqlx::query("DELETE FROM secret_sources")
        .execute(&f.app.db.pool)
        .await
        .unwrap();
    assert_eq!(bearer(&f, Method::DELETE, &path, None).await.status(), 204);
    assert_eq!(f.count("broker_credentials").await, 0);
    let linked: Option<i64> =
        sqlx::query_scalar("SELECT broker_credential_id FROM static_secrets WHERE id = $1")
            .bind(secret_id)
            .fetch_one(&f.app.db.pool)
            .await
            .unwrap();
    assert_eq!(linked, None);
    assert_eq!(bearer(&f, Method::DELETE, &path, None).await.status(), 404);
}

#[tokio::test]
async fn a_credential_minted_by_an_app_shows_its_app_and_wrapper() {
    let Some(f) = fixture().await else { return };
    f.insert_app("google", "google", false).await;
    let credential_id = f.insert_credential_for("google").await;
    let secret_id: i64 = sqlx::query_scalar(
        "INSERT INTO static_secrets (name, kind, labels, broker_credential_id, created_at, updated_at) \
         VALUES ('Google token', 'custom', '{}', $1, now(), now()) RETURNING id",
    )
    .bind(credential_id)
    .fetch_one(&f.app.db.pool)
    .await
    .unwrap();
    let path = format!(
        "/api/v1/console/broker_credentials/{}",
        oid("bcr", credential_id)
    );
    let data = console(&f, Method::GET, &path, None).await.json()["data"].clone();
    assert_eq!(data["oauth_app"]["slug"], "google");
    assert_eq!(data["oauth_app_id"], data["oauth_app"]["id"]);
    assert_eq!(data["effective_client_id"], "client-id");
    assert_eq!(data["static_secret"]["id"], oid("ssr", secret_id));
    assert_eq!(data["static_secret"]["name"], "Google token");
    assert_eq!(data["provider_email"], "someone@example.test");
    // Editing an app-minted credential tells the principals area.
    let response = console(
        &f,
        Method::PATCH,
        &path,
        Some(&json!({"data": {"name": "Mine"}})),
    )
    .await;
    assert_eq!(response.status(), 200, "{}", response.text());
}
