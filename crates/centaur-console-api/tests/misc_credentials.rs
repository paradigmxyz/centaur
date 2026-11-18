//! The caller's personal SSH key, bearer API keys, and proxies.

mod misc_common;

use axum::http::Method;
use centaur_console_api::{auth::Access, misc, testing::TestApp};
use centaur_console_db::oid;
use misc_common::{browser, console};
use serde_json::{Value, json};

// A throwaway key generated for tests only.
const KEY: &str = "-----BEGIN OPENSSH PRIVATE KEY-----
b3BlbnNzaC1rZXktdjEAAAAABG5vbmUAAAAEbm9uZQAAAAAAAAABAAAAMwAAAAtzc2gtZW
QyNTUxOQAAACCM5PbHMICER29FSSQxVg1BlvlSr181HbjPaJLDhYMQ3AAAAJhEonyMRKJ8
jAAAAAtzc2gtZWQyNTUxOQAAACCM5PbHMICER29FSSQxVg1BlvlSr181HbjPaJLDhYMQ3A
AAAED96ea9aUZyIb7JKnzjtYc4Q17zOVoDO3kvWsa6p6n2FYzk9scwgIRHb0VJJDFWDUGW
+VKvXzUduM9oksOFgxDcAAAAEXRlc3RAZXhhbXBsZS50ZXN0AQIDBA==
-----END OPENSSH PRIVATE KEY-----
";
const KEY_PATH: &str = "/api/v1/console/me/ssh_key";

async fn app() -> Option<(TestApp, i64, i64)> {
    let app = TestApp::new(misc::routes()).await?;
    let admin = app
        .create_user("admin@example.test", true, "active")
        .await
        .id;
    let member = app
        .create_user("member@example.test", false, "active")
        .await
        .id;
    Some((app, admin, member))
}

// --- personal SSH key -------------------------------------------------------------

#[tokio::test]
async fn personal_key_routes_allow_any_active_user() {
    let Some((app, ..)) = app().await else { return };
    let body = json!({"data": {"name": "k", "private_key": KEY}});
    app.assert_console_access(Access::Active, Method::GET, KEY_PATH, None)
        .await;
    app.assert_console_access(Access::Active, Method::PUT, KEY_PATH, Some(&body))
        .await;
    app.assert_console_access(Access::Active, Method::DELETE, KEY_PATH, None)
        .await;
    app.assert_console_access(
        Access::Active,
        Method::POST,
        &format!("{KEY_PATH}/validate"),
        Some(&body),
    )
    .await;
}

#[tokio::test]
async fn an_unsaved_key_shows_the_defaults() {
    let Some((app, _, member)) = app().await else {
        return;
    };
    let mut b = browser(&app, member);
    let response = console(&app, &mut b, Method::GET, KEY_PATH, None).await;
    assert_eq!(response.header("cache-control"), Some("no-store"));
    let data = &response.json()["data"];
    assert_eq!(data["saved"], false);
    assert_eq!(data["name"], "My SSH key");
    assert_eq!(data["grants"], json!([]));
    assert!(data["id"].is_null() && data["fingerprint"].is_null());
}

#[tokio::test]
async fn validating_a_key_reports_its_fingerprint_or_why_not() {
    let Some((app, _, member)) = app().await else {
        return;
    };
    let mut b = browser(&app, member);
    let path = format!("{KEY_PATH}/validate");
    let ok = console(
        &app,
        &mut b,
        Method::POST,
        &path,
        Some(json!({"data": {"private_key": KEY}})),
    )
    .await;
    assert_eq!(ok.status(), 200);
    assert_eq!(ok.json()["data"]["valid"], true);
    assert!(
        ok.json()["data"]["fingerprint"]
            .as_str()
            .unwrap()
            .starts_with("SHA256:")
    );
    let public = console(
        &app,
        &mut b,
        Method::POST,
        &path,
        Some(json!({"data": {"private_key": "ssh-ed25519 AAAA x"}})),
    )
    .await;
    assert_eq!(public.status(), 422);
    assert!(public.error_message().unwrap().contains("public key"));
    let blank = console(&app, &mut b, Method::POST, &path, Some(json!({"data": {}}))).await;
    assert_eq!(blank.status(), 422);
}

