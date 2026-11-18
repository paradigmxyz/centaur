//! Secrets on the simplified base: the bearer API for every kind, the
//! console's list, detail, role grants, and the four kinds it edits, and the
//! sandbox admin API.

use axum::http::Method;
use centaur_console_api::{
    auth::Access,
    secrets,
    testing::{TestApp, TestResponse, TestUser},
};
use centaur_console_db::oid;
use serde_json::{Value, json};

struct Fx {
    app: TestApp,
    admin: TestUser,
    key: String,
}

async fn setup() -> Option<Fx> {
    let app = TestApp::new(secrets::routes()).await?;
    let admin = app.create_user("admin@example.test", true, "active").await;
    let key = app.create_api_key(admin.id).await;
    Some(Fx { app, admin, key })
}

impl Fx {
    async fn call(&self, method: Method, path: &str, body: Option<Value>) -> TestResponse {
        let request = self.app.request(method, path).bearer(&self.key);
        match body {
            Some(body) => request.json(&json!({ "data": body })).send().await,
            None => request.send().await,
        }
    }

    async fn create(&self, resource: &str, body: Value) -> Value {
        let response = self
            .call(Method::POST, &format!("/api/v1/{resource}"), Some(body))
            .await;
        assert_eq!(response.status(), 201, "{}", response.text());
        response.json()["data"].clone()
    }

    async fn version(&self, principal_id: i64) -> i64 {
        sqlx::query_scalar("SELECT sync_config_cache_version FROM principals WHERE id = $1")
            .bind(principal_id)
            .fetch_one(&self.app.db.pool)
            .await
            .unwrap()
    }

    /// A principal that holds the secret directly, and one that holds it
    /// through a role.
    async fn grantees(&self, column: &str, secret_id: i64) -> (i64, i64) {
        let direct = self
            .app
            .create_principal("user", "direct", self.admin.id)
            .await;
        let via_role = self
            .app
            .create_principal("user", "via-role", self.admin.id)
            .await;
        let now = self.app.now().naive_utc();
        let role: i64 = sqlx::query_scalar(
            "INSERT INTO roles (name, created_by_id, created_at, updated_at) \
             VALUES ('r', $1, $2, $2) RETURNING id",
        )
        .bind(self.admin.id)
        .bind(now)
        .fetch_one(&self.app.db.pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO principal_roles (principal_id, role_id, created_at, updated_at) \
             VALUES ($1, $2, $3, $3)",
        )
        .bind(via_role)
        .bind(role)
        .bind(now)
        .execute(&self.app.db.pool)
        .await
        .unwrap();
        for (principal, role) in [(Some(direct), None), (None, Some(role))] {
            sqlx::query(&format!(
                "INSERT INTO grants (principal_id, role_id, {column}, priority, created_by_id, \
                 created_at, updated_at) VALUES ($1, $2, $3, 0, $4, $5, $5)"
            ))
            .bind(principal)
            .bind(role)
            .bind(secret_id)
            .bind(self.admin.id)
            .bind(now)
            .execute(&self.app.db.pool)
            .await
            .unwrap();
        }
        (direct, via_role)
    }

    async fn stored_secret(&self, column: &str, owner: i64) -> Option<String> {
        let stored: Option<String> = sqlx::query_scalar(&format!(
            "SELECT secret FROM secret_sources WHERE {column} = $1"
        ))
        .bind(owner)
        .fetch_optional(&self.app.db.pool)
        .await
        .unwrap()
        .flatten();
        stored.map(|envelope| {
            self.app
                .state
                .encryptor
                .decrypt(&centaur_console_db::Ciphertext::from_stored(envelope))
                .unwrap()
        })
    }
}

fn env_source(var: &str) -> Value {
    json!({"source_type": "env", "config": {"var": var}})
}

fn inline_source(secret: &str) -> Value {
    json!({"source_type": "control_plane", "secret": secret, "config": {}})
}

fn static_body(foreign_id: &str) -> Value {
    json!({
        "foreign_id": foreign_id,
        "name": "Example API",
        "description": "An API key",
        "labels": {"team": "platform"},
        "inject_config": {"header": "X-Api-Key"},
        "source": env_source("EXAMPLE_KEY"),
        "rules": [{"host": "api.example.test", "http_methods": ["GET"], "paths": ["/v1"]}],
    })
}

fn id_of(record: &Value) -> i64 {
    let id = record["id"].as_str().unwrap();
    centaur_console_db::decode_oid(&id[..3], id).unwrap()
}

fn oid_of(kind: &str, id: i64) -> String {
    oid(kind, id)
}

fn error_fields(response: &TestResponse) -> Vec<String> {
    response.json()["error"]["details"]
        .as_object()
        .map(|details| details.keys().cloned().collect())
        .unwrap_or_default()
}

// --- authorization ----------------------------------------------------------

#[tokio::test]
async fn bearer_routes_require_an_admin_api_key() {
    let Some(f) = setup().await else { return };
    let body = json!({"data": {}});
    for (method, path, body) in [
        (Method::GET, "/api/v1/static_secrets", None),
        (Method::POST, "/api/v1/static_secrets", Some(&body)),
        (Method::GET, "/api/v1/ssh_key_secrets/lookup/x", None),
        (Method::PUT, "/api/v1/hmac_secrets/x", Some(&body)),
        (Method::DELETE, "/api/v1/pg_dsn_secrets/pgs_x", None),
    ] {
        f.app.assert_bearer_access(method, path, body).await;
    }
}

#[tokio::test]
async fn console_routes_require_an_admin_session() {
    let Some(f) = setup().await else { return };
    let body = json!({"data": {}});
    for (method, path, body) in [
        (Method::GET, "/api/v1/console/secrets", None),
        (Method::GET, "/api/v1/console/secrets/static/ssr_x", None),
        (
            Method::GET,
            "/api/v1/console/secrets/static/ssr_x/role_grants",
            None,
        ),
        (
            Method::POST,
            "/api/v1/console/secrets/static/ssr_x/role_grants",
            Some(&body),
        ),
        (
            Method::DELETE,
            "/api/v1/console/secrets/static/ssr_x/role_grants/grant_x",
            None,
        ),
        (Method::POST, "/api/v1/console/static_secrets", Some(&body)),
        (
            Method::PATCH,
            "/api/v1/console/pg_dsn_secrets/pgs_x",
            Some(&body),
        ),
        (
            Method::DELETE,
            "/api/v1/console/gcp_auth_secrets/gas_x",
            None,
        ),
    ] {
        f.app
            .assert_console_access(Access::Admin, method, path, body)
            .await;
    }
}

#[tokio::test]
async fn a_browser_session_cannot_use_the_bearer_api() {
    let Some(f) = setup().await else { return };
    f.create("static_secrets", static_body("hidden")).await;
    let mut browser = f.app.client();
    browser.sign_in(f.admin.id);
    for request in [
        f.app.get("/api/v1/static_secrets"),
        f.app.delete("/api/v1/static_secrets/ssr_x"),
    ] {
        let response = browser.send(request.csrf(&browser)).await;
        assert_eq!(response.status(), 401);
        assert!(!response.text().contains("hidden"));
    }
}

