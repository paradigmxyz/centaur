//! The sandbox admin surface: an admin's Slack DM sandbox managing
//! principals, roles, grants, permissions, and the Slack Connect allowlist.

mod principals_support;

use std::sync::{Arc, Mutex};

use axum::{Json, Router, extract::State, http::HeaderMap, routing::get};
use http::Method;
use principals_support::Fx;
use serde_json::{Value, json};

const TEAM: &str = "T00HOME001";

fn data(response: &centaur_console_api::testing::TestResponse) -> Value {
    response.json()["data"].clone()
}

#[tokio::test]
async fn only_an_active_admins_home_workspace_dm_is_an_admin() {
    let Some(fx) = Fx::new().await else { return };
    let admin_dm = fx.admin_dm_token(TEAM).await;
    let ok = fx.sandbox(&admin_dm, Method::GET, "/roles", None).await;
    assert_eq!(ok.status(), 200, "{}", ok.text());

    let missing = fx.app.get("/api/v1/sandbox/admin/roles").send().await;
    assert_eq!(missing.status(), 401);
    let bearer_key = fx.sandbox(&fx.key, Method::GET, "/roles", None).await;
    assert!(matches!(bearer_key.status(), 401 | 403));

    let member = fx
        .app
        .create_user("member@example.test", false, "active")
        .await;
    let channel = fx
        .app
        .create_principal("slack_channel", "chan", fx.admin.id)
        .await;
    let member_dm = fx
        .scalar_i64(&format!(
            "INSERT INTO principals (kind, foreign_id, slack_team_id, console_user_id, created_by_id, created_at, updated_at) \
             VALUES ('slack_dm', 'member-dm', '{TEAM}', {}, {}, now(), now()) RETURNING id",
            member.id, fx.admin.id
        ))
        .await;
    let foreign_dm = fx
        .scalar_i64(&format!(
            "INSERT INTO principals (kind, foreign_id, slack_team_id, console_user_id, created_by_id, created_at, updated_at) \
             VALUES ('slack_dm', 'foreign-dm', 'T00OTHER01', {}, {}, now(), now()) RETURNING id",
            fx.admin.id, fx.admin.id
        ))
        .await;
    for (name, principal) in [
        ("channel", channel),
        ("member dm", member_dm),
        ("other workspace", foreign_dm),
    ] {
        let proxy = fx.app.create_proxy(name, principal).await;
        let token = fx.app.sandbox_token(proxy, principal).await;
        let response = fx.sandbox(&token, Method::GET, "/roles", None).await;
        assert_eq!(response.status(), 403, "{name}: {}", response.text());
    }
    // A disabled admin loses it.
    fx.exec(&format!(
        "UPDATE users SET status = 'disabled' WHERE id = {}",
        fx.admin.id
    ))
    .await;
    assert_eq!(
        fx.sandbox(&admin_dm, Method::GET, "/roles", None)
            .await
            .status(),
        403
    );
}