#[tokio::test]
async fn saving_a_key_stores_it_encrypted_and_never_returns_it() {
    let Some((app, _, member)) = app().await else {
        return;
    };
    let mut b = browser(&app, member);
    let response = console(
        &app,
        &mut b,
        Method::PUT,
        KEY_PATH,
        Some(json!({"data": {"name": " Laptop ", "private_key": KEY}})),
    )
    .await;
    assert_eq!(response.status(), 200, "{}", response.text());
    let text = response.text();
    assert!(!text.contains("PRIVATE KEY") && !text.contains("b3BlbnNzaC1r"));
    let data = response.json()["data"].clone();
    assert_eq!(data["saved"], true);
    assert_eq!(data["name"], "Laptop");
    assert_eq!(data["has_private_key"], true);
    assert!(data["id"].as_str().unwrap().starts_with("sks_"));
    assert!(
        data["public_key"]
            .as_str()
            .unwrap()
            .starts_with("ssh-ed25519 AAAA")
    );
    assert!(data["fingerprint"].as_str().unwrap().starts_with("SHA256:"));
    // The stored private key is an encrypted inline source.
    let (source_type, secret): (String, String) =
        sqlx::query_as("SELECT source_type, secret FROM secret_sources")
            .fetch_one(&app.db.pool)
            .await
            .unwrap();
    assert_eq!(source_type, "control_plane");
    assert!(!secret.contains("OPENSSH"), "stored encrypted");
    // Renaming keeps the key; a second save does not create another row.
    let renamed = console(
        &app,
        &mut b,
        Method::PATCH,
        KEY_PATH,
        Some(json!({"data": {"name": "Desktop"}})),
    )
    .await;
    assert_eq!(renamed.json()["data"]["name"], "Desktop");
    assert_eq!(renamed.json()["data"]["has_private_key"], true);
    assert_eq!(renamed.json()["data"]["id"], data["id"]);
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM ssh_key_secrets")
        .fetch_one(&app.db.pool)
        .await
        .unwrap();
    assert_eq!(rows, 1);
}

#[tokio::test]
async fn saving_validates_the_name_and_requires_a_key_first() {
    let Some((app, _, member)) = app().await else {
        return;
    };
    let mut b = browser(&app, member);
    let none = console(
        &app,
        &mut b,
        Method::PUT,
        KEY_PATH,
        Some(json!({"data": {"name": "x"}})),
    )
    .await;
    assert_eq!(none.status(), 422);
    assert!(none.json()["error"]["details"]["private_key"].is_array());
    let bad_key = console(
        &app,
        &mut b,
        Method::PUT,
        KEY_PATH,
        Some(json!({"data": {"private_key": "nonsense"}})),
    )
    .await;
    assert_eq!(bad_key.status(), 422);
    assert!(!bad_key.text().contains("nonsense"));
    let long = "n".repeat(101);
    let response = console(
        &app,
        &mut b,
        Method::PUT,
        KEY_PATH,
        Some(json!({"data": {"name": long, "private_key": KEY}})),
    )
    .await;
    assert!(response.json()["error"]["details"]["name"].is_array());
    let blank = console(
        &app,
        &mut b,
        Method::PUT,
        KEY_PATH,
        Some(json!({"data": {"name": " ", "private_key": KEY}})),
    )
    .await;
    assert_eq!(blank.status(), 422);
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM ssh_key_secrets")
        .fetch_one(&app.db.pool)
        .await
        .unwrap();
    assert_eq!(rows, 0);
}