/// A sandbox token for an admin's Slack DM principal.
async fn dm_token(f: &Fx) -> String {
    let now = f.app.now().naive_utc();
    sqlx::query(
        "INSERT INTO slack_bot_channels (team_id, bot_user_id, channel_id, name, private, \
         archived, active, member_user_ids, created_at, updated_at) VALUES ('T0123456789', \
         'U0999999999', 'C0123456789', 'general', false, false, true, '{U0123456789}', $1, $1)",
    )
    .bind(now)
    .execute(&f.app.db.pool)
    .await
    .unwrap();
    let principal = f
        .app
        .create_principal("slack_dm", "slack_dm-admin", f.admin.id)
        .await;
    sqlx::query(
        "UPDATE principals SET slack_user_id = 'U0123456789', slack_team_id = 'T0123456789', \
         console_user_id = $2 WHERE id = $1",
    )
    .bind(principal)
    .bind(f.admin.id)
    .execute(&f.app.db.pool)
    .await
    .unwrap();
    let proxy = f.app.create_proxy("proxy-admin-dm", principal).await;
    f.app.sandbox_token(proxy, principal).await
}

async fn as_sandbox(
    f: &Fx,
    token: &str,
    method: Method,
    path: &str,
    body: Option<Value>,
) -> TestResponse {
    let request = f.app.request(method, path).bearer(token);
    match body {
        Some(body) => request.json(&json!({ "data": body })).send().await,
        None => request.send().await,
    }
}

#[tokio::test]
async fn sandbox_admin_routes_require_an_admin_dm_sandbox() {
    let Some(f) = setup().await else { return };
    let path = "/api/v1/sandbox/admin/secrets";
    // No token, a forged one, and an API key are refused.
    assert_eq!(f.app.get(path).send().await.status(), 401);
    assert_eq!(f.app.get(path).bearer("junk").send().await.status(), 401);
    assert_eq!(f.app.get(path).bearer(&f.key).send().await.status(), 401);
    // A sandbox of a principal that is not an admin's DM is refused.
    let user = f.app.create_principal("user", "plain", f.admin.id).await;
    let proxy = f.app.create_proxy("plain-proxy", user).await;
    let plain = f.app.sandbox_token(proxy, user).await;
    for (method, path, body) in [
        (Method::GET, path, None),
        (
            Method::POST,
            "/api/v1/sandbox/admin/static_secrets",
            Some(json!({})),
        ),
        (
            Method::PATCH,
            "/api/v1/sandbox/admin/pg_dsn_secrets/pgs_x",
            Some(json!({})),
        ),
    ] {
        let response = as_sandbox(&f, &plain, method, path, body).await;
        assert_eq!(response.status(), 403, "{path}: {}", response.text());
    }
    let admin = dm_token(&f).await;
    assert_eq!(
        as_sandbox(&f, &admin, Method::GET, path, None)
            .await
            .status(),
        200
    );
}

// --- bearer: every kind -----------------------------------------------------

#[tokio::test]
async fn a_static_secret_round_trips() {
    let Some(f) = setup().await else { return };
    let created = f.create("static_secrets", static_body("example-api")).await;
    assert!(created["id"].as_str().unwrap().starts_with("ssr_"));
    for (field, expected) in [
        ("foreign_id", json!("example-api")),
        ("name", json!("Example API")),
        ("description", json!("An API key")),
        ("kind", json!("custom")),
        ("labels", json!({"team": "platform"})),
        ("inject_config", json!({"header": "X-Api-Key"})),
        ("replace_config", Value::Null),
        ("source", env_source("EXAMPLE_KEY")),
        (
            "rules",
            json!([{"host": "api.example.test", "cidr": null, "position": 0,
                    "http_methods": ["GET"], "paths": ["/v1"]}]),
        ),
    ] {
        assert_eq!(created[field], expected, "{field}");
    }
    // RFC 3339 timestamps.
    assert!(created["created_at"].as_str().unwrap().ends_with('Z'));
    let id = created["id"].as_str().unwrap();
    let shown = f
        .call(Method::GET, &format!("/api/v1/static_secrets/{id}"), None)
        .await;
    assert_eq!(shown.json()["data"], created);
    for path in [
        "/api/v1/static_secrets/lookup/example-api",
        "/api/v1/static_secrets/lookup/default/example-api",
    ] {
        let found = f.call(Method::GET, path, None).await;
        assert_eq!(found.json()["data"]["id"], id, "{path}");
    }
    let created_by: i64 = sqlx::query_scalar("SELECT created_by_id FROM static_secrets")
        .fetch_one(&f.app.db.pool)
        .await
        .unwrap();
    assert_eq!(created_by, f.admin.id);
}

#[tokio::test]
async fn every_kind_can_be_created_listed_and_deleted() {
    let Some(f) = setup().await else { return };
    let keyfile = env_source("SA_KEY");
    let rules = json!([{"host": "api.example.test"}]);
    let cases = vec![
        ("static_secrets", static_body("k-static"), "ssr_"),
        (
            "gcp_auth_secrets",
            json!({"foreign_id": "k-gcp", "keyfile": keyfile, "scopes": ["https://www.googleapis.com/auth/cloud-platform"]}),
            "gas_",
        ),
        (
            "gcp_id_token_secrets",
            json!({"foreign_id": "k-id", "audience": "https://svc.run.app", "keyfile": keyfile, "rules": rules}),
            "gid_",
        ),
        (
            "aws_auth_secrets",
            json!({"foreign_id": "k-aws", "access_key_id": env_source("A"), "secret_access_key": env_source("B"), "rules": rules}),
            "aas_",
        ),
        (
            "oauth_token_secrets",
            json!({"foreign_id": "k-oauth", "grant": "client_credentials",
                   "token_endpoint": "https://idp.example.test/token",
                   "credentials": {"client_id": env_source("C"), "client_secret": env_source("D")},
                   "token_endpoint_headers": {"X-Tenant": env_source("T")}, "rules": rules}),
            "ots_",
        ),
        (
            "pg_dsn_secrets",
            json!({"foreign_id": "k-pg", "database": "shop", "role": "reader",
                   "settings": [{"name": "app.tenant", "value": "acme"}], "dsn": env_source("DSN")}),
            "pgs_",
        ),
        (
            "hmac_secrets",
            json!({"foreign_id": "k-hmac", "timestamp_format": "unix_seconds",
                   "signature_algorithm": "sha256", "signature_key_encoding": "raw",
                   "signature_output_encoding": "hex", "signature_message": "{{ .Body }}",
                   "headers": [{"name": "X-Sig", "value": "{{ .Signature }}"}],
                   "credentials": {"secret": env_source("H")}, "rules": rules}),
            "hms_",
        ),
    ];
    for (resource, body, prefix) in cases {
        let created = f.create(resource, body).await;
        assert!(
            created["id"].as_str().unwrap().starts_with(prefix),
            "{resource}"
        );
        let list = f
            .call(Method::GET, &format!("/api/v1/{resource}"), None)
            .await
            .json();
        assert_eq!(list["data"].as_array().unwrap().len(), 1, "{resource}");
        assert_eq!(
            list["meta"],
            json!({"page": 1, "limit": 50, "total": 1, "total_pages": 1})
        );
        let path = format!("/api/v1/{resource}/{}", created["id"].as_str().unwrap());
        assert_eq!(
            f.call(Method::DELETE, &path, None).await.status(),
            204,
            "{resource}"
        );
        assert_eq!(
            f.call(Method::GET, &path, None).await.status(),
            404,
            "{resource}"
        );
    }
    // Deleting a secret leaves none of its parts behind.
    for table in ["secret_sources", "request_rules"] {
        let left: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM {table}"))
            .fetch_one(&f.app.db.pool)
            .await
            .unwrap();
        assert_eq!(left, 0, "{table}");
    }
}