#[tokio::test]
async fn an_admin_dm_manages_roles_and_grants_of_principals() {
    let Some(fx) = Fx::new().await else { return };
    let token = fx.admin_dm_token(TEAM).await;
    let role = fx.role("ops").await;
    let target = fx.principal("target", "Target").await;
    let secret = fx.static_secret("sandbox-secret").await;

    let roles = fx.sandbox(&token, Method::GET, "/roles", None).await.json();
    assert_eq!(roles["data"][0]["foreign_id"], "ops");
    assert_eq!(roles["meta"]["total"], 1);
    let looked_up = fx
        .sandbox(&token, Method::GET, "/roles/lookup/ops", None)
        .await;
    assert_eq!(data(&looked_up)["id"], role);
    assert_eq!(
        data(
            &fx.sandbox(&token, Method::GET, &format!("/roles/{role}"), None)
                .await
        )["foreign_id"],
        "ops"
    );
    assert_eq!(
        data(
            &fx.sandbox(&token, Method::GET, "/principals/lookup/target", None)
                .await
        )["id"],
        target
    );
    assert_eq!(
        data(
            &fx.sandbox(&token, Method::GET, &format!("/principals/{target}"), None)
                .await
        )["id"],
        target
    );

    let before = fx.version(&target).await;
    let assigned = fx
        .sandbox(
            &token,
            Method::POST,
            &format!("/principals/{target}/roles"),
            Some(json!({"data": {"role_id": role}})),
        )
        .await;
    assert_eq!(assigned.status(), 201, "{}", assigned.text());
    assert_eq!(fx.version(&target).await, before + 1);
    let listed = fx
        .sandbox(
            &token,
            Method::GET,
            &format!("/principals/{target}/roles"),
            None,
        )
        .await;
    assert_eq!(data(&listed)[0]["id"], role);

    let granted = fx
        .sandbox(
            &token,
            Method::POST,
            "/grants",
            Some(json!({"data": {"role_id": role, "static_secret_id": secret}})),
        )
        .await;
    assert_eq!(granted.status(), 201, "{}", granted.text());
    // The grant was made by the admin behind the DM.
    assert_eq!(
        fx.scalar_i64("SELECT created_by_id FROM grants").await,
        fx.admin.id
    );
    let role_grants = fx
        .sandbox(&token, Method::GET, &format!("/roles/{role}/grants"), None)
        .await
        .json();
    assert_eq!(role_grants["data"][0]["static_secret_id"], secret);
    let effective = fx
        .sandbox(
            &token,
            Method::GET,
            &format!("/principals/{target}/effective_grants"),
            None,
        )
        .await
        .json();
    assert_eq!(effective["data"][0]["foreign_id"], "sandbox-secret");
    assert_eq!(effective["data"][0]["roles"][0]["foreign_id"], "ops");
    let principal_grants = fx
        .sandbox(
            &token,
            Method::GET,
            &format!("/principals/{target}/grants"),
            None,
        )
        .await;
    assert_eq!(data(&principal_grants), json!([]));

    let grant_id = data(&granted)["id"].as_str().unwrap().to_owned();
    assert_eq!(
        fx.sandbox(&token, Method::DELETE, &format!("/grants/{grant_id}"), None)
            .await
            .status(),
        204
    );
    let removed = fx
        .sandbox(
            &token,
            Method::DELETE,
            &format!("/principals/{target}/roles/{role}"),
            None,
        )
        .await;
    assert_eq!(removed.status(), 204);

    // Permissions on principals; nothing beyond the sandbox surface.
    let permission = fx
        .sandbox(
            &token,
            Method::POST,
            &format!("/principals/{target}/slack_channel_permissions"),
            Some(json!({"data": {"channel_id": "C00000001", "history_enabled": true}})),
        )
        .await;
    assert_eq!(permission.status(), 201);
    let deleted = fx
        .sandbox(
            &token,
            Method::DELETE,
            &format!("/principals/{target}/slack_channel_permissions/C00000001"),
            None,
        )
        .await;
    assert_eq!(deleted.status(), 204);
    for (method, path) in [
        (Method::DELETE, format!("/principals/{target}")),
        (Method::PUT, format!("/principals/{target}")),
        (Method::POST, "/roles".to_owned()),
        (Method::DELETE, format!("/roles/{role}")),
        (
            Method::GET,
            format!("/principals/{target}/effective_config"),
        ),
    ] {
        let response = fx
            .sandbox(&token, method.clone(), &path, Some(json!({"data": {}})))
            .await;
        assert!(
            matches!(response.status(), 404 | 405),
            "{method} {path}: {}",
            response.status()
        );
    }
}

#[tokio::test]
async fn principals_can_be_found_by_slack_channel() {
    let Some(fx) = Fx::new().await else { return };
    let token = fx.admin_dm_token(TEAM).await;
    fx.bearer(Method::POST, "/principals", Some(json!({"data": {"foreign_id": "c1", "kind": "slack_channel", "slack_channel_id": "C00000001"}}))).await;
    fx.bearer(Method::POST, "/principals", Some(json!({"data": {"foreign_id": "c2", "kind": "slack_channel", "slack_channel_id": "C00000002"}}))).await;
    let found = fx
        .sandbox(
            &token,
            Method::GET,
            "/principals?slack_channel_id=c00000001",
            None,
        )
        .await
        .json();
    assert_eq!(found["data"].as_array().unwrap().len(), 1);
    assert_eq!(found["data"][0]["foreign_id"], "c1");
    assert!(found.get("meta").is_none());
    let all = fx
        .sandbox(&token, Method::GET, "/principals?limit=1", None)
        .await
        .json();
    assert_eq!(all["data"].as_array().unwrap().len(), 1);
    assert!(all["meta"]["total"].as_i64().unwrap() >= 3);
}