#[tokio::test]
async fn the_key_is_granted_to_the_owners_principals_and_removed_with_it() {
    let Some((app, admin, member)) = app().await else {
        return;
    };
    let principal = app.create_principal("console_user", "member", admin).await;
    sqlx::query("UPDATE principals SET console_user_id = $1 WHERE id = $2")
        .bind(member)
        .bind(principal)
        .execute(&app.db.pool)
        .await
        .unwrap();
    let stranger = app
        .create_principal("console_user", "stranger", admin)
        .await;
    let mut b = browser(&app, member);
    let version = |id: i64| {
        let pool = app.db.pool.clone();
        async move {
            sqlx::query_scalar::<_, i64>(
                "SELECT sync_config_cache_version FROM principals WHERE id = $1",
            )
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap()
        }
    };
    let response = console(
        &app,
        &mut b,
        Method::PUT,
        KEY_PATH,
        Some(json!({"data": {"private_key": KEY}})),
    )
    .await;
    let grants = &response.json()["data"]["grants"];
    assert_eq!(grants.as_array().unwrap().len(), 1, "{grants}");
    assert_eq!(grants[0]["grantee_type"], "principal");
    assert_eq!(grants[0]["grantee_id"], oid("prn", principal));
    assert_eq!(grants[0]["label"], "member");
    assert_eq!(grants[0]["principal_kind"], "console_user");
    assert_eq!(version(principal).await, 1);
    assert_eq!(version(stranger).await, 0);
    let created_by: i64 = sqlx::query_scalar("SELECT created_by_id FROM grants")
        .fetch_one(&app.db.pool)
        .await
        .unwrap();
    assert_eq!(created_by, member);
    // Replacing the private key invalidates the grantee's config again,
    // without duplicating the grant.
    let again = console(
        &app,
        &mut b,
        Method::PUT,
        KEY_PATH,
        Some(json!({"data": {"private_key": KEY}})),
    )
    .await;
    assert_eq!(again.json()["data"]["grants"].as_array().unwrap().len(), 1);
    assert_eq!(version(principal).await, 2);
    // Deleting removes the source, the grants, and the key, and invalidates.
    assert_eq!(
        console(&app, &mut b, Method::DELETE, KEY_PATH, None)
            .await
            .status(),
        204
    );
    for table in ["ssh_key_secrets", "secret_sources", "grants"] {
        let rows: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM {table}"))
            .fetch_one(&app.db.pool)
            .await
            .unwrap();
        assert_eq!(rows, 0, "{table}");
    }
    assert_eq!(version(principal).await, 3);
    assert_eq!(
        console(&app, &mut b, Method::DELETE, KEY_PATH, None)
            .await
            .status(),
        204
    );
}

#[tokio::test]
async fn keys_belong_to_their_owner() {
    let Some((app, admin, member)) = app().await else {
        return;
    };
    let mut m = browser(&app, member);
    console(
        &app,
        &mut m,
        Method::PUT,
        KEY_PATH,
        Some(json!({"data": {"private_key": KEY}})),
    )
    .await;
    let mut a = browser(&app, admin);
    let theirs = console(&app, &mut a, Method::GET, KEY_PATH, None).await;
    assert_eq!(theirs.json()["data"]["saved"], false);
    assert_eq!(
        console(&app, &mut a, Method::DELETE, KEY_PATH, None)
            .await
            .status(),
        204
    );
    let mine = console(&app, &mut m, Method::GET, KEY_PATH, None).await;
    assert_eq!(
        mine.json()["data"]["saved"],
        true,
        "another user's delete leaves the key"
    );
}

// --- API keys ----------------------------------------------------------------------------

#[tokio::test]
async fn api_key_routes_are_for_admin_bearers() {
    let Some((app, ..)) = app().await else { return };
    app.assert_bearer_access(Method::GET, "/api/v1/api_keys", None)
        .await;
    app.assert_bearer_access(
        Method::POST,
        "/api/v1/api_keys",
        Some(&json!({"data": {"name": "ci"}})),
    )
    .await;
    app.assert_bearer_access(Method::GET, "/api/v1/api_keys/ak_x", None)
        .await;
    app.assert_bearer_access(Method::DELETE, "/api/v1/api_keys/ak_x", None)
        .await;
}