#[tokio::test]
async fn role_keyed_kinds_render_their_credentials_by_role() {
    let Some(f) = setup().await else { return };
    let created = f
        .create(
            "oauth_token_secrets",
            json!({"foreign_id": "oauth", "grant": "refresh_token",
                   "token_endpoint": "https://idp.example.test/token",
                   "credentials": {"refresh_token": inline_source("rt-plaintext"), "client_id": env_source("C")},
                   "token_endpoint_headers": {"X-Tenant": env_source("T")},
                   "rules": [{"host": "api.example.test"}]}),
        )
        .await;
    assert_eq!(created["credentials"]["client_id"], env_source("C"));
    assert_eq!(
        created["credentials"]["refresh_token"],
        json!({"source_type": "control_plane", "config": {}})
    );
    assert_eq!(
        created["token_endpoint_headers"]["X-Tenant"],
        env_source("T")
    );
    assert!(!created.to_string().contains("rt-plaintext"));
    let aws = f
        .create(
            "aws_auth_secrets",
            json!({"foreign_id": "aws", "access_key_id": env_source("A"),
                   "secret_access_key": env_source("B"), "rules": [{"host": "s3.amazonaws.com"}]}),
        )
        .await;
    assert_eq!(aws["session_token"], Value::Null);
    assert_eq!(aws["access_key_id"], env_source("A"));
}

#[tokio::test]
async fn lists_filter_by_labels_and_paginate() {
    let Some(f) = setup().await else { return };
    for (index, team) in ["a", "b", "a"].iter().enumerate() {
        let mut body = static_body(&format!("s-{index}"));
        body["labels"] = json!({"team": team});
        f.create("static_secrets", body).await;
    }
    let list = f
        .call(Method::GET, "/api/v1/static_secrets?labels[team]=a", None)
        .await
        .json();
    assert_eq!(list["meta"]["total"], 2);
    let names: Vec<&str> = list["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["foreign_id"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["s-0", "s-2"]);
    let page = f
        .call(Method::GET, "/api/v1/static_secrets?limit=2&page=2", None)
        .await
        .json();
    assert_eq!(page["data"].as_array().unwrap().len(), 1);
    assert_eq!(
        page["meta"],
        json!({"page": 2, "limit": 2, "total": 3, "total_pages": 2})
    );
    let none = f
        .call(Method::GET, "/api/v1/static_secrets?labels[team]=zzz", None)
        .await
        .json();
    assert_eq!(none["data"], json!([]));
}

#[tokio::test]
async fn upsert_creates_by_foreign_id_then_updates() {
    let Some(f) = setup().await else { return };
    let mut body = static_body("ignored-in-body");
    body.as_object_mut().unwrap().remove("foreign_id");
    let first = f
        .call(
            Method::PUT,
            "/api/v1/static_secrets/by-ref",
            Some(body.clone()),
        )
        .await;
    assert_eq!(first.status(), 201, "{}", first.text());
    assert_eq!(first.json()["data"]["foreign_id"], "by-ref");
    body["name"] = json!("Renamed");
    let second = f
        .call(
            Method::PATCH,
            "/api/v1/static_secrets/by-ref",
            Some(body.clone()),
        )
        .await;
    assert_eq!(second.status(), 200, "{}", second.text());
    assert_eq!(second.json()["data"]["name"], "Renamed");
    assert_eq!(second.json()["data"]["id"], first.json()["data"]["id"]);
    // By oid too; a foreign id that is not given is kept.
    let id = first.json()["data"]["id"].as_str().unwrap().to_owned();
    body["name"] = json!("By oid");
    let third = f
        .call(
            Method::PUT,
            &format!("/api/v1/static_secrets/{id}"),
            Some(body.clone()),
        )
        .await;
    assert_eq!(third.json()["data"]["foreign_id"], "by-ref");
    assert_eq!(third.json()["data"]["name"], "By oid");
    // The identity a write is addressed by is not what it changes, and a
    // blank one is left out.
    body["foreign_id"] = Value::Null;
    let by_ref = f
        .call(
            Method::PUT,
            "/api/v1/static_secrets/by-ref",
            Some(body.clone()),
        )
        .await;
    assert_eq!(by_ref.json()["data"]["foreign_id"], "by-ref");
    body["foreign_id"] = json!("  ");
    let blank = f
        .call(
            Method::PUT,
            &format!("/api/v1/static_secrets/{id}"),
            Some(body),
        )
        .await;
    assert_eq!(blank.json()["data"]["foreign_id"], "by-ref");
    // An unknown oid is a 404, never a new secret.
    let missing = f
        .call(
            Method::PUT,
            "/api/v1/static_secrets/ssr_nonesuch",
            Some(static_body("x")),
        )
        .await;
    assert_eq!(missing.status(), 404);
}

#[tokio::test]
async fn concurrent_upserts_of_one_new_foreign_id_make_one_secret() {
    let Some(f) = setup().await else { return };
    let (a, b) = tokio::join!(
        f.call(
            Method::PUT,
            "/api/v1/static_secrets/race",
            Some(static_body("race"))
        ),
        f.call(
            Method::PUT,
            "/api/v1/static_secrets/race",
            Some(static_body("race"))
        ),
    );
    let mut statuses = [a.status(), b.status()];
    statuses.sort_unstable();
    assert!(matches!(statuses, [200, 201] | [201, 201]), "{statuses:?}");
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM static_secrets")
        .fetch_one(&f.app.db.pool)
        .await
        .unwrap();
    assert_eq!(count, 1);
}

#[tokio::test]
async fn unknown_and_malformed_ids_are_404() {
    let Some(f) = setup().await else { return };
    for (method, path) in [
        (Method::GET, "/api/v1/static_secrets/ssr_nonesuch"),
        (Method::GET, "/api/v1/static_secrets/nonsense"),
        (Method::GET, "/api/v1/pg_dsn_secrets/ssr_wrongprefix"),
        (Method::GET, "/api/v1/static_secrets/lookup/missing"),
        (Method::DELETE, "/api/v1/static_secrets/ssr_nonesuch"),
    ] {
        let response = f.call(method.clone(), path, None).await;
        assert_eq!(response.status(), 404, "{method} {path}");
        assert!(response.error_message().is_some());
    }
}

// --- validation -------------------------------------------------------------

#[tokio::test]
async fn invalid_documents_are_422_with_fields_and_write_nothing() {
    let Some(f) = setup().await else { return };
    let post = |resource: &'static str, body: Value| {
        let f = &f;
        async move {
            f.call(Method::POST, &format!("/api/v1/{resource}"), Some(body))
                .await
        }
    };
    // A static secret needs exactly one mode, a source, a valid schema.
    let response = post("static_secrets", json!({"name": "x"})).await;
    assert_eq!(response.status(), 422);
    assert!(error_fields(&response).contains(&"base".to_owned()));
    let mut body = static_body("bad-schema");
    body["inject_config"] = json!({"header": "X", "query_param": "q"});
    assert!(
        error_fields(&post("static_secrets", body).await).contains(&"inject_config".to_owned())
    );
    let mut body = static_body("bad-source");
    body["source"] = json!({"source_type": "env", "config": {}});
    assert!(
        error_fields(&post("static_secrets", body).await).contains(&"source.config".to_owned())
    );
    let mut body = static_body("bad-rule");
    body["rules"] = json!([{"host": "a.test", "cidr": "10.0.0.0/8"}, {"cidr": "nope"}]);
    let fields = error_fields(&post("static_secrets", body).await);
    assert!(fields.contains(&"rules[0].base".to_owned()), "{fields:?}");
    assert!(fields.contains(&"rules[1].cidr".to_owned()), "{fields:?}");
    for bad in ["has space", "ssr_reserved"] {
        let response = post("static_secrets", static_body(bad)).await;
        assert!(
            error_fields(&response).contains(&"foreign_id".to_owned()),
            "{bad}"
        );
    }
    // Per-kind requirements.
    let response = post("pg_dsn_secrets", json!({})).await;
    let fields = error_fields(&response);
    for field in ["foreign_id", "database", "dsn_source"] {
        assert!(fields.contains(&field.to_owned()), "{field}");
    }
    let response = post(
        "gcp_id_token_secrets",
        json!({"audience": "a", "keyfile": env_source("K"), "header": "x-nope"}),
    )
    .await;
    let fields = error_fields(&response);
    assert!(fields.contains(&"header".to_owned()) && fields.contains(&"rules".to_owned()));
    let response = post(
        "oauth_token_secrets",
        json!({"grant": "magic", "rules": [{"host": "a.test"}]}),
    )
    .await;
    let fields = error_fields(&response);
    assert!(fields.contains(&"grant".to_owned()) && fields.contains(&"token_endpoint".to_owned()));
    let response = post("hmac_secrets", json!({"signature_algorithm": "md5"})).await;
    assert!(error_fields(&response).contains(&"signature_algorithm".to_owned()));
    // Nothing was written.
    for table in [
        "static_secrets",
        "pg_dsn_secrets",
        "secret_sources",
        "request_rules",
    ] {
        let count: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM {table}"))
            .fetch_one(&f.app.db.pool)
            .await
            .unwrap();
        assert_eq!(count, 0, "{table}");
    }
}