#[tokio::test]
async fn principals_are_created_for_channels_dms_and_workflows() {
    let Some(fx) = Fx::new().await else { return };
    let token = fx.admin_dm_token(TEAM).await;

    let channel = fx
        .sandbox(
            &token,
            Method::POST,
            "/principals",
            Some(json!({"data": {"kind": "slack_channel", "slack_channel_id": " c00000009 "}})),
        )
        .await;
    assert_eq!(channel.status(), 201, "{}", channel.text());
    let channel = data(&channel);
    assert_eq!(channel["foreign_id"], "slack-channel-c00000009");
    assert_eq!(channel["name"], "Slack Channel C00000009");
    assert_eq!(channel["kind"], "slack_channel");
    assert_eq!(
        channel["slack_channel_permissions"],
        json!([{"channel_id": "C00000009", "upload_enabled": true, "download_enabled": true, "history_enabled": true}])
    );
    let duplicate = fx
        .sandbox(
            &token,
            Method::POST,
            "/principals",
            Some(json!({"data": {"kind": "slack_channel", "slack_channel_id": "C00000009"}})),
        )
        .await;
    assert_eq!(duplicate.status(), 422);
    assert!(duplicate.json()["error"]["details"]["foreign_id"].is_array());
    assert_eq!(
        fx.scalar_i64("SELECT COUNT(*) FROM slack_channel_permissions")
            .await,
        1
    );

    let dm = fx
        .sandbox(
            &token,
            Method::POST,
            "/principals",
            Some(json!({"data": {"kind": "slack_dm", "slack_user_id": "u00000007"}})),
        )
        .await;
    assert_eq!(dm.status(), 201, "{}", dm.text());
    assert_eq!(data(&dm)["foreign_id"], "slack-user-t00home001-u00000007");
    assert_eq!(
        data(&dm)["name"],
        format!("Slack User U00000007 (team {TEAM})")
    );
    assert_eq!(data(&dm)["slack_user_id"], "U00000007");
    assert_eq!(
        fx.scalar_string("SELECT slack_team_id FROM principals WHERE foreign_id = 'slack-user-t00home001-u00000007'").await.as_deref(),
        Some(TEAM)
    );

    let workflow = fx
        .sandbox(
            &token,
            Method::POST,
            "/principals",
            Some(json!({"data": {"kind": "workflow", "workflow_name": "Daily Report #1"}})),
        )
        .await;
    assert_eq!(workflow.status(), 201, "{}", workflow.text());
    assert_eq!(data(&workflow)["foreign_id"], "workflow-daily-report-1");
    assert_eq!(data(&workflow)["name"], "Workflow Daily Report #1");
    assert_eq!(data(&workflow)["labels"]["managed-by"], "centaur");
    assert_eq!(
        data(&workflow)["labels"]["workflow_name"],
        "Daily Report #1"
    );

    for body in [
        json!({"data": {"kind": "slack_channel", "slack_channel_id": "U00000001"}}),
        json!({"data": {"kind": "workflow", "workflow_name": "  "}}),
        json!({"data": {"kind": "workflow"}}),
        json!({"data": {"kind": "teapot"}}),
        json!({"data": {}}),
    ] {
        let response = fx
            .sandbox(&token, Method::POST, "/principals", Some(body.clone()))
            .await;
        assert_eq!(response.status(), 400, "{body}: {}", response.text());
    }
    let bad_dm = fx
        .sandbox(
            &token,
            Method::POST,
            "/principals",
            Some(json!({"data": {"kind": "slack_dm", "slack_user_id": "nope"}})),
        )
        .await;
    assert_eq!(bad_dm.status(), 422);
}

#[tokio::test]
async fn sandbox_access_changes_only_the_capabilities_given() {
    let Some(fx) = Fx::new().await else { return };
    let token = fx.admin_dm_token(TEAM).await;
    let target = fx.principal("access", "Access").await;
    let path = format!("/principals/{target}/sandbox_access");
    let before = fx.version(&target).await;
    let response = fx
        .sandbox(&token, Method::PATCH, &path, Some(json!({"data": {"sandbox_sessions_read_enabled": true, "sandbox_repo_cache": "none"}})))
        .await;
    assert_eq!(response.status(), 200, "{}", response.text());
    let principal = data(&response);
    assert_eq!(principal["sandbox_sessions_read_enabled"], true);
    assert_eq!(principal["sandbox_repo_cache"], "none");
    assert_eq!(principal["labels"]["centaur.sandbox_repo_cache"], "none");
    assert_eq!(
        principal["sandbox_observability_enabled"], true,
        "left alone"
    );
    assert_eq!(fx.version(&target).await, before + 1);

    for (body, status) in [
        (json!({"data": {}}), 400),
        (json!({"data": {"other": true}}), 400),
        (
            json!({"data": {"sandbox_sessions_read_enabled": "yes"}}),
            400,
        ),
        (json!({"data": {"sandbox_repo_cache": true}}), 400),
        (json!({"data": {"sandbox_repo_cache": "most"}}), 422),
    ] {
        let response = fx
            .sandbox(&token, Method::PATCH, &path, Some(body.clone()))
            .await;
        assert_eq!(response.status(), status, "{body}: {}", response.text());
    }
    let missing = fx
        .sandbox(
            &token,
            Method::PATCH,
            "/principals/prn_unknown/sandbox_access",
            Some(json!({"data": {"sandbox_sessions_read_enabled": true}})),
        )
        .await;
    assert_eq!(missing.status(), 404);
}

