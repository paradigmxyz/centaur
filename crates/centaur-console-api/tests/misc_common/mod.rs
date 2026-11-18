//! Helpers shared by the misc area's tests.
#![allow(dead_code)]

use axum::http::Method;
use centaur_console_api::testing::{TestApp, TestClient, TestResponse};
use serde_json::Value;

/// A signed-in browser for `user_id`.
pub fn browser(app: &TestApp, user_id: i64) -> TestClient {
    let mut browser = app.client();
    browser.sign_in(user_id);
    browser
}

/// A console API call as `browser`, with its CSRF token.
pub async fn console(
    app: &TestApp,
    browser: &mut TestClient,
    method: Method,
    path: &str,
    body: Option<Value>,
) -> TestResponse {
    let mut request = app.request(method, path);
    if let Some(body) = &body {
        request = request.json(body);
    }
    browser.send(request.csrf(browser)).await
}

/// A sandbox for a principal linked to `user_id` (or unlinked when `None`):
/// returns its entitlement token.
pub async fn sandbox(app: &TestApp, name: &str, user_id: Option<i64>, created_by: i64) -> String {
    let principal = app
        .create_principal(
            if user_id.is_some() {
                "console_user"
            } else {
                "unknown"
            },
            name,
            created_by,
        )
        .await;
    if let Some(user_id) = user_id {
        sqlx::query("UPDATE principals SET console_user_id = $1 WHERE id = $2")
            .bind(user_id)
            .bind(principal)
            .execute(&app.db.pool)
            .await
            .unwrap();
    }
    let proxy = app
        .create_proxy(&format!("sandbox-{name}"), principal)
        .await;
    app.sandbox_token(proxy, principal).await
}

/// A sandbox call with `token`.
pub async fn sandbox_call(
    app: &TestApp,
    token: &str,
    method: Method,
    path: &str,
    body: Option<Value>,
) -> TestResponse {
    let mut request = app.request(method, path).bearer(token);
    if let Some(body) = &body {
        request = request.json(body);
    }
    request.send().await
}