#[tokio::test]
async fn an_inline_dsn_must_name_the_secrets_database() {
    let Some(f) = setup().await else { return };
    let body =
        |dsn: &str| json!({"foreign_id": "pg", "database": "shop", "dsn": inline_source(dsn)});
    let response = f
        .call(
            Method::POST,
            "/api/v1/pg_dsn_secrets",
            Some(body("postgres://u:pw-sentinel@h/other")),
        )
        .await;
    assert_eq!(response.status(), 422);
    assert!(error_fields(&response).contains(&"database".to_owned()));
    assert!(!response.text().contains("pw-sentinel"));
    let ok = f
        .call(
            Method::POST,
            "/api/v1/pg_dsn_secrets",
            Some(body("postgres://u:pw@h:5432/shop")),
        )
        .await;
    assert_eq!(ok.status(), 201, "{}", ok.text());
}

#[tokio::test]
async fn foreign_ids_are_unique() {
    let Some(f) = setup().await else { return };
    f.create("static_secrets", static_body("taken")).await;
    let response = f
        .call(
            Method::POST,
            "/api/v1/static_secrets",
            Some(static_body("taken")),
        )
        .await;
    assert_eq!(response.status(), 422);
    assert_eq!(
        response.json()["error"]["details"]["foreign_id"],
        json!(["has already been taken"])
    );
}

#[tokio::test]
async fn a_wrongly_shaped_body_is_a_400_that_does_not_echo_it() {
    let Some(f) = setup().await else { return };
    let response = f
        .call(
            Method::POST,
            "/api/v1/static_secrets",
            Some(json!({"name": "x", "source": {"secret": 424242424}})),
        )
        .await;
    assert_eq!(response.status(), 400);
    assert!(!response.text().contains("424242424"));
    // Not wrapped in `data`.
    let response = f
        .app
        .post("/api/v1/static_secrets")
        .bearer(&f.key)
        .json(&json!({"name": "x"}))
        .send()
        .await;
    assert_eq!(response.status(), 400);
}

#[tokio::test]
async fn the_github_profile_fills_in_its_canonical_form() {
    let Some(f) = setup().await else { return };
    let created = f
        .create(
            "static_secrets",
            json!({"foreign_id": "gh", "kind": "github_token", "source": env_source("GITHUB_PAT")}),
        )
        .await;
    assert_eq!(created["kind"], "github_token");
    assert_eq!(created["replace_config"]["proxy_value"], "GITHUB_TOKEN");
    let hosts: Vec<&str> = created["rules"]
        .as_array()
        .unwrap()
        .iter()
        .map(|rule| rule["host"].as_str().unwrap())
        .collect();
    assert_eq!(
        hosts,
        ["api.github.com", "github.com", "api.githubcopilot.com"]
    );
    // Rules for other hosts are refused.
    let response = f
        .call(
            Method::POST,
            "/api/v1/static_secrets",
            Some(
                json!({"foreign_id": "gh2", "kind": "github_token", "source": env_source("X"),
                        "rules": [{"host": "evil.example"}]}),
            ),
        )
        .await;
    assert!(error_fields(&response).contains(&"rules".to_owned()));
}

// --- inline values -----------------------------------------------------------

#[tokio::test]
async fn inline_values_are_encrypted_and_never_returned() {
    let Some(f) = setup().await else { return };
    let mut body = static_body("inline");
    body["source"] = inline_source("sentinel-plaintext");
    let created = f.create("static_secrets", body.clone()).await;
    assert_eq!(
        created["source"],
        json!({"source_type": "control_plane", "config": {}})
    );
    let id = id_of(&created);
    let stored: String = sqlx::query_scalar("SELECT secret FROM secret_sources")
        .fetch_one(&f.app.db.pool)
        .await
        .unwrap();
    assert!(!stored.contains("sentinel-plaintext"));
    assert_eq!(
        f.stored_secret("static_secret_id", id).await.as_deref(),
        Some("sentinel-plaintext")
    );
    // Not in a listing, a lookup, or a validation error either.
    for path in [
        "/api/v1/static_secrets",
        "/api/v1/static_secrets/lookup/inline",
    ] {
        assert!(
            !f.call(Method::GET, path, None)
                .await
                .text()
                .contains("sentinel")
        );
    }
    body["inject_config"] = json!({"header": "X", "query_param": "q"});
    let rejected = f
        .call(Method::POST, "/api/v1/static_secrets", Some(body))
        .await;
    assert_eq!(rejected.status(), 422);
    assert!(!rejected.text().contains("sentinel"));
}

