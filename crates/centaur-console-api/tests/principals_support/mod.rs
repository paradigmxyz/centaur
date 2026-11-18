//! Shared fixtures for the principals tests.
#![allow(dead_code)]

use centaur_console_api::{
    principals,
    testing::{TestApp, TestResponse, TestUser},
};
use http::Method;
use serde_json::Value;

pub struct Fx {
    pub app: TestApp,
    pub admin: TestUser,
    pub key: String,
}

impl Fx {
    pub async fn new() -> Option<Self> {
        Self::with_env(&[]).await
    }

    pub async fn with_env(env: &[(&str, &str)]) -> Option<Self> {
        let app = TestApp::with_env(principals::routes(), env).await?;
        let admin = app.create_user("admin@example.test", true, "active").await;
        let key = app.create_api_key(admin.id).await;
        Some(Self { app, admin, key })
    }

    /// A request to `/api/v1{path}` with the admin's API key.
    pub async fn bearer(&self, method: Method, path: &str, body: Option<Value>) -> TestResponse {
        let mut request = self
            .app
            .request(method, &format!("/api/v1{path}"))
            .bearer(&self.key);
        if let Some(body) = &body {
            request = request.json(body);
        }
        request.send().await
    }

    /// A request to `/api/v1/console{path}` from a signed-in admin browser.
    pub async fn console(&self, method: Method, path: &str, body: Option<Value>) -> TestResponse {
        let mut browser = self.app.client();
        browser.sign_in(self.admin.id);
        let mut request = self
            .app
            .request(method, &format!("/api/v1/console{path}"))
            .csrf(&browser);
        if let Some(body) = &body {
            request = request.json(body);
        }
        browser.send(request).await
    }

    /// A request to `/api/v1/sandbox/admin{path}` with a sandbox token.
    pub async fn sandbox(
        &self,
        token: &str,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> TestResponse {
        let mut request = self
            .app
            .request(method, &format!("/api/v1/sandbox/admin{path}"))
            .bearer(token);
        if let Some(body) = &body {
            request = request.json(body);
        }
        request.send().await
    }

    pub async fn exec(&self, sql: &str) {
        sqlx::query(sql)
            .execute(&self.app.db.pool)
            .await
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
    }

    pub async fn scalar_i64(&self, sql: &str) -> i64 {
        sqlx::query_scalar(sql)
            .fetch_one(&self.app.db.pool)
            .await
            .unwrap_or_else(|error| panic!("{sql}: {error}"))
    }

    pub async fn scalar_string(&self, sql: &str) -> Option<String> {
        sqlx::query_scalar(sql)
            .fetch_one(&self.app.db.pool)
            .await
            .unwrap_or_else(|error| panic!("{sql}: {error}"))
    }

    pub async fn scalar_bool(&self, sql: &str) -> bool {
        sqlx::query_scalar(sql)
            .fetch_one(&self.app.db.pool)
            .await
            .unwrap_or_else(|error| panic!("{sql}: {error}"))
    }

    /// `sync_config_cache_version` of the principal with this oid.
    pub async fn version(&self, principal_oid: &str) -> i64 {
        let id = centaur_console_db::decode_oid("prn", principal_oid).expect("principal oid");
        self.scalar_i64(&format!(
            "SELECT sync_config_cache_version FROM principals WHERE id = {id}"
        ))
        .await
    }

    /// Creates a principal over the bearer API; returns its oid.
    pub async fn principal(&self, foreign_id: &str, name: &str) -> String {
        let response = self
            .bearer(
                Method::POST,
                "/principals",
                Some(serde_json::json!({"data": {"foreign_id": foreign_id, "name": name}})),
            )
            .await;
        assert_eq!(response.status(), 201, "{}", response.text());
        response.json()["data"]["id"].as_str().unwrap().to_owned()
    }

    /// Creates a role over the bearer API; returns its oid.
    pub async fn role(&self, foreign_id: &str) -> String {
        let response = self
            .bearer(
                Method::POST,
                "/roles",
                Some(serde_json::json!({"data": {"foreign_id": foreign_id, "name": foreign_id}})),
            )
            .await;
        assert_eq!(response.status(), 201, "{}", response.text());
        response.json()["data"]["id"].as_str().unwrap().to_owned()
    }

    /// A static secret; returns its oid.
    pub async fn static_secret(&self, foreign_id: &str) -> String {
        let id = self
            .scalar_i64(&format!(
                "INSERT INTO static_secrets (foreign_id, name, created_at, updated_at) \
                 VALUES ('{foreign_id}', '{foreign_id} name', now(), now()) RETURNING id"
            ))
            .await;
        centaur_console_db::oid("ssr", id)
    }

    /// A Slack DM principal for the admin, a proxy for it, and the token a
    /// sandbox running as it presents. The bot's home workspace is `team`.
    pub async fn admin_dm_token(&self, team: &str) -> String {
        self.exec(&format!(
            "INSERT INTO slack_bot_channels (bot_user_id, channel_id, name, team_id, created_at, updated_at) \
             VALUES ('UBOT00001', 'C00000001', 'general', '{team}', now(), now()) \
             ON CONFLICT DO NOTHING"
        ))
        .await;
        let id = self
            .scalar_i64(&format!(
                "INSERT INTO principals (kind, name, foreign_id, slack_user_id, slack_team_id, \
                 console_user_id, created_by_id, created_at, updated_at) \
                 VALUES ('slack_dm', 'admin dm', 'slack-dm-admin', 'U00ADMIN01', '{team}', {admin}, {admin}, now(), now()) \
                 RETURNING id",
                admin = self.admin.id
            ))
            .await;
        let proxy = self.app.create_proxy("sandbox-admin-dm", id).await;
        self.app.sandbox_token(proxy, id).await
    }
}