#[tokio::test]
async fn a_browser_session_cannot_mint_or_revoke_keys_or_touch_proxies() {
    let Some((app, admin, _)) = app().await else {
        return;
    };
    let mut b = browser(&app, admin);
    for (method, path) in [
        (Method::POST, "/api/v1/api_keys"),
        (Method::DELETE, "/api/v1/api_keys/ak_x"),
        (Method::GET, "/api/v1/api_keys"),
        (Method::POST, "/api/v1/proxies"),
        (Method::PATCH, "/api/v1/proxies/prx_x"),
        (Method::DELETE, "/api/v1/proxies/prx_x"),
    ] {
        let response = console(
            &app,
            &mut b,
            method.clone(),
            path,
            Some(json!({"data": {"name": "browser"}})),
        )
        .await;
        assert_eq!(response.status(), 401, "{method} {path}");
        assert!(!response.text().contains("iak_"));
    }
    for table in ["api_keys", "proxies"] {
        let rows: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM {table}"))
            .fetch_one(&app.db.pool)
            .await
            .unwrap();
        assert_eq!(rows, 0, "{table}");
    }
}

#[tokio::test]
async fn api_keys_are_created_listed_and_revoked_by_their_owner() {
    let Some((app, admin, _)) = app().await else {
        return;
    };
    let other_admin = app
        .create_user("admin2@example.test", true, "active")
        .await
        .id;
    let token = app.create_api_key(admin).await;
    let other_token = app.create_api_key(other_admin).await;
    let created = app
        .post("/api/v1/api_keys")
        .bearer(&token)
        .json(&json!({"data": {"name": "ci"}}))
        .send()
        .await;
    assert_eq!(created.status(), 201, "{}", created.text());
    let data = created.json()["data"].clone();
    let secret = data["token"].as_str().unwrap().to_owned();
    assert!(secret.starts_with("iak_") && secret.len() == 68);
    // The new key authenticates, and only its hash is stored.
    assert_eq!(
        app.get("/api/v1/api_keys")
            .bearer(&secret)
            .send()
            .await
            .status(),
        200
    );
    let stored: i64 = sqlx::query_scalar("SELECT count(*) FROM api_keys WHERE token_hash = $1")
        .bind(centaur_console_auth::bearer::hash_token(&secret))
        .fetch_one(&app.db.pool)
        .await
        .unwrap();
    assert_eq!(stored, 1);
    let plaintext: i64 = sqlx::query_scalar("SELECT count(*) FROM api_keys WHERE token_hash = $1")
        .bind(&secret)
        .fetch_one(&app.db.pool)
        .await
        .unwrap();
    assert_eq!(plaintext, 0);
    // Lists show only the caller's keys, never the token.
    let list = app.get("/api/v1/api_keys").bearer(&token).send().await;
    let body = list.json();
    assert_eq!(body["data"].as_array().unwrap().len(), 2);
    assert_eq!(body["meta"]["total"], 2);
    assert!(!list.text().contains(&secret));
    let id = data["id"].as_str().unwrap();
    let show = app
        .get(&format!("/api/v1/api_keys/{id}"))
        .bearer(&token)
        .send()
        .await;
    assert_eq!(show.json()["data"]["name"], "ci");
    assert!(show.json()["data"].get("token").is_none());
    // Another admin cannot see or revoke it.
    assert_eq!(
        app.get(&format!("/api/v1/api_keys/{id}"))
            .bearer(&other_token)
            .send()
            .await
            .status(),
        404
    );
    assert_eq!(
        app.delete(&format!("/api/v1/api_keys/{id}"))
            .bearer(&other_token)
            .send()
            .await
            .status(),
        404
    );
    // The key in use cannot be revoked; another of the caller's keys can.
    let in_use_id: i64 = sqlx::query_scalar("SELECT id FROM api_keys WHERE token_hash = $1")
        .bind(centaur_console_auth::bearer::hash_token(&token))
        .fetch_one(&app.db.pool)
        .await
        .unwrap();
    let in_use = oid("ak", in_use_id);
    let refused = app
        .delete(&format!("/api/v1/api_keys/{in_use}"))
        .bearer(&token)
        .send()
        .await;
    assert_eq!(refused.status(), 422);
    assert_eq!(
        app.delete(&format!("/api/v1/api_keys/{id}"))
            .bearer(&token)
            .send()
            .await
            .status(),
        204
    );
    assert_eq!(
        app.get("/api/v1/api_keys")
            .bearer(&secret)
            .send()
            .await
            .status(),
        401
    );
    assert_eq!(
        app.get(&format!("/api/v1/api_keys/{id}"))
            .bearer(&token)
            .send()
            .await
            .status(),
        404
    );
}