#[tokio::test]
async fn an_inline_source_without_a_value_keeps_the_stored_one() {
    let Some(f) = setup().await else { return };
    let mut body = static_body("keep");
    body["source"] = inline_source("stored-value");
    let created = f.create("static_secrets", body.clone()).await;
    body["name"] = json!("Renamed");
    body["source"] = json!({"source_type": "control_plane", "config": {}});
    let id = created["id"].as_str().unwrap();
    let response = f
        .call(
            Method::PATCH,
            &format!("/api/v1/static_secrets/{id}"),
            Some(body.clone()),
        )
        .await;
    assert_eq!(response.status(), 200, "{}", response.text());
    assert_eq!(response.json()["data"]["name"], "Renamed");
    assert_eq!(
        f.stored_secret("static_secret_id", id_of(&created))
            .await
            .as_deref(),
        Some("stored-value")
    );
    // A new value replaces it; a non-inline source drops it.
    body["source"] = inline_source("new-value");
    f.call(
        Method::PATCH,
        &format!("/api/v1/static_secrets/{id}"),
        Some(body.clone()),
    )
    .await;
    assert_eq!(
        f.stored_secret("static_secret_id", id_of(&created))
            .await
            .as_deref(),
        Some("new-value")
    );
    body["source"] = env_source("E");
    f.call(
        Method::PATCH,
        &format!("/api/v1/static_secrets/{id}"),
        Some(body),
    )
    .await;
    assert_eq!(
        f.stored_secret("static_secret_id", id_of(&created)).await,
        None
    );
}

// --- no lost updates -----------------------------------------------------------

/// Regression: a metadata-only edit must read the inline value under the
/// row lock, so a rotation that commits while the edit waits is kept.
#[tokio::test]
async fn a_rotation_that_commits_before_the_edit_locks_is_kept() {
    let Some(f) = setup().await else { return };
    let mut body = static_body("rotating");
    body["source"] = inline_source("stored-value");
    let created = f.create("static_secrets", body.clone()).await;
    let id = id_of(&created);
    body["name"] = json!("Renamed while rotating");
    body["source"] = json!({"source_type": "control_plane", "config": {}});

    // The rotator holds the row lock; the edit queues behind it.
    let mut rotator = f.app.db.pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM static_secrets WHERE id = $1 FOR UPDATE")
        .bind(id)
        .execute(&mut *rotator)
        .await
        .unwrap();
    let rotated = f.app.state.encryptor.encrypt("rotated-value").unwrap();
    let path = format!("/api/v1/static_secrets/{}", created["id"].as_str().unwrap());
    let edit = f.call(Method::PATCH, &path, Some(body));
    let rotate = async {
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        sqlx::query("UPDATE secret_sources SET secret = $1 WHERE static_secret_id = $2")
            .bind(rotated)
            .bind(id)
            .execute(&mut *rotator)
            .await
            .unwrap();
        rotator.commit().await.unwrap();
    };
    let (response, ()) = tokio::join!(edit, rotate);
    assert_eq!(response.status(), 200, "{}", response.text());
    assert_eq!(response.json()["data"]["name"], "Renamed while rotating");
    assert_eq!(
        f.stored_secret("static_secret_id", id).await.as_deref(),
        Some("rotated-value")
    );
}

#[tokio::test]
async fn an_edit_writes_only_the_columns_a_document_carries() {
    let Some(f) = setup().await else { return };
    let created = f.create("static_secrets", static_body("cols")).await;
    let id = id_of(&created);
    // Links no document carries (here set directly; foreign keys are not
    // what this checks) survive an edit.
    let mut conn = f.app.db.pool.acquire().await.unwrap();
    sqlx::query("SET session_replication_role = replica")
        .execute(&mut *conn)
        .await
        .unwrap();
    sqlx::query("UPDATE static_secrets SET broker_credential_id = 4242 WHERE id = $1")
        .bind(id)
        .execute(&mut *conn)
        .await
        .unwrap();
    drop(conn);
    let mut body = static_body("cols");
    body["name"] = json!("Edited");
    let path = format!("/api/v1/static_secrets/{}", created["id"].as_str().unwrap());
    assert_eq!(f.call(Method::PUT, &path, Some(body)).await.status(), 200);
    let (name, broker, created_by): (String, Option<i64>, Option<i64>) = sqlx::query_as(
        "SELECT name, broker_credential_id, created_by_id FROM static_secrets WHERE id = $1",
    )
    .bind(id)
    .fetch_one(&f.app.db.pool)
    .await
    .unwrap();
    assert_eq!(
        (name.as_str(), broker, created_by),
        ("Edited", Some(4242), Some(f.admin.id))
    );
}

// --- sync config ---------------------------------------------------------------

#[tokio::test]
async fn writes_refresh_the_config_of_every_grantee_and_no_one_else() {
    let Some(f) = setup().await else { return };
    let created = f.create("static_secrets", static_body("synced")).await;
    let id = id_of(&created);
    let (direct, via_role) = f.grantees("static_secret_id", id).await;
    let bystander = f
        .app
        .create_principal("user", "bystander", f.admin.id)
        .await;
    let path = format!("/api/v1/static_secrets/{}", created["id"].as_str().unwrap());
    let versions = || async {
        (
            f.version(direct).await,
            f.version(via_role).await,
            f.version(bystander).await,
        )
    };
    let before = versions().await;

    // The same document again changes nothing, so nothing is refreshed.
    assert_eq!(
        f.call(Method::PUT, &path, Some(static_body("synced")))
            .await
            .status(),
        200
    );
    assert_eq!(versions().await, before);

    // A changed column, a changed source, a changed rule: each refreshes.
    let mut body = static_body("synced");
    body["name"] = json!("Renamed");
    f.call(Method::PUT, &path, Some(body.clone())).await;
    assert_eq!(versions().await, (before.0 + 1, before.1 + 1, before.2));
    body["source"] = env_source("OTHER");
    f.call(Method::PUT, &path, Some(body.clone())).await;
    body["rules"] = json!([{"host": "other.example.test"}]);
    f.call(Method::PUT, &path, Some(body)).await;
    assert_eq!(versions().await, (before.0 + 3, before.1 + 3, before.2));

    // Deleting refreshes whoever held it.
    assert_eq!(f.call(Method::DELETE, &path, None).await.status(), 204);
    assert_eq!(versions().await, (before.0 + 4, before.1 + 4, before.2));
}

#[tokio::test]
async fn an_unchanged_document_keeps_its_rows() {
    let Some(f) = setup().await else { return };
    let created = f.create("static_secrets", static_body("steady")).await;
    let before: (chrono::NaiveDateTime, i64, i64) = sqlx::query_as(
        "SELECT (SELECT updated_at FROM static_secrets), (SELECT id FROM secret_sources), \
         (SELECT id FROM request_rules)",
    )
    .fetch_one(&f.app.db.pool)
    .await
    .unwrap();
    f.app.clock.advance(chrono::Duration::seconds(60));
    let path = format!("/api/v1/static_secrets/{}", created["id"].as_str().unwrap());
    assert_eq!(
        f.call(Method::PUT, &path, Some(static_body("steady")))
            .await
            .status(),
        200
    );
    let after: (chrono::NaiveDateTime, i64, i64) = sqlx::query_as(
        "SELECT (SELECT updated_at FROM static_secrets), (SELECT id FROM secret_sources), \
         (SELECT id FROM request_rules)",
    )
    .fetch_one(&f.app.db.pool)
    .await
    .unwrap();
    assert_eq!(before, after);
}