// --- the Slack Connect allowlist, kept in api-rs ------------------------------------

#[derive(Clone, Default)]
struct ApiRs {
    ids: Arc<Mutex<Vec<String>>>,
    authorization: Arc<Mutex<Vec<String>>>,
}

async fn api_rs(initial: &[&str]) -> (String, ApiRs) {
    let state = ApiRs {
        ids: Arc::new(Mutex::new(
            initial.iter().map(|id| (*id).to_owned()).collect(),
        )),
        ..ApiRs::default()
    };
    async fn read(State(state): State<ApiRs>, headers: HeaderMap) -> Json<Value> {
        note(&state, &headers);
        Json(json!({"slack_external_team_ids": *state.ids.lock().unwrap()}))
    }
    async fn replace(
        State(state): State<ApiRs>,
        headers: HeaderMap,
        Json(body): Json<Value>,
    ) -> (http::StatusCode, Json<Value>) {
        note(&state, &headers);
        let ids: Vec<String> =
            serde_json::from_value(body["slack_external_team_ids"].clone()).unwrap();
        if ids.iter().any(|id| id == "T00REJECT1") {
            return (
                http::StatusCode::BAD_REQUEST,
                Json(json!({"error": "that workspace is not allowed"})),
            );
        }
        *state.ids.lock().unwrap() = ids.clone();
        (
            http::StatusCode::OK,
            Json(json!({"slack_external_team_ids": ids})),
        )
    }
    fn note(state: &ApiRs, headers: &HeaderMap) {
        let value = headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default();
        state.authorization.lock().unwrap().push(value.to_owned());
    }
    let router = Router::new()
        .route("/api/slack/external-team-ids", get(read).put(replace))
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    (url, state)
}

#[tokio::test]
async fn the_slack_connect_allowlist_is_read_and_changed_through_api_rs() {
    let (url, api) = api_rs(&["T00ALLOWED"]).await;
    let Some(fx) = Fx::with_env(&[("CENTAUR_API_URL", &url)]).await else {
        return;
    };
    let token = fx.admin_dm_token(TEAM).await;

    let listed = fx
        .sandbox(&token, Method::GET, "/slack_external_teams", None)
        .await;
    assert_eq!(data(&listed), json!(["T00ALLOWED"]));
    assert!(
        api.authorization.lock().unwrap()[0].starts_with("Bearer "),
        "the console's service token is sent"
    );

    let added = fx
        .sandbox(
            &token,
            Method::POST,
            "/slack_external_teams",
            Some(json!({"data": {"team_id": " e00org0001 "}})),
        )
        .await;
    assert_eq!(data(&added), json!(["T00ALLOWED", "E00ORG0001"]));
    let again = fx
        .sandbox(
            &token,
            Method::POST,
            "/slack_external_teams",
            Some(json!({"data": {"team_id": "E00ORG0001"}})),
        )
        .await;
    assert_eq!(data(&again), json!(["T00ALLOWED", "E00ORG0001"]));
    assert_eq!(*api.ids.lock().unwrap(), ["T00ALLOWED", "E00ORG0001"]);

    let removed = fx
        .sandbox(
            &token,
            Method::DELETE,
            "/slack_external_teams/t00allowed",
            None,
        )
        .await;
    assert_eq!(data(&removed), json!(["E00ORG0001"]));
    let unknown = fx
        .sandbox(
            &token,
            Method::DELETE,
            "/slack_external_teams/T00NOPE001",
            None,
        )
        .await;
    assert_eq!(unknown.status(), 404);

    // api-rs refusing the id is the caller's problem (422, with its words).
    let rejected = fx
        .sandbox(
            &token,
            Method::POST,
            "/slack_external_teams",
            Some(json!({"data": {"team_id": "T00REJECT1"}})),
        )
        .await;
    assert_eq!(rejected.status(), 422);
    assert_eq!(
        rejected.error_message().as_deref(),
        Some("that workspace is not allowed")
    );
    let wrong_type = fx
        .sandbox(
            &token,
            Method::POST,
            "/slack_external_teams",
            Some(json!({"data": {"team_id": 7}})),
        )
        .await;
    assert_eq!(wrong_type.status(), 400);
}

#[tokio::test]
async fn an_unreachable_api_rs_is_a_503() {
    let Some(fx) = Fx::with_env(&[("CENTAUR_API_URL", "http://127.0.0.1:9")]).await else {
        return;
    };
    let token = fx.admin_dm_token(TEAM).await;
    let response = fx
        .sandbox(&token, Method::GET, "/slack_external_teams", None)
        .await;
    assert_eq!(response.status(), 503);
    assert_eq!(
        response.error_message().as_deref(),
        Some("the Centaur API is unavailable")
    );
}