#[tokio::test]
async fn api_keys_need_a_name() {
    let Some((app, admin, _)) = app().await else {
        return;
    };
    let token = app.create_api_key(admin).await;
    let response = app
        .post("/api/v1/api_keys")
        .bearer(&token)
        .json(&json!({"data": {"name": "  "}}))
        .send()
        .await;
    assert_eq!(response.status(), 422);
    assert!(response.json()["error"]["details"]["name"].is_array());
    assert_eq!(
        app.post("/api/v1/api_keys")
            .bearer(&token)
            .json(&json!({"name": "x"}))
            .send()
            .await
            .status(),
        400
    );
}

// --- proxies -------------------------------------------------------------------------------

#[tokio::test]
async fn proxy_routes_are_for_admin_bearers() {
    let Some((app, ..)) = app().await else { return };
    app.assert_bearer_access(Method::GET, "/api/v1/proxies", None)
        .await;
    app.assert_bearer_access(
        Method::POST,
        "/api/v1/proxies",
        Some(&json!({"data": {"name": "p"}})),
    )
    .await;
    app.assert_bearer_access(
        Method::PATCH,
        "/api/v1/proxies/prx_x",
        Some(&json!({"data": {}})),
    )
    .await;
    app.assert_bearer_access(Method::DELETE, "/api/v1/proxies/prx_x", None)
        .await;
}