// --- ssh keys -------------------------------------------------------------------

// A throwaway Ed25519 key generated for these tests only.
const ED25519: &str = "-----BEGIN OPENSSH PRIVATE KEY-----
b3BlbnNzaC1rZXktdjEAAAAABG5vbmUAAAAEbm9uZQAAAAAAAAABAAAAMwAAAAtzc2gtZW
QyNTUxOQAAACCM5PbHMICER29FSSQxVg1BlvlSr181HbjPaJLDhYMQ3AAAAJhEonyMRKJ8
jAAAAAtzc2gtZWQyNTUxOQAAACCM5PbHMICER29FSSQxVg1BlvlSr181HbjPaJLDhYMQ3A
AAAED96ea9aUZyIb7JKnzjtYc4Q17zOVoDO3kvWsa6p6n2FYzk9scwgIRHb0VJJDFWDUGW
+VKvXzUduM9oksOFgxDcAAAAEXRlc3RAZXhhbXBsZS50ZXN0AQIDBA==
-----END OPENSSH PRIVATE KEY-----
";

#[tokio::test]
async fn an_ssh_key_derives_its_public_key_from_an_inline_private_key() {
    let Some(f) = setup().await else { return };
    let created = f
        .create(
            "ssh_key_secrets",
            json!({"foreign_id": "deploy", "name": "  Deploy key ", "private_key": inline_source(ED25519)}),
        )
        .await;
    assert_eq!(created["name"], "Deploy key");
    assert!(
        created["public_key"]
            .as_str()
            .unwrap()
            .starts_with("ssh-ed25519 AAAA")
    );
    assert_eq!(created["personal"], false);
    assert_eq!(
        created["private_key"],
        json!({"source_type": "control_plane", "config": {}})
    );
    assert!(!created.to_string().contains("OPENSSH PRIVATE KEY"));
    let stored = f
        .stored_secret("ssh_key_secret_id", id_of(&created))
        .await
        .unwrap();
    assert!(
        stored.starts_with("-----BEGIN OPENSSH PRIVATE KEY-----") && stored.ends_with("-----\n")
    );
    // A public key and garbage are refused without echoing them.
    for bad in ["ssh-ed25519 AAAAC3Nza me@host", "nonsense-sentinel"] {
        let response = f
            .call(
                Method::POST,
                "/api/v1/ssh_key_secrets",
                Some(json!({"name": "bad", "private_key": inline_source(bad)})),
            )
            .await;
        assert_eq!(response.status(), 422, "{bad}");
        assert!(error_fields(&response).contains(&"private_key".to_owned()));
        assert!(!response.text().contains(bad));
    }
}

#[tokio::test]
async fn a_referenced_ssh_key_needs_a_valid_public_key() {
    let Some(f) = setup().await else { return };
    let body = |public_key: &str| json!({"name": "ref", "public_key": public_key, "private_key": env_source("SSH")});
    let response = f
        .call(Method::POST, "/api/v1/ssh_key_secrets", Some(body("nope")))
        .await;
    assert!(error_fields(&response).contains(&"public_key".to_owned()));
    let derived = centaur_console_db::keys::inspect(ED25519)
        .unwrap()
        .public_key;
    let ok = f
        .call(
            Method::POST,
            "/api/v1/ssh_key_secrets",
            Some(body(&format!("{derived} comment"))),
        )
        .await;
    assert_eq!(ok.status(), 201, "{}", ok.text());
    assert_eq!(ok.json()["data"]["public_key"], derived.as_str());
}

#[tokio::test]
async fn personal_ssh_keys_are_not_editable_through_the_api() {
    let Some(f) = setup().await else { return };
    let created = f
        .create(
            "ssh_key_secrets",
            json!({"name": "mine", "private_key": inline_source(ED25519)}),
        )
        .await;
    sqlx::query("UPDATE ssh_key_secrets SET owner_id = $1")
        .bind(f.admin.id)
        .execute(&f.app.db.pool)
        .await
        .unwrap();
    let path = format!(
        "/api/v1/ssh_key_secrets/{}",
        created["id"].as_str().unwrap()
    );
    let response = f
        .call(
            Method::PATCH,
            &path,
            Some(json!({"name": "hijacked", "private_key": inline_source(ED25519)})),
        )
        .await;
    assert_eq!(response.status(), 403);
    let shown = f.call(Method::GET, &path, None).await.json();
    assert_eq!(shown["data"]["personal"], true);
    assert_eq!(shown["data"]["name"], "mine");
}

// --- console ---------------------------------------------------------------------

async fn console(f: &Fx, method: Method, path: &str, body: Option<Value>) -> TestResponse {
    let mut browser = f.app.client();
    browser.sign_in(f.admin.id);
    let mut request = f.app.request(method, path);
    if let Some(body) = body {
        request = request.json(&json!({ "data": body }));
    }
    browser.send(request.csrf(&browser)).await
}

#[tokio::test]
async fn the_console_edits_the_kinds_it_has_forms_for() {
    let Some(f) = setup().await else { return };
    let created = console(
        &f,
        Method::POST,
        "/api/v1/console/static_secrets",
        Some(static_body("console")),
    )
    .await;
    assert_eq!(created.status(), 201, "{}", created.text());
    let id = created.json()["data"]["id"].as_str().unwrap().to_owned();
    // A cleared foreign id is an explicit null; an omitted one is kept.
    let mut body = static_body("console");
    body.as_object_mut().unwrap().remove("foreign_id");
    let kept = console(
        &f,
        Method::PATCH,
        &format!("/api/v1/console/static_secrets/{id}"),
        Some(body.clone()),
    )
    .await;
    assert_eq!(kept.json()["data"]["foreign_id"], "console");
    body["foreign_id"] = Value::Null;
    let cleared = console(
        &f,
        Method::PATCH,
        &format!("/api/v1/console/static_secrets/{id}"),
        Some(body),
    )
    .await;
    assert_eq!(cleared.json()["data"]["foreign_id"], Value::Null);
    // By oid only: a foreign id is not a console update target.
    let missing = console(
        &f,
        Method::PATCH,
        "/api/v1/console/static_secrets/console",
        Some(static_body("x")),
    )
    .await;
    assert_eq!(missing.status(), 404);
    let deleted = console(
        &f,
        Method::DELETE,
        &format!("/api/v1/console/static_secrets/{id}"),
        None,
    )
    .await;
    assert_eq!(deleted.status(), 204);
    // ssh keys have no console form.
    let none = console(
        &f,
        Method::POST,
        "/api/v1/console/ssh_key_secrets",
        Some(json!({})),
    )
    .await;
    assert_eq!(none.status(), 404);
}