#[tokio::test]
async fn proxies_are_registered_assigned_filtered_and_deleted() {
    let Some((app, admin, _)) = app().await else {
        return;
    };
    let token = app.create_api_key(admin).await;
    let principal = app.create_principal("unknown", "agent", admin).await;
    let principal_oid = oid("prn", principal);
    let created = app
        .post("/api/v1/proxies")
        .bearer(&token)
        .json(&json!({"data": {"name": "edge-1", "labels": {"zone": "a", "tier": "web"}}}))
        .send()
        .await;
    assert_eq!(created.status(), 201, "{}", created.text());
    let data = created.json()["data"].clone();
    let secret = data["token"].as_str().unwrap();
    assert!(secret.starts_with("iprx_") && secret.len() == 69);
    assert_eq!(
        (data["status"].as_str(), data["principal_id"].is_null()),
        (Some("unassigned"), true)
    );
    assert_eq!(data["labels"], json!({"zone": "a", "tier": "web"}));
    let hash: String =
        sqlx::query_scalar("SELECT bearer_token_hash FROM proxies WHERE name = 'edge-1'")
            .fetch_one(&app.db.pool)
            .await
            .unwrap();
    assert_eq!(hash, centaur_console_auth::bearer::hash_token(secret));
    let id = data["id"].as_str().unwrap().to_owned();
    // Assign: the time is stamped; unrelated fields stay.
    let path = format!("/api/v1/proxies/{id}");
    let assigned = app
        .patch(&path)
        .bearer(&token)
        .json(&json!({"data": {"principal_id": principal_oid}}))
        .send()
        .await;
    assert_eq!(assigned.status(), 200, "{}", assigned.text());
    let data = &assigned.json()["data"];
    assert_eq!(
        (data["status"].as_str(), data["principal_id"].as_str()),
        (Some("assigned"), Some(principal_oid.as_str()))
    );
    assert!(data["principal_assigned_at"].is_string());
    assert_eq!(data["labels"], json!({"zone": "a", "tier": "web"}));
    assert_eq!(data["name"], "edge-1");
    assert!(data.get("token").is_none());
    // Filters: by principal and by label containment.
    let other = app
        .post("/api/v1/proxies")
        .bearer(&token)
        .json(&json!({"data": {"name": "edge-2", "labels": {"zone": "b"}}}))
        .send()
        .await;
    assert_eq!(other.status(), 201);
    let names = |response: &centaur_console_api::testing::TestResponse| -> Vec<Value> {
        response.json()["data"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["name"].clone())
            .collect()
    };
    let by_principal = app
        .get(&format!("/api/v1/proxies?principal_id={principal_oid}"))
        .bearer(&token)
        .send()
        .await;
    assert_eq!(names(&by_principal), [json!("edge-1")]);
    let by_label = app
        .get("/api/v1/proxies?labels[zone]=b")
        .bearer(&token)
        .send()
        .await;
    assert_eq!(names(&by_label), [json!("edge-2")]);
    let both = app
        .get("/api/v1/proxies?labels%5Bzone%5D=a&labels%5Btier%5D=web")
        .bearer(&token)
        .send()
        .await;
    assert_eq!(names(&both), [json!("edge-1")]);
    let paged = app
        .get("/api/v1/proxies?limit=1&page=2")
        .bearer(&token)
        .send()
        .await;
    assert_eq!(names(&paged), [json!("edge-2")]);
    assert_eq!(paged.json()["meta"]["total_pages"], 2);
    assert_eq!(
        app.get(&format!(
            "/api/v1/proxies?principal_id={}",
            oid("prn", 9999)
        ))
        .bearer(&token)
        .send()
        .await
        .status(),
        404
    );
    // Unassign with null; a replace-all PUT of labels.
    let cleared = app
        .put(&path)
        .bearer(&token)
        .json(&json!({"data": {"principal_id": null, "labels": {"zone": "c"}}}))
        .send()
        .await;
    let data = &cleared.json()["data"];
    assert_eq!(data["status"], "unassigned");
    assert!(data["principal_assigned_at"].is_null());
    assert_eq!(data["labels"], json!({"zone": "c"}));
    assert_eq!(
        app.get(&path).bearer(&token).send().await.json()["data"]["name"],
        "edge-1"
    );
    assert_eq!(app.delete(&path).bearer(&token).send().await.status(), 204);
    assert_eq!(app.get(&path).bearer(&token).send().await.status(), 404);
}

#[tokio::test]
async fn proxy_validation_and_unknown_principals() {
    let Some((app, admin, _)) = app().await else {
        return;
    };
    let token = app.create_api_key(admin).await;
    let blank = app
        .post("/api/v1/proxies")
        .bearer(&token)
        .json(&json!({"data": {"name": " "}}))
        .send()
        .await;
    assert_eq!(blank.status(), 422);
    let labels = app
        .post("/api/v1/proxies")
        .bearer(&token)
        .json(&json!({"data": {"name": "p", "labels": {"n": 1}}}))
        .send()
        .await;
    assert_eq!(labels.status(), 422);
    assert!(labels.json()["error"]["details"]["labels"].is_array());
    let unknown = app
        .post("/api/v1/proxies")
        .bearer(&token)
        .json(&json!({"data": {"name": "p", "principal_id": oid("prn", 4242)}}))
        .send()
        .await;
    assert_eq!(unknown.status(), 404);
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM proxies")
        .fetch_one(&app.db.pool)
        .await
        .unwrap();
    assert_eq!(rows, 0);
    let created = app
        .post("/api/v1/proxies")
        .bearer(&token)
        .json(&json!({"data": {"name": "p"}}))
        .send()
        .await;
    let path = format!(
        "/api/v1/proxies/{}",
        created.json()["data"]["id"].as_str().unwrap()
    );
    let rename = app
        .patch(&path)
        .bearer(&token)
        .json(&json!({"data": {"name": ""}}))
        .send()
        .await;
    assert_eq!(rename.status(), 422);
    assert_eq!(
        app.get("/api/v1/proxies/nonsense")
            .bearer(&token)
            .send()
            .await
            .status(),
        404
    );
}