#[tokio::test]
async fn the_console_lists_every_kind_by_name_with_filters() {
    let Some(f) = setup().await else { return };
    f.create("static_secrets", static_body("zeta")).await;
    let mut alpha = static_body("alpha");
    alpha["name"] = json!("Alpha 100%");
    f.create("static_secrets", alpha).await;
    f.create(
        "pg_dsn_secrets",
        json!({"foreign_id": "mid", "name": "mid", "database": "d", "dsn": inline_source("postgres://u:p@h/d")}),
    )
    .await;
    let list = console(&f, Method::GET, "/api/v1/console/secrets", None)
        .await
        .json();
    let names: Vec<&str> = list["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["foreign_id"].as_str().unwrap())
        .collect();
    // By name ("Alpha 100%", "Example API", "mid"), case-insensitively.
    assert_eq!(names, ["alpha", "zeta", "mid"]);
    assert_eq!(list["meta"]["total"], 3);
    let first = &list["data"][0];
    assert_eq!(first["kind"], "static");
    assert_eq!(first["kind_label"], "Static");
    assert_eq!(first["grant_key"], "static_secret_id");
    assert_eq!(first["source_types"], json!(["Env"]));
    assert_eq!(first["managed_by"], Value::Null);
    assert_eq!(list["data"][2]["source_types"], json!(["Inline"]));
    assert!(!list.to_string().contains("postgres://"));
    // Filters: kind, search (wildcards are literal), pagination.
    let only = console(&f, Method::GET, "/api/v1/console/secrets?kind=pg_dsn", None)
        .await
        .json();
    assert_eq!(only["meta"]["total"], 1);
    let found = console(&f, Method::GET, "/api/v1/console/secrets?q=100%25", None)
        .await
        .json();
    assert_eq!(found["meta"]["total"], 1);
    let none = console(&f, Method::GET, "/api/v1/console/secrets?q=%25", None)
        .await
        .json();
    assert_eq!(none["meta"]["total"], 1);
    let page = console(
        &f,
        Method::GET,
        "/api/v1/console/secrets?limit=1&page=3",
        None,
    )
    .await
    .json();
    assert_eq!(page["data"][0]["foreign_id"], "mid");
    let bad = console(&f, Method::GET, "/api/v1/console/secrets?kind=bogus", None).await;
    assert_eq!(bad.status(), 400);
}

#[tokio::test]
async fn the_console_list_can_leave_out_what_a_grantee_holds() {
    let Some(f) = setup().await else { return };
    let held = f.create("static_secrets", static_body("held")).await;
    f.create("static_secrets", static_body("free")).await;
    let (direct, _) = f.grantees("static_secret_id", id_of(&held)).await;
    let path = format!(
        "/api/v1/console/secrets?exclude_grantee=principal:{}",
        oid_of("prn", direct)
    );
    let list = console(&f, Method::GET, &path, None).await.json();
    let names: Vec<&str> = list["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["foreign_id"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["free"]);
    for bad in ["nonsense", "user:1"] {
        let response = console(
            &f,
            Method::GET,
            &format!("/api/v1/console/secrets?exclude_grantee={bad}"),
            None,
        )
        .await;
        assert_eq!(response.status(), 400, "{bad}");
    }
    let missing = console(
        &f,
        Method::GET,
        "/api/v1/console/secrets?exclude_grantee=principal:prn_nonesuch",
        None,
    )
    .await;
    assert_eq!(missing.status(), 404);
}

#[tokio::test]
async fn the_console_detail_explains_a_secret_without_its_value() {
    let Some(f) = setup().await else { return };
    let mut body = static_body("detail");
    body["source"] = inline_source("sentinel-plaintext");
    let created = f.create("static_secrets", body).await;
    let id = created["id"].as_str().unwrap();
    let shown = console(
        &f,
        Method::GET,
        &format!("/api/v1/console/secrets/static/{id}"),
        None,
    )
    .await;
    assert_eq!(shown.status(), 200, "{}", shown.text());
    let detail = shown.json()["data"].clone();
    assert_eq!(detail["editable"], true);
    assert_eq!(detail["injection"], "X-Api-Key");
    assert_eq!(
        detail["source_segments"],
        json!([{"role": null, "type": "control_plane", "ref": "inline", "type_label": "Inline"}])
    );
    assert_eq!(
        detail["configuration"][0],
        json!({"label": "Injects into", "value": "X-Api-Key"})
    );
    assert_eq!(detail["signed_headers"], Value::Null);
    assert_eq!(detail["rules"][0]["host"], "api.example.test");
    assert!(!shown.text().contains("sentinel"));
    // An unknown kind, a wrong-kind oid, and an unknown oid are 404s.
    for path in [
        "/api/v1/console/secrets/bogus/ssr_x".to_owned(),
        format!("/api/v1/console/secrets/pg_dsn/{id}"),
        "/api/v1/console/secrets/static/ssr_nonesuch".to_owned(),
    ] {
        assert_eq!(
            console(&f, Method::GET, &path, None).await.status(),
            404,
            "{path}"
        );
    }
    // Workload identity shows its provider as the source.
    let wi = f
        .create(
            "gcp_auth_secrets",
            json!({"foreign_id": "wi", "credentials_provider": {"type": "workload_identity"},
                   "scopes": ["https://www.googleapis.com/auth/cloud-platform"]}),
        )
        .await;
    let detail = console(
        &f,
        Method::GET,
        &format!(
            "/api/v1/console/secrets/gcp_auth/{}",
            wi["id"].as_str().unwrap()
        ),
        None,
    )
    .await
    .json();
    assert_eq!(detail["data"]["source_types"], json!(["Workload-Identity"]));
}

async fn role(f: &Fx, name: &str) -> i64 {
    let now = f.app.now().naive_utc();
    sqlx::query_scalar(
        "INSERT INTO roles (name, foreign_id, created_by_id, created_at, updated_at) \
         VALUES ($1, $1, $2, $3, $3) RETURNING id",
    )
    .bind(name)
    .bind(f.admin.id)
    .bind(now)
    .fetch_one(&f.app.db.pool)
    .await
    .unwrap()
}

#[tokio::test]
async fn role_grants_are_idempotent_and_refresh_the_members() {
    let Some(f) = setup().await else { return };
    let secret = f.create("static_secrets", static_body("granted")).await;
    let secret_oid = secret["id"].as_str().unwrap();
    let readers = role(&f, "readers").await;
    let writers = role(&f, "writers").await;
    let member = f.app.create_principal("user", "member", f.admin.id).await;
    let now = f.app.now().naive_utc();
    sqlx::query("INSERT INTO principal_roles (principal_id, role_id, created_at, updated_at) VALUES ($1, $2, $3, $3)")
        .bind(member)
        .bind(readers)
        .bind(now)
        .execute(&f.app.db.pool)
        .await
        .unwrap();
    let base = format!("/api/v1/console/secrets/static/{secret_oid}/role_grants");
    let before = f.version(member).await;
    let first = console(
        &f,
        Method::POST,
        &base,
        Some(json!({"role_id": oid_of("role", readers)})),
    )
    .await;
    assert_eq!(first.status(), 201, "{}", first.text());
    let grant = first.json()["data"].clone();
    assert!(grant["id"].as_str().unwrap().starts_with("grant_"));
    assert_eq!(grant["role"]["name"], "readers");
    assert_eq!(grant["grantable"]["id"], secret_oid);
    assert_eq!(grant["priority"], 0);
    assert_eq!(f.version(member).await, before + 1);
    let again = console(
        &f,
        Method::POST,
        &base,
        Some(json!({"role_id": oid_of("role", readers)})),
    )
    .await;
    assert_eq!(again.status(), 200);
    assert_eq!(again.json()["data"]["id"], grant["id"]);
    assert_eq!(f.version(member).await, before + 1);

    let index = console(&f, Method::GET, &base, None).await.json();
    assert_eq!(index["data"].as_array().unwrap().len(), 1);
    let assignable: Vec<&str> = index["assignable_roles"]
        .as_array()
        .unwrap()
        .iter()
        .map(|role| role["name"].as_str().unwrap())
        .collect();
    assert_eq!(assignable, ["writers"]);
    let _ = writers;

    for body in [json!({}), json!({"role_id": ""})] {
        assert_eq!(
            console(&f, Method::POST, &base, Some(body)).await.status(),
            400
        );
    }
    let unknown = console(
        &f,
        Method::POST,
        &base,
        Some(json!({"role_id": "role_nonesuch"})),
    )
    .await;
    assert_eq!(unknown.status(), 404);

    let delete = format!("{base}/{}", grant["id"].as_str().unwrap());
    assert_eq!(
        console(&f, Method::DELETE, &delete, None).await.status(),
        204
    );
    assert_eq!(f.version(member).await, before + 2);
    assert_eq!(
        console(&f, Method::DELETE, &delete, None).await.status(),
        404
    );
}

// --- sandbox admin ------------------------------------------------------------------

#[tokio::test]
async fn a_sandbox_finds_secrets_by_identifying_metadata_only() {
    let Some(f) = setup().await else { return };
    f.create("static_secrets", static_body("staging-key")).await;
    f.create(
        "pg_dsn_secrets",
        json!({"foreign_id": "warehouse", "database": "w", "dsn": inline_source("postgres://u:pw@h/w")}),
    )
    .await;
    let token = dm_token(&f).await;
    let all = as_sandbox(
        &f,
        &token,
        Method::GET,
        "/api/v1/sandbox/admin/secrets",
        None,
    )
    .await;
    let rows = all.json()["data"].as_array().unwrap().clone();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["grant_key"], "static_secret_id");
    let mut keys: Vec<&String> = rows[0].as_object().unwrap().keys().collect();
    keys.sort();
    assert_eq!(
        keys,
        [
            "description",
            "foreign_id",
            "grant_key",
            "id",
            "labels",
            "name"
        ]
    );
    let found = as_sandbox(
        &f,
        &token,
        Method::GET,
        "/api/v1/sandbox/admin/secrets?q=ware",
        None,
    )
    .await;
    assert_eq!(found.json()["data"].as_array().unwrap().len(), 1);
    let wildcard = as_sandbox(
        &f,
        &token,
        Method::GET,
        "/api/v1/sandbox/admin/secrets?q=%25",
        None,
    )
    .await;
    assert_eq!(wildcard.json()["data"], json!([]));
}

#[tokio::test]
async fn a_sandbox_references_external_stores_but_never_inline_values() {
    let Some(f) = setup().await else { return };
    let token = dm_token(&f).await;
    let create = |body: Value| {
        let (f, token) = (&f, &token);
        async move {
            as_sandbox(
                f,
                token,
                Method::POST,
                "/api/v1/sandbox/admin/static_secrets",
                Some(body),
            )
            .await
        }
    };
    let created = create(static_body("from-sandbox")).await;
    assert_eq!(created.status(), 201, "{}", created.text());
    let created_by: i64 = sqlx::query_scalar("SELECT created_by_id FROM static_secrets")
        .fetch_one(&f.app.db.pool)
        .await
        .unwrap();
    assert_eq!(created_by, f.admin.id);
    for source_type in ["control_plane", "token_broker"] {
        let mut body = static_body("refused");
        body["source"] =
            json!({"source_type": source_type, "secret": "v", "config": {"credential_id": "x"}});
        let response = create(body).await;
        assert_eq!(response.status(), 403, "{source_type}");
        assert!(response.error_message().unwrap().contains(source_type));
    }
    // Only the three kinds a sandbox may touch are routed.
    let other = as_sandbox(
        &f,
        &token,
        Method::POST,
        "/api/v1/sandbox/admin/hmac_secrets",
        Some(json!({})),
    )
    .await;
    assert_eq!(other.status(), 404);
}

#[tokio::test]
async fn a_sandbox_edit_changes_only_the_fields_it_sends() {
    let Some(f) = setup().await else { return };
    let token = dm_token(&f).await;
    let mut body = static_body("partial");
    body["source"] = inline_source("operator-set-value");
    let created = f.create("static_secrets", body).await;
    let id = created["id"].as_str().unwrap();
    let path = format!("/api/v1/sandbox/admin/static_secrets/{id}");
    let edited = as_sandbox(
        &f,
        &token,
        Method::PATCH,
        &path,
        Some(json!({"description": "new words"})),
    )
    .await;
    assert_eq!(edited.status(), 200, "{}", edited.text());
    let after = edited.json()["data"].clone();
    assert_eq!(after["description"], "new words");
    for field in [
        "name",
        "labels",
        "inject_config",
        "rules",
        "source",
        "foreign_id",
    ] {
        assert_eq!(after[field], created[field], "{field}");
    }
    // The inline value an operator set is untouched.
    assert_eq!(
        f.stored_secret("static_secret_id", id_of(&created))
            .await
            .as_deref(),
        Some("operator-set-value")
    );
    // Rules are replaced when sent; an empty list clears them.
    let cleared = as_sandbox(&f, &token, Method::PUT, &path, Some(json!({"rules": []}))).await;
    assert_eq!(cleared.json()["data"]["rules"], json!([]));
    // A sandbox cannot swap in an inline value.
    let swap = as_sandbox(
        &f,
        &token,
        Method::PATCH,
        &path,
        Some(json!({"source": inline_source("mine")})),
    )
    .await;
    assert_eq!(swap.status(), 403);
    // Nor edit a secret an OAuth credential manages.
    sqlx::query("UPDATE secret_sources SET source_type = 'token_broker', secret = NULL, config = '{\"credential_id\": \"c\"}'")
        .execute(&f.app.db.pool)
        .await
        .unwrap();
    let managed = as_sandbox(&f, &token, Method::PATCH, &path, Some(json!({"name": "x"}))).await;
    assert_eq!(managed.status(), 403);
    let missing = as_sandbox(
        &f,
        &token,
        Method::PATCH,
        "/api/v1/sandbox/admin/static_secrets/ssr_nonesuch",
        Some(json!({})),
    )
    .await;
    assert_eq!(missing.status(), 404);
}

#[tokio::test]
async fn a_sandbox_edit_that_breaks_a_rule_is_refused_and_changes_nothing() {
    let Some(f) = setup().await else { return };
    let token = dm_token(&f).await;
    let created = f.create("static_secrets", static_body("guarded")).await;
    let path = format!(
        "/api/v1/sandbox/admin/static_secrets/{}",
        created["id"].as_str().unwrap()
    );
    let response = as_sandbox(
        &f,
        &token,
        Method::PATCH,
        &path,
        Some(json!({"inject_config": {"header": "X", "query_param": "q"}, "name": "changed"})),
    )
    .await;
    assert_eq!(response.status(), 422);
    let shown = f
        .call(
            Method::GET,
            &format!("/api/v1/static_secrets/{}", created["id"].as_str().unwrap()),
            None,
        )
        .await;
    assert_eq!(shown.json()["data"]["name"], "Example API");
}
