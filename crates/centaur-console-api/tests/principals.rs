//! Principals, roles, role assignments, grants, Slack channel permissions,
//! and effective config/grants over the bearer and console APIs.

mod principals_support;

use std::cell::Cell;

use axum::http::Method;
use centaur_console_api::auth::Access;
use principals_support::Fx;
use serde_json::{Value, json};

fn data(response: &centaur_console_api::testing::TestResponse) -> Value {
    response.json()["data"].clone()
}

fn ids(list: &Value) -> Vec<String> {
    list.as_array()
        .unwrap()
        .iter()
        .map(|row| row["foreign_id"].as_str().unwrap_or_default().to_owned())
        .collect()
}

// --- who may call -----------------------------------------------------------------

#[tokio::test]
async fn bearer_routes_take_only_an_admins_api_key() {
    let Some(fx) = Fx::new().await else { return };
    let body = json!({"data": {}});
    let routes = [
        (Method::GET, "/principals"),
        (Method::POST, "/principals"),
        (Method::GET, "/principals/prn_unknown"),
        (Method::PUT, "/principals/some-principal"),
        (Method::PATCH, "/principals/some-principal"),
        (Method::GET, "/principals/lookup/some-principal"),
        (Method::GET, "/principals/prn_unknown/effective_config"),
        (Method::GET, "/principals/lookup/x/effective_config"),
        (
            Method::POST,
            "/principals/prn_unknown/slack_channel_permissions",
        ),
        (Method::GET, "/principals/prn_unknown/roles"),
        (Method::POST, "/principals/prn_unknown/roles"),
        (Method::DELETE, "/principals/prn_unknown/roles/role_unknown"),
        (Method::GET, "/principals/prn_unknown/grants"),
        (Method::GET, "/roles"),
        (Method::POST, "/roles"),
        (Method::GET, "/roles/role_unknown"),
        (Method::PUT, "/roles/some-role"),
        (Method::DELETE, "/roles/role_unknown"),
        (Method::GET, "/roles/lookup/some-role"),
        (
            Method::POST,
            "/roles/role_unknown/slack_channel_permissions",
        ),
        (Method::GET, "/roles/role_unknown/grants"),
        (Method::POST, "/grants"),
        (Method::GET, "/grants/grant_unknown"),
        (Method::DELETE, "/grants/grant_unknown"),
    ];
    for (method, path) in routes {
        let body = (method != Method::GET && method != Method::DELETE).then_some(&body);
        fx.app
            .assert_bearer_access(method, &format!("/api/v1{path}"), body)
            .await;
    }
}

#[tokio::test]
async fn console_routes_take_only_an_admins_session() {
    let Some(fx) = Fx::new().await else { return };
    let body = json!({"data": {}});
    let routes = [
        (Method::GET, "/principals"),
        (Method::POST, "/principals"),
        (Method::GET, "/principals/prn_unknown"),
        (Method::PATCH, "/principals/prn_unknown"),
        (Method::DELETE, "/principals/prn_unknown"),
        (Method::GET, "/principals/prn_unknown/effective_grants"),
        (
            Method::PUT,
            "/principals/prn_unknown/slack_channel_permissions",
        ),
        (
            Method::DELETE,
            "/principals/prn_unknown/slack_channel_permissions/C00000001",
        ),
        (Method::GET, "/principals/prn_unknown/roles"),
        (Method::POST, "/principals/prn_unknown/roles"),
        (Method::DELETE, "/principals/prn_unknown/roles/role_unknown"),
        (Method::GET, "/principals/prn_unknown/grants"),
        (Method::GET, "/roles"),
        (Method::POST, "/roles"),
        (Method::PUT, "/roles/role_unknown"),
        (Method::DELETE, "/roles/role_unknown"),
        (Method::PUT, "/roles/role_unknown/slack_channel_permissions"),
        (
            Method::DELETE,
            "/roles/role_unknown/slack_channel_permissions/C00000001",
        ),
        (Method::GET, "/roles/role_unknown/grants"),
        (Method::POST, "/grants"),
        (Method::GET, "/grants/grant_unknown"),
        (Method::DELETE, "/grants/grant_unknown"),
    ];
    for (method, path) in routes {
        let body = (method != Method::GET && method != Method::DELETE).then_some(&body);
        fx.app
            .assert_console_access(
                Access::Admin,
                method,
                &format!("/api/v1/console{path}"),
                body,
            )
            .await;
    }
}

#[tokio::test]
async fn the_bearer_api_does_not_serve_console_only_operations() {
    let Some(fx) = Fx::new().await else { return };
    let principal = fx.principal("p1", "P1").await;
    let role = fx.role("r1").await;
    for (method, path) in [
        (Method::DELETE, format!("/principals/{principal}")),
        (
            Method::GET,
            format!("/principals/{principal}/effective_grants"),
        ),
        (
            Method::PUT,
            format!("/principals/{principal}/slack_channel_permissions"),
        ),
        (
            Method::PUT,
            format!("/roles/{role}/slack_channel_permissions"),
        ),
    ] {
        let response = fx
            .bearer(method.clone(), &path, Some(json!({"data": {}})))
            .await;
        assert!(
            matches!(response.status(), 404 | 405),
            "{method} {path}: {}",
            response.status()
        );
    }
}

// --- principals ---------------------------------------------------------------------

#[tokio::test]
async fn creating_a_principal_applies_defaults_default_roles_and_the_repo_cache_label() {
    let Some(fx) = Fx::new().await else { return };
    fx.exec(&format!(
        "INSERT INTO roles (foreign_id, name, assign_by_default, created_by_id, created_at, updated_at) \
         VALUES ('everyone', 'Everyone', true, {}, now(), now())",
        fx.admin.id
    ))
    .await;
    let response = fx
        .bearer(
            Method::POST,
            "/principals",
            Some(json!({"data": {
                "foreign_id": "alice", "name": "Alice",
                "labels": {"namespace": "x", "team": "infra"},
                "sandbox_repo_cache": "public",
            }})),
        )
        .await;
    assert_eq!(response.status(), 201, "{}", response.text());
    let principal = data(&response);
    assert!(principal["id"].as_str().unwrap().starts_with("prn_"));
    assert_eq!(principal["foreign_id"], "alice");
    assert_eq!(principal["kind"], "unknown");
    assert_eq!(principal["sandbox_repo_cache"], "public");
    assert_eq!(
        principal["labels"],
        json!({"namespace": "x", "team": "infra", "centaur.sandbox_repo_cache": "public"})
    );
    assert_eq!(principal["sandbox_observability_enabled"], true);
    assert_eq!(principal["sandbox_sessions_read_enabled"], false);
    assert_eq!(principal["slack_channel_permissions"], json!([]));
    assert_eq!(principal["roles"][0]["foreign_id"], "everyone");
    assert!(principal["created_at"].as_str().unwrap().ends_with('Z'));
    // The label is stored, not only rendered.
    let stored = fx
        .scalar_string("SELECT labels->>'centaur.sandbox_repo_cache' FROM principals WHERE foreign_id = 'alice'")
        .await;
    assert_eq!(stored.as_deref(), Some("public"));
}

#[tokio::test]
async fn system_defaults_fill_only_the_capabilities_a_request_leaves_out() {
    let Some(fx) = Fx::new().await else { return };
    fx.exec(
        "INSERT INTO system_settings (default_sandbox_repo_cache, default_sandbox_observability_enabled, \
         default_sandbox_sessions_read_enabled, default_sandbox_workflows_read_enabled, \
         default_sandbox_workflows_write_enabled, created_at, updated_at) \
         VALUES ('none', false, true, true, false, now(), now())",
    )
    .await;
    let response = fx
        .bearer(
            Method::POST,
            "/principals",
            Some(json!({"data": {"foreign_id": "bob", "sandbox_workflows_read_enabled": false}})),
        )
        .await;
    assert_eq!(response.status(), 201, "{}", response.text());
    let principal = data(&response);
    assert_eq!(principal["sandbox_repo_cache"], "none");
    assert_eq!(principal["sandbox_observability_enabled"], false);
    assert_eq!(principal["sandbox_sessions_read_enabled"], true);
    assert_eq!(principal["sandbox_workflows_read_enabled"], false);
    assert_eq!(principal["sandbox_workflows_write_enabled"], false);
    // Without a settings row the built-in defaults apply.
    fx.exec("DELETE FROM system_settings").await;
    let response = fx
        .bearer(
            Method::POST,
            "/principals",
            Some(json!({"data": {"foreign_id": "carol"}})),
        )
        .await;
    assert_eq!(data(&response)["sandbox_repo_cache"], "all");
    assert_eq!(data(&response)["sandbox_observability_enabled"], true);
}

#[tokio::test]
async fn principal_validation_reports_each_field() {
    let Some(fx) = Fx::new().await else { return };
    let response = fx
        .bearer(
            Method::POST,
            "/principals",
            Some(json!({"data": {
                "foreign_id": "bad id!", "kind": "wizard", "sandbox_repo_cache": "most",
                "slack_user_id": "nope", "slack_channel_id": "X1", "slack_team_id": "t",
                "slack_email": "not-an-email", "console_user_id": 999_999,
            }})),
        )
        .await;
    assert_eq!(response.status(), 422);
    let body = response.json();
    assert_eq!(body["error"]["message"], "validation failed");
    let details = &body["error"]["details"];
    for field in [
        "foreign_id",
        "kind",
        "sandbox_repo_cache",
        "slack_user_id",
        "slack_channel_id",
        "slack_team_id",
        "slack_email",
        "console_user_id",
    ] {
        assert!(details[field].is_array(), "{field}: {details}");
    }
    let count = fx.scalar_i64("SELECT COUNT(*) FROM principals").await;
    assert_eq!(count, 0);

    let reserved = fx
        .bearer(
            Method::POST,
            "/principals",
            Some(json!({"data": {"foreign_id": "prn_abc"}})),
        )
        .await;
    assert_eq!(reserved.status(), 422);
    assert!(
        reserved.json()["error"]["details"]["foreign_id"][0]
            .as_str()
            .unwrap()
            .contains("reserved")
    );
    // Malformed bodies are 400s, not 500s.
    let wrong_type = fx
        .bearer(
            Method::POST,
            "/principals",
            Some(json!({"data": {"labels": "text"}})),
        )
        .await;
    assert_eq!(wrong_type.status(), 400);
    let no_data = fx
        .bearer(Method::POST, "/principals", Some(json!({"name": "x"})))
        .await;
    assert_eq!(no_data.status(), 400);
}

#[tokio::test]
async fn a_taken_foreign_id_is_a_validation_error() {
    let Some(fx) = Fx::new().await else { return };
    fx.principal("dup", "One").await;
    let response = fx
        .bearer(
            Method::POST,
            "/principals",
            Some(json!({"data": {"foreign_id": "dup"}})),
        )
        .await;
    assert_eq!(response.status(), 422);
    assert_eq!(
        response.json()["error"]["details"]["foreign_id"],
        json!(["has already been taken"])
    );
    // A principal without a foreign id is allowed, any number of times.
    for _ in 0..2 {
        let response = fx
            .console(
                Method::POST,
                "/principals",
                Some(json!({"data": {"name": "anonymous"}})),
            )
            .await;
        assert_eq!(response.status(), 201, "{}", response.text());
        assert_eq!(data(&response)["foreign_id"], Value::Null);
    }
}

#[tokio::test]
async fn upserts_create_by_foreign_id_and_update_by_foreign_id_or_oid() {
    let Some(fx) = Fx::new().await else { return };
    let created = fx
        .bearer(
            Method::PUT,
            "/principals/slack-dm-U01",
            Some(json!({"data": {"name": "Alice", "kind": "slack_dm", "labels": {"a": "1"}}})),
        )
        .await;
    assert_eq!(created.status(), 201, "{}", created.text());
    let oid = data(&created)["id"].as_str().unwrap().to_owned();
    assert_eq!(data(&created)["foreign_id"], "slack-dm-U01");

    // The foreign id in the body never renames a principal.
    let updated = fx
        .bearer(
            Method::PATCH,
            "/principals/slack-dm-U01",
            Some(json!({"data": {"foreign_id": "renamed", "name": "Alice B"}})),
        )
        .await;
    assert_eq!(updated.status(), 200);
    assert_eq!(data(&updated)["name"], "Alice B");
    assert_eq!(data(&updated)["foreign_id"], "slack-dm-U01");
    // Omitted fields stay; null clears a nullable one.
    assert_eq!(data(&updated)["labels"]["a"], "1");
    let by_oid = fx
        .console(
            Method::PUT,
            &format!("/principals/{oid}"),
            Some(json!({"data": {"name": null}})),
        )
        .await;
    assert_eq!(by_oid.status(), 200);
    assert_eq!(data(&by_oid)["name"], Value::Null);
    assert_eq!(data(&by_oid)["kind"], "slack_dm");

    let missing = fx
        .bearer(
            Method::PUT,
            "/principals/prn_nope",
            Some(json!({"data": {"name": "x"}})),
        )
        .await;
    assert_eq!(missing.status(), 404);
    assert_eq!(fx.scalar_i64("SELECT COUNT(*) FROM principals").await, 1);
    // A new principal through an upsert is validated like any other.
    let invalid = fx
        .bearer(
            Method::PUT,
            "/principals/no%20good",
            Some(json!({"data": {"name": "x"}})),
        )
        .await;
    assert_eq!(invalid.status(), 422);
}

#[tokio::test]
async fn updates_write_and_bump_only_what_changed() {
    let Some(fx) = Fx::new().await else { return };
    let oid = fx.principal("quiet", "Quiet").await;
    let id = centaur_console_db::decode_oid("prn", &oid).unwrap();
    let before = fx.version(&oid).await;
    fx.exec(&format!(
        "UPDATE principals SET updated_at = '2001-01-01' WHERE id = {id}"
    ))
    .await;

    // The same values again: nothing is written.
    let same = fx
        .bearer(
            Method::PUT,
            &format!("/principals/{oid}"),
            Some(json!({"data": {"name": "Quiet", "sandbox_repo_cache": "all"}})),
        )
        .await;
    assert_eq!(same.status(), 200);
    assert_eq!(fx.version(&oid).await, before);
    assert!(
        fx.scalar_bool(&format!(
            "SELECT updated_at = '2001-01-01' FROM principals WHERE id = {id}"
        ))
        .await
    );

    // Observability does not change the served config; a name does.
    fx.bearer(
        Method::PUT,
        &format!("/principals/{oid}"),
        Some(json!({"data": {"sandbox_observability_enabled": false}})),
    )
    .await;
    assert_eq!(fx.version(&oid).await, before);
    fx.bearer(
        Method::PUT,
        &format!("/principals/{oid}"),
        Some(json!({"data": {"name": "Loud"}})),
    )
    .await;
    assert_eq!(fx.version(&oid).await, before + 1);
    // The earlier, unrelated change survived the later write.
    assert!(
        !fx.scalar_bool(&format!(
            "SELECT sandbox_observability_enabled FROM principals WHERE id = {id}"
        ))
        .await
    );
}

#[tokio::test]
async fn a_slack_dm_email_links_the_console_user() {
    let Some(fx) = Fx::new().await else { return };
    let linked = fx
        .app
        .create_user("dana@example.test", false, "active")
        .await;
    let response = fx
        .bearer(
            Method::PUT,
            "/principals/slack-dm-U02",
            Some(json!({"data": {"kind": "slack_dm", "slack_user_id": "U00000002", "slack_email": "Dana@Example.test "}})),
        )
        .await;
    assert_eq!(
        response.status(),
        422,
        "an email with spaces is not an email"
    );
    let response = fx
        .bearer(
            Method::PUT,
            "/principals/slack-dm-U02",
            Some(json!({"data": {"kind": "slack_dm", "slack_user_id": "U00000002", "slack_email": "dana@example.test"}})),
        )
        .await;
    assert_eq!(response.status(), 201, "{}", response.text());
    let console_user = fx
        .scalar_i64("SELECT console_user_id FROM principals WHERE foreign_id = 'slack-dm-U02'")
        .await;
    assert_eq!(console_user, linked.id);
    // An unknown email unlinks; leaving the email out keeps the link.
    fx.bearer(
        Method::PUT,
        "/principals/slack-dm-U02",
        Some(json!({"data": {"name": "Dana"}})),
    )
    .await;
    assert_eq!(
        fx.scalar_i64("SELECT console_user_id FROM principals WHERE foreign_id = 'slack-dm-U02'")
            .await,
        linked.id
    );
    fx.bearer(
        Method::PUT,
        "/principals/slack-dm-U02",
        Some(json!({"data": {"slack_email": "nobody@example.test"}})),
    )
    .await;
    assert!(
        fx.scalar_bool(
            "SELECT console_user_id IS NULL FROM principals WHERE foreign_id = 'slack-dm-U02'"
        )
        .await
    );
}

#[tokio::test]
async fn listing_filters_sorts_and_paginates() {
    let Some(fx) = Fx::new().await else { return };
    for (foreign_id, name, labels) in [
        ("p-charlie", "Charlie", json!({"team": "infra"})),
        ("p-alice", "Alice", json!({"team": "infra", "tier": "1"})),
        ("p-bob", "Bob", json!({"team": "sales"})),
    ] {
        let response = fx
            .bearer(
                Method::POST,
                "/principals",
                Some(json!({"data": {"foreign_id": foreign_id, "name": name, "labels": labels}})),
            )
            .await;
        assert_eq!(response.status(), 201);
    }
    let all = fx.bearer(Method::GET, "/principals", None).await.json();
    assert_eq!(ids(&all["data"]), ["p-charlie", "p-alice", "p-bob"]);
    assert_eq!(
        all["meta"],
        json!({"page": 1, "limit": 50, "total": 3, "total_pages": 1})
    );

    let infra = fx
        .bearer(Method::GET, "/principals?labels[team]=infra", None)
        .await
        .json();
    assert_eq!(ids(&infra["data"]), ["p-charlie", "p-alice"]);
    let both = fx
        .bearer(
            Method::GET,
            "/principals?labels%5Bteam%5D=infra&labels%5Btier%5D=1",
            None,
        )
        .await
        .json();
    assert_eq!(ids(&both["data"]), ["p-alice"]);

    let page = fx
        .bearer(Method::GET, "/principals?limit=2&page=2", None)
        .await
        .json();
    assert_eq!(ids(&page["data"]), ["p-bob"]);
    assert_eq!(
        page["meta"],
        json!({"page": 2, "limit": 2, "total": 3, "total_pages": 2})
    );

    let sorted = fx
        .console(Method::GET, "/principals?sort=name", None)
        .await
        .json();
    assert_eq!(ids(&sorted["data"]), ["p-alice", "p-bob", "p-charlie"]);
    let searched = fx
        .console(Method::GET, "/principals?q=ALI", None)
        .await
        .json();
    assert_eq!(ids(&searched["data"]), ["p-alice"]);
    // LIKE wildcards in the search are literal.
    let wildcard = fx
        .console(Method::GET, "/principals?q=%25", None)
        .await
        .json();
    assert_eq!(wildcard["data"], json!([]));
    assert_eq!(
        fx.console(Method::GET, "/principals?sort=age", None)
            .await
            .status(),
        400
    );
}

#[tokio::test]
async fn principal_payloads_carry_roles_and_direct_inherited_and_effective_permissions() {
    let Some(fx) = Fx::new().await else { return };
    let principal = fx.principal("pp", "PP").await;
    let role = fx.role("ops").await;
    fx.console(
        Method::POST,
        &format!("/principals/{principal}/roles"),
        Some(json!({"data": {"role_id": role}})),
    )
    .await;
    fx.console(
        Method::PUT,
        &format!("/principals/{principal}/slack_channel_permissions"),
        Some(json!({"data": {"slack_channel_permissions": [
            {"channel_id": "C00000001", "upload_enabled": true},
        ]}})),
    )
    .await;
    fx.console(
        Method::PUT,
        &format!("/roles/{role}/slack_channel_permissions"),
        Some(json!({"data": {"slack_channel_permissions": [
            {"channel_id": "C00000001", "history_enabled": true},
            {"channel_id": "C00000002", "download_enabled": true},
        ]}})),
    )
    .await;
    let body = fx
        .console(Method::GET, &format!("/principals/{principal}"), None)
        .await;
    let principal = data(&body);
    assert_eq!(
        principal["roles"],
        json!([{"id": role, "foreign_id": "ops", "name": "ops"}])
    );
    assert_eq!(
        principal["slack_channel_permissions"],
        json!([{"channel_id": "C00000001", "upload_enabled": true, "download_enabled": false, "history_enabled": false}])
    );
    assert_eq!(
        principal["inherited_slack_channel_permissions"],
        json!([
            {"channel_id": "C00000001", "upload_enabled": false, "download_enabled": false, "history_enabled": true},
            {"channel_id": "C00000002", "upload_enabled": false, "download_enabled": true, "history_enabled": false},
        ])
    );
    assert_eq!(
        principal["effective_slack_channel_permissions"],
        json!([
            {"channel_id": "C00000001", "upload_enabled": true, "download_enabled": false, "history_enabled": true},
            {"channel_id": "C00000002", "upload_enabled": false, "download_enabled": true, "history_enabled": false},
        ])
    );
}

thread_local! {
    static STATEMENTS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// A process-wide subscriber that counts, per thread, the statements sqlx
/// traces (each test runs on its own thread).
struct StatementCounter;

impl tracing::Subscriber for StatementCounter {
    fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
        metadata.target() == "sqlx::query"
    }
    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn event(&self, _: &tracing::Event<'_>) {
        STATEMENTS.with(|count| count.set(count.get() + 1));
    }
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
}

async fn statements_for(fx: &Fx, path: &str) -> usize {
    static INSTALL: std::sync::Once = std::sync::Once::new();
    INSTALL.call_once(|| {
        tracing::subscriber::set_global_default(StatementCounter).expect("no other subscriber");
    });
    STATEMENTS.with(|count| count.set(0));
    let response = fx.console(Method::GET, path, None).await;
    assert_eq!(response.status(), 200, "{}", response.text());
    STATEMENTS.with(Cell::get)
}

/// Regression: listing principals ran seven association queries per row.
#[tokio::test(flavor = "current_thread")]
async fn a_listing_takes_the_same_queries_whatever_the_page_size() {
    let Some(fx) = Fx::new().await else { return };
    let role = fx.role("shared").await;
    let one = fx.principal("first", "First").await;
    fx.console(
        Method::POST,
        &format!("/principals/{one}/roles"),
        Some(json!({"data": {"role_id": role}})),
    )
    .await;
    fx.console(
        Method::PUT,
        &format!("/principals/{one}/slack_channel_permissions"),
        Some(json!({"data": {"slack_channel_permissions": [{"channel_id": "C00000001", "upload_enabled": true}]}})),
    )
    .await;
    let small = statements_for(&fx, "/principals").await;
    assert!(small > 0, "the statement counter saw nothing");
    for n in 0..12 {
        let oid = fx.principal(&format!("more-{n}"), "More").await;
        fx.console(
            Method::POST,
            &format!("/principals/{oid}/roles"),
            Some(json!({"data": {"role_id": role}})),
        )
        .await;
        fx.console(
            Method::PUT,
            &format!("/principals/{oid}/slack_channel_permissions"),
            Some(json!({"data": {"slack_channel_permissions": [{"channel_id": "C00000002", "history_enabled": true}]}})),
        )
        .await;
    }
    let large = statements_for(&fx, "/principals").await;
    assert_eq!(small, large, "the listing's queries grew with the page");
    let roles_small = statements_for(&fx, "/roles").await;
    assert_eq!(roles_small, statements_for(&fx, "/roles?limit=1").await);
}

#[tokio::test]
async fn show_and_lookup_find_one_principal() {
    let Some(fx) = Fx::new().await else { return };
    let oid = fx.principal("with.dots-and_more", "Dotted").await;
    let by_oid = fx
        .bearer(Method::GET, &format!("/principals/{oid}"), None)
        .await;
    assert_eq!(data(&by_oid)["foreign_id"], "with.dots-and_more");
    for path in [
        "/principals/lookup/with.dots-and_more",
        "/principals/lookup/default/with.dots-and_more",
    ] {
        let response = fx.bearer(Method::GET, path, None).await;
        assert_eq!(data(&response)["id"], oid, "{path}");
    }
    for path in [
        "/principals/prn_zzzz",
        "/principals/not-an-oid",
        "/principals/with.dots-and_more",
        "/principals/lookup/nobody",
        "/principals/lookup/default/nobody",
    ] {
        let response = fx.bearer(Method::GET, path, None).await;
        assert_eq!(response.status(), 404, "{path}");
        assert!(response.error_message().is_some());
    }
}

#[tokio::test]
async fn destroying_a_principal_removes_what_depends_on_it() {
    let Some(fx) = Fx::new().await else { return };
    let oid = fx.principal("doomed", "Doomed").await;
    let id = centaur_console_db::decode_oid("prn", &oid).unwrap();
    let role = fx.role("r").await;
    let secret = fx.static_secret("s").await;
    fx.console(
        Method::POST,
        &format!("/principals/{oid}/roles"),
        Some(json!({"data": {"role_id": role}})),
    )
    .await;
    fx.console(
        Method::POST,
        "/grants",
        Some(json!({"data": {"principal_id": oid, "static_secret_id": secret}})),
    )
    .await;
    fx.console(
        Method::PUT,
        &format!("/principals/{oid}/slack_channel_permissions"),
        Some(json!({"data": {"slack_channel_permissions": [{"channel_id": "C00000001", "upload_enabled": true}]}})),
    )
    .await;
    let proxy = fx.app.create_proxy("sb", id).await;

    let response = fx
        .console(Method::DELETE, &format!("/principals/{oid}"), None)
        .await;
    assert_eq!(response.status(), 204);
    for table in ["grants", "principal_roles", "slack_channel_permissions"] {
        assert_eq!(
            fx.scalar_i64(&format!(
                "SELECT COUNT(*) FROM {table} WHERE principal_id = {id}"
            ))
            .await,
            0,
            "{table}"
        );
    }
    assert_eq!(
        fx.scalar_i64(&format!("SELECT COUNT(*) FROM principals WHERE id = {id}"))
            .await,
        0
    );
    assert!(
        fx.scalar_bool(&format!(
            "SELECT principal_id IS NULL FROM proxies WHERE id = {proxy}"
        ))
        .await
    );
    // The role and the secret stay.
    assert_eq!(fx.scalar_i64("SELECT COUNT(*) FROM roles").await, 1);
    assert_eq!(
        fx.scalar_i64("SELECT COUNT(*) FROM static_secrets").await,
        1
    );
    let again = fx
        .console(Method::DELETE, &format!("/principals/{oid}"), None)
        .await;
    assert_eq!(again.status(), 404);
}

// --- roles --------------------------------------------------------------------------

#[tokio::test]
async fn roles_are_created_upserted_validated_and_found() {
    let Some(fx) = Fx::new().await else { return };
    let created = fx
        .bearer(
            Method::POST,
            "/roles",
            Some(json!({"data": {"foreign_id": "ops", "name": "Ops", "labels": {"k": "v"},
                "slack_channel_permissions": [{"channel_id": "c00000001", "history_enabled": true}]}})),
        )
        .await;
    assert_eq!(created.status(), 201, "{}", created.text());
    let role = data(&created);
    assert!(role["id"].as_str().unwrap().starts_with("role_"));
    assert_eq!(role["labels"], json!({"k": "v"}));
    assert_eq!(
        role["slack_channel_permissions"][0]["channel_id"],
        "C00000001"
    );

    let updated = fx
        .bearer(
            Method::PATCH,
            "/roles/ops",
            Some(json!({"data": {"name": "Operations"}})),
        )
        .await;
    assert_eq!(updated.status(), 200);
    assert_eq!(data(&updated)["name"], "Operations");
    assert_eq!(data(&updated)["labels"], json!({"k": "v"}));
    let upserted = fx
        .bearer(
            Method::PUT,
            "/roles/fresh",
            Some(json!({"data": {"name": "Fresh"}})),
        )
        .await;
    assert_eq!(upserted.status(), 201);
    assert_eq!(
        fx.bearer(Method::GET, "/roles/lookup/ops", None)
            .await
            .json()["data"]["id"],
        role["id"]
    );
    assert_eq!(
        fx.bearer(Method::GET, "/roles/lookup/default/ops", None)
            .await
            .json()["data"]["id"],
        role["id"]
    );
    let by_oid = fx
        .bearer(
            Method::GET,
            &format!("/roles/{}", role["id"].as_str().unwrap()),
            None,
        )
        .await;
    assert_eq!(data(&by_oid)["foreign_id"], "ops");

    for (path, body, status) in [
        ("/roles", json!({"data": {"foreign_id": "ops"}}), 422),
        ("/roles", json!({"data": {"foreign_id": "role_abc"}}), 422),
        ("/roles", json!({"data": {"foreign_id": "no spaces"}}), 422),
        (
            "/roles",
            json!({"data": {"foreign_id": "x", "labels": []}}),
            400,
        ),
        ("/roles/role_missing", json!({"data": {"name": "x"}}), 404),
    ] {
        let method = if path == "/roles" {
            Method::POST
        } else {
            Method::PUT
        };
        let response = fx.bearer(method, path, Some(body)).await;
        assert_eq!(response.status(), status, "{path}: {}", response.text());
    }
    // A role is not a principal, and the reverse.
    assert_eq!(
        fx.bearer(Method::GET, "/roles/lookup/nobody", None)
            .await
            .status(),
        404
    );
    assert_eq!(
        fx.bearer(Method::GET, "/principals/lookup/ops", None)
            .await
            .status(),
        404
    );
}

#[tokio::test]
async fn role_lists_filter_by_search_labels_and_exclusion() {
    let Some(fx) = Fx::new().await else { return };
    for (id, labels) in [
        ("alpha", json!({"t": "1"})),
        ("beta", json!({"t": "2"})),
        ("gamma", json!({"t": "1"})),
    ] {
        fx.bearer(
            Method::POST,
            "/roles",
            Some(json!({"data": {"foreign_id": id, "name": id.to_uppercase(), "labels": labels}})),
        )
        .await;
    }
    let principal = fx.principal("holder", "Holder").await;
    let beta = fx
        .bearer(Method::GET, "/roles/lookup/beta", None)
        .await
        .json()["data"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    fx.console(
        Method::POST,
        &format!("/principals/{principal}/roles"),
        Some(json!({"data": {"role_id": beta}})),
    )
    .await;

    let all = fx.console(Method::GET, "/roles", None).await.json();
    assert_eq!(ids(&all["data"]), ["alpha", "beta", "gamma"]);
    let excluded = fx
        .console(
            Method::GET,
            &format!("/roles?exclude_principal={principal}"),
            None,
        )
        .await
        .json();
    assert_eq!(ids(&excluded["data"]), ["alpha", "gamma"]);
    assert_eq!(excluded["meta"]["total"], 2);
    let searched = fx.console(Method::GET, "/roles?q=MM", None).await.json();
    assert_eq!(ids(&searched["data"]), ["gamma"]);
    let labelled = fx
        .bearer(Method::GET, "/roles?labels[t]=1", None)
        .await
        .json();
    assert_eq!(ids(&labelled["data"]), ["alpha", "gamma"]);
    assert_eq!(
        fx.console(Method::GET, "/roles?exclude_principal=prn_zzz", None)
            .await
            .status(),
        404
    );
}

#[tokio::test]
async fn destroying_a_role_removes_its_grants_assignments_and_permissions() {
    let Some(fx) = Fx::new().await else { return };
    let role = fx.role("temp").await;
    let member = fx.principal("member", "Member").await;
    let secret = fx.static_secret("rs").await;
    fx.console(
        Method::POST,
        &format!("/principals/{member}/roles"),
        Some(json!({"data": {"role_id": role}})),
    )
    .await;
    fx.console(
        Method::POST,
        "/grants",
        Some(json!({"data": {"role_id": role, "static_secret_id": secret}})),
    )
    .await;
    fx.console(
        Method::PUT,
        &format!("/roles/{role}/slack_channel_permissions"),
        Some(json!({"data": {"slack_channel_permissions": [{"channel_id": "C00000001", "upload_enabled": true}]}})),
    )
    .await;
    let before = fx.version(&member).await;
    let response = fx
        .bearer(Method::DELETE, &format!("/roles/{role}"), None)
        .await;
    assert_eq!(response.status(), 204);
    for table in [
        "grants",
        "principal_roles",
        "slack_channel_permissions",
        "roles",
    ] {
        assert_eq!(
            fx.scalar_i64(&format!("SELECT COUNT(*) FROM {table}"))
                .await,
            0,
            "{table}"
        );
    }
    assert_eq!(
        fx.version(&member).await,
        before + 1,
        "the member's config is refreshed"
    );
    assert_eq!(
        fx.scalar_i64("SELECT COUNT(*) FROM static_secrets").await,
        1
    );
    assert_eq!(
        fx.bearer(Method::DELETE, &format!("/roles/{role}"), None)
            .await
            .status(),
        404
    );
}

// --- role assignments ---------------------------------------------------------------

#[tokio::test]
async fn roles_are_assigned_and_unassigned() {
    let Some(fx) = Fx::new().await else { return };
    let principal = fx.principal("pa", "PA").await;
    let role = fx.role("ra").await;
    let before = fx.version(&principal).await;

    let first = fx
        .bearer(
            Method::POST,
            &format!("/principals/{principal}/roles"),
            Some(json!({"data": {"role_id": role}})),
        )
        .await;
    assert_eq!(first.status(), 201, "{}", first.text());
    assert_eq!(data(&first)["id"], role);
    assert_eq!(fx.version(&principal).await, before + 1);
    let again = fx
        .bearer(
            Method::POST,
            &format!("/principals/{principal}/roles"),
            Some(json!({"data": {"role_id": role}})),
        )
        .await;
    assert_eq!(again.status(), 200);
    assert_eq!(
        fx.version(&principal).await,
        before + 1,
        "an existing assignment changes nothing"
    );
    let list = fx
        .bearer(Method::GET, &format!("/principals/{principal}/roles"), None)
        .await;
    assert_eq!(ids(&data(&list)), ["ra"]);

    for (principal_path, role_id, status) in [
        (principal.as_str(), "role_unknown", 404),
        (principal.as_str(), "nonsense", 404),
        ("prn_unknown", role.as_str(), 404),
    ] {
        let response = fx
            .bearer(
                Method::POST,
                &format!("/principals/{principal_path}/roles"),
                Some(json!({"data": {"role_id": role_id}})),
            )
            .await;
        assert_eq!(response.status(), status, "{principal_path} {role_id}");
    }
    let missing_id = fx
        .bearer(
            Method::POST,
            &format!("/principals/{principal}/roles"),
            Some(json!({"data": {}})),
        )
        .await;
    assert_eq!(missing_id.status(), 400);

    let removed = fx
        .bearer(
            Method::DELETE,
            &format!("/principals/{principal}/roles/{role}"),
            None,
        )
        .await;
    assert_eq!(removed.status(), 204);
    assert_eq!(fx.version(&principal).await, before + 2);
    let gone = fx
        .bearer(
            Method::DELETE,
            &format!("/principals/{principal}/roles/{role}"),
            None,
        )
        .await;
    assert_eq!(gone.status(), 404);
    assert_eq!(fx.version(&principal).await, before + 2);
}

// --- grants -------------------------------------------------------------------------

#[tokio::test]
async fn grants_are_created_once_shown_listed_and_revoked() {
    let Some(fx) = Fx::new().await else { return };
    let principal = fx.principal("pg", "PG").await;
    let role = fx.role("rg").await;
    let member = fx.principal("member", "Member").await;
    fx.console(
        Method::POST,
        &format!("/principals/{member}/roles"),
        Some(json!({"data": {"role_id": role}})),
    )
    .await;
    let secret = fx.static_secret("sec-one").await;
    let (principal_version, member_version) =
        (fx.version(&principal).await, fx.version(&member).await);

    let created = fx
        .console(
            Method::POST,
            "/grants",
            Some(json!({"data": {"principal_id": principal, "static_secret_id": secret}})),
        )
        .await;
    assert_eq!(created.status(), 201, "{}", created.text());
    let grant = data(&created);
    assert!(grant["id"].as_str().unwrap().starts_with("grant_"));
    assert_eq!(grant["principal_id"], principal);
    assert_eq!(grant["static_secret_id"], secret);
    assert_eq!(
        grant["grantable"],
        json!({"kind": "static", "kind_label": "Static", "id": secret, "foreign_id": "sec-one", "name": "sec-one name"})
    );
    assert_eq!(fx.version(&principal).await, principal_version + 1);
    assert_eq!(
        fx.scalar_i64("SELECT priority::bigint FROM grants").await,
        100
    );

    let again = fx
        .bearer(
            Method::POST,
            "/grants",
            Some(json!({"data": {"principal_id": principal, "static_secret_id": secret}})),
        )
        .await;
    assert_eq!(again.status(), 200);
    assert_eq!(data(&again)["id"], grant["id"]);
    assert_eq!(fx.scalar_i64("SELECT COUNT(*) FROM grants").await, 1);
    assert_eq!(fx.version(&principal).await, principal_version + 1);

    // A role's grant refreshes every member and has the lower priority.
    let role_grant = fx
        .bearer(
            Method::POST,
            "/grants",
            Some(json!({"data": {"role_id": role, "static_secret_id": secret}})),
        )
        .await;
    assert_eq!(role_grant.status(), 201);
    assert_eq!(data(&role_grant)["role_id"], role);
    assert_eq!(fx.version(&member).await, member_version + 1);
    assert_eq!(
        fx.scalar_i64("SELECT priority::bigint FROM grants WHERE role_id IS NOT NULL")
            .await,
        0
    );

    let shown = fx
        .bearer(
            Method::GET,
            &format!("/grants/{}", grant["id"].as_str().unwrap()),
            None,
        )
        .await;
    assert_eq!(data(&shown)["static_secret_id"], secret);
    let listed = fx
        .console(
            Method::GET,
            &format!("/principals/{principal}/grants"),
            None,
        )
        .await
        .json();
    assert_eq!(listed["data"][0]["id"], grant["id"]);
    assert_eq!(listed["meta"]["total"], 1);
    let role_listed = fx
        .bearer(Method::GET, &format!("/roles/{role}/grants"), None)
        .await
        .json();
    assert_eq!(role_listed["data"][0]["id"], data(&role_grant)["id"]);

    let revoked = fx
        .console(
            Method::DELETE,
            &format!("/grants/{}", data(&role_grant)["id"].as_str().unwrap()),
            None,
        )
        .await;
    assert_eq!(revoked.status(), 204);
    assert_eq!(fx.version(&member).await, member_version + 2);
    assert_eq!(fx.scalar_i64("SELECT COUNT(*) FROM grants").await, 1);
    let gone = fx
        .console(
            Method::DELETE,
            &format!("/grants/{}", data(&role_grant)["id"].as_str().unwrap()),
            None,
        )
        .await;
    assert_eq!(gone.status(), 404);
}

#[tokio::test]
async fn every_secret_kind_can_be_granted() {
    let Some(fx) = Fx::new().await else { return };
    let principal = fx.principal("kinds", "Kinds").await;
    let admin = fx.admin.id;
    let tables = [
        ("static_secrets", "static_secret_id", "ssr", "static"),
        ("gcp_auth_secrets", "gcp_auth_secret_id", "gas", "gcp_auth"),
        (
            "gcp_id_token_secrets",
            "gcp_id_token_secret_id",
            "gid",
            "gcp_id_token",
        ),
        ("aws_auth_secrets", "aws_auth_secret_id", "aas", "aws_auth"),
        (
            "oauth_token_secrets",
            "oauth_token_secret_id",
            "ots",
            "oauth_token",
        ),
        ("pg_dsn_secrets", "pg_dsn_secret_id", "pgs", "pg_dsn"),
        ("hmac_secrets", "hmac_secret_id", "hms", "hmac"),
        ("ssh_key_secrets", "ssh_key_secret_id", "sks", "ssh_key"),
    ];
    for (table, column, prefix, kind) in tables {
        let id = insert_secret(&fx, table, admin).await;
        let oid = centaur_console_db::oid(prefix, id);
        let response = fx
            .console(
                Method::POST,
                "/grants",
                Some(json!({"data": {"principal_id": principal, column: oid}})),
            )
            .await;
        assert_eq!(response.status(), 201, "{table}: {}", response.text());
        assert_eq!(data(&response)[column], oid);
        assert_eq!(data(&response)["grantable"]["kind"], kind);
    }
    assert_eq!(fx.scalar_i64("SELECT COUNT(*) FROM grants").await, 8);
}

/// A minimal row of a secret table (whatever its required columns are).
async fn insert_secret(fx: &Fx, table: &str, admin: i64) -> i64 {
    let columns: Vec<(String, String, Option<String>)> = sqlx::query_as(
        "SELECT column_name, data_type, column_default FROM information_schema.columns \
         WHERE table_name = $1 AND is_nullable = 'NO' AND column_default IS NULL ORDER BY ordinal_position",
    )
    .bind(table)
    .fetch_all(&fx.app.db.pool)
    .await
    .unwrap();
    let mut names = Vec::new();
    let mut values = Vec::new();
    for (name, data_type, _) in columns {
        if name == "id" {
            continue;
        }
        names.push(name.clone());
        values.push(match (name.as_str(), data_type.as_str()) {
            ("created_at" | "updated_at", _) => "now()".to_owned(),
            ("created_by_id", _) => admin.to_string(),
            (_, "jsonb") => "'{}'".to_owned(),
            (_, "character varying" | "text") => format!("'{table}'"),
            (_, "boolean") => "false".to_owned(),
            (_, "bigint" | "integer") => "1".to_owned(),
            other => panic!("{table}: unhandled column {other:?}"),
        });
    }
    fx.scalar_i64(&format!(
        "INSERT INTO {table} ({}) VALUES ({}) RETURNING id",
        names.join(", "),
        values.join(", ")
    ))
    .await
}

#[tokio::test]
async fn grant_requests_are_validated() {
    let Some(fx) = Fx::new().await else { return };
    let principal = fx.principal("pv", "PV").await;
    let secret = fx.static_secret("sv").await;
    for (body, status) in [
        (json!({"static_secret_id": secret}), 422),
        (json!({"principal_id": principal}), 422),
        (json!({"principal_id": "", "static_secret_id": secret}), 422),
        (
            json!({"principal_id": "prn_unknown", "static_secret_id": secret}),
            404,
        ),
        (
            json!({"principal_id": "junk", "static_secret_id": secret}),
            404,
        ),
        (
            json!({"role_id": "role_unknown", "static_secret_id": secret}),
            404,
        ),
        (
            json!({"principal_id": principal, "static_secret_id": "ssr_unknown"}),
            404,
        ),
        (
            json!({"principal_id": principal, "static_secret_id": "pgs_wrongkind"}),
            404,
        ),
    ] {
        let response = fx
            .console(Method::POST, "/grants", Some(json!({"data": body.clone()})))
            .await;
        assert_eq!(response.status(), status, "{body}: {}", response.text());
    }
    let missing = fx
        .console(Method::POST, "/grants", Some(json!({"data": {}})))
        .await;
    assert_eq!(missing.status(), 422);
    assert!(
        missing.json()["error"]["details"]["base"]
            .as_array()
            .unwrap()
            .len()
            >= 2
    );
    assert_eq!(fx.scalar_i64("SELECT COUNT(*) FROM grants").await, 0);
    assert_eq!(
        fx.console(Method::GET, "/grants/grant_unknown", None)
            .await
            .status(),
        404
    );
    assert_eq!(
        fx.console(Method::GET, "/principals/prn_unknown/grants", None)
            .await
            .status(),
        404
    );
}

#[tokio::test]
async fn grants_of_a_grantee_are_paginated() {
    let Some(fx) = Fx::new().await else { return };
    let role = fx.role("many").await;
    for n in 0..3 {
        let secret = fx.static_secret(&format!("many-{n}")).await;
        fx.console(
            Method::POST,
            "/grants",
            Some(json!({"data": {"role_id": role, "static_secret_id": secret}})),
        )
        .await;
    }
    let page = fx
        .console(
            Method::GET,
            &format!("/roles/{role}/grants?limit=2&page=2"),
            None,
        )
        .await
        .json();
    assert_eq!(page["data"].as_array().unwrap().len(), 1);
    assert_eq!(
        page["meta"],
        json!({"page": 2, "limit": 2, "total": 3, "total_pages": 2})
    );
}

// --- Slack channel permissions ------------------------------------------------------

#[tokio::test]
async fn one_permission_is_upserted_with_every_flag_on_unless_given() {
    let Some(fx) = Fx::new().await else { return };
    let principal = fx.principal("sp", "SP").await;
    let path = format!("/principals/{principal}/slack_channel_permissions");
    let before = fx.version(&principal).await;

    let created = fx
        .bearer(
            Method::POST,
            &path,
            Some(json!({"data": {"channel_id": " c00000001 "}})),
        )
        .await;
    assert_eq!(created.status(), 201, "{}", created.text());
    assert_eq!(
        data(&created),
        json!({"channel_id": "C00000001", "upload_enabled": true, "download_enabled": true, "history_enabled": true})
    );
    assert_eq!(fx.version(&principal).await, before + 1);
    let updated = fx
        .bearer(
            Method::POST,
            &path,
            Some(json!({"data": {"channel_id": "C00000001", "upload_enabled": false}})),
        )
        .await;
    assert_eq!(updated.status(), 200);
    assert_eq!(data(&updated)["upload_enabled"], false);
    assert_eq!(data(&updated)["download_enabled"], true);
    assert_eq!(fx.version(&principal).await, before + 2);
    let same = fx
        .bearer(
            Method::POST,
            &path,
            Some(json!({"data": {"channel_id": "C00000001", "upload_enabled": false}})),
        )
        .await;
    assert_eq!(same.status(), 200);
    assert_eq!(
        fx.version(&principal).await,
        before + 2,
        "an identical upsert changes nothing"
    );

    let none_on = fx
        .bearer(
            Method::POST,
            &path,
            Some(json!({"data": {"channel_id": "C00000001", "upload_enabled": false, "download_enabled": false, "history_enabled": false}})),
        )
        .await;
    assert_eq!(none_on.status(), 422);
    assert!(
        none_on.json()["error"]["details"]["base"][0]
            .as_str()
            .unwrap()
            .contains("at least one")
    );
    let invalid = fx
        .bearer(
            Method::POST,
            &path,
            Some(json!({"data": {"channel_id": "nope"}})),
        )
        .await;
    assert_eq!(invalid.status(), 422);
    assert!(invalid.json()["error"]["details"]["channel_id"].is_array());
    let missing = fx
        .bearer(
            Method::POST,
            "/principals/prn_unknown/slack_channel_permissions",
            Some(json!({"data": {"channel_id": "C00000009"}})),
        )
        .await;
    assert_eq!(missing.status(), 404);

    let role = fx.role("sr").await;
    let on_role = fx
        .bearer(Method::POST, &format!("/roles/{role}/slack_channel_permissions"), Some(json!({"data": {"channel_id": "G00000001", "history_enabled": true, "upload_enabled": false, "download_enabled": false}})))
        .await;
    assert_eq!(on_role.status(), 201);
}

#[tokio::test]
async fn permissions_are_replaced_as_a_set_and_unchanged_sets_write_nothing() {
    let Some(fx) = Fx::new().await else { return };
    let principal = fx.principal("rp", "RP").await;
    let path = format!("/principals/{principal}/slack_channel_permissions");
    let before = fx.version(&principal).await;
    // A direct-message permission (made by Slack, not by the API client).
    fx.bearer(Method::POST, &path, Some(json!({"data": {"channel_id": "D00000001", "history_enabled": true, "upload_enabled": false, "download_enabled": false}})))
        .await;
    let bumped = fx.version(&principal).await;
    assert_eq!(bumped, before + 1);

    let replaced = fx
        .console(
            Method::PUT,
            &path,
            Some(json!({"data": {"slack_channel_permissions": [
                {"channel_id": "c00000002", "upload_enabled": true},
                {"channel_id": "C00000002", "history_enabled": true},
                {"channel_id": "D00000099", "upload_enabled": true},
                {"channel_id": "C00000001", "download_enabled": true},
            ]}})),
        )
        .await;
    assert_eq!(replaced.status(), 200, "{}", replaced.text());
    assert_eq!(
        data(&replaced),
        json!([
            {"channel_id": "C00000001", "upload_enabled": false, "download_enabled": true, "history_enabled": false},
            {"channel_id": "C00000002", "upload_enabled": true, "download_enabled": false, "history_enabled": true},
            {"channel_id": "D00000001", "upload_enabled": false, "download_enabled": false, "history_enabled": true},
        ]),
        "duplicates merge; the client's direct-message rows are dropped and the existing one kept"
    );
    assert_eq!(fx.version(&principal).await, bumped + 1);
    let same = fx
        .console(
            Method::PUT,
            &path,
            Some(json!({"data": {"slack_channel_permissions": [
                {"channel_id": "C00000001", "download_enabled": true},
                {"channel_id": "C00000002", "upload_enabled": true, "history_enabled": true},
            ]}})),
        )
        .await;
    assert_eq!(same.status(), 200);
    assert_eq!(fx.version(&principal).await, bumped + 1, "nothing changed");

    let invalid = fx
        .console(
            Method::PUT,
            &path,
            Some(json!({"data": {"slack_channel_permissions": [
                {"channel_id": "C00000003", "upload_enabled": true},
                {"channel_id": "C00000004"},
            ]}})),
        )
        .await;
    assert_eq!(invalid.status(), 422);
    assert_eq!(
        fx.scalar_i64(&format!(
            "SELECT COUNT(*) FROM slack_channel_permissions WHERE principal_id = {}",
            centaur_console_db::decode_oid("prn", &principal).unwrap()
        ))
        .await,
        3,
        "an invalid set replaces nothing"
    );

    let cleared = fx
        .console(
            Method::PUT,
            &path,
            Some(json!({"data": {"slack_channel_permissions": []}})),
        )
        .await;
    assert_eq!(
        data(&cleared).as_array().unwrap().len(),
        1,
        "only the direct-message row stays"
    );
    let wrong_shape = fx
        .console(
            Method::PUT,
            &path,
            Some(json!({"data": {"slack_channel_permissions": "all"}})),
        )
        .await;
    assert_eq!(wrong_shape.status(), 400);

    // A role's set is replaced whole, and its members are refreshed.
    let role = fx.role("rr").await;
    fx.console(
        Method::POST,
        &format!("/principals/{principal}/roles"),
        Some(json!({"data": {"role_id": role}})),
    )
    .await;
    let version = fx.version(&principal).await;
    let on_role = fx
        .console(Method::PUT, &format!("/roles/{role}/slack_channel_permissions"), Some(json!({"data": {"slack_channel_permissions": [{"channel_id": "D00000005", "upload_enabled": true}]}})))
        .await;
    assert_eq!(on_role.status(), 200);
    assert_eq!(
        data(&on_role).as_array().unwrap().len(),
        1,
        "roles keep no direct-message exception"
    );
    assert_eq!(fx.version(&principal).await, version + 1);
}

/// Regression (bc32ec #3): a replace normalizes channel ids before it
/// validates them and before it filters direct-message rows, so a lower-case
/// or padded id is accepted (as Rails did) rather than refused with 422.
#[tokio::test]
async fn replacing_permissions_normalizes_channel_ids_first() {
    let Some(fx) = Fx::new().await else { return };
    let principal = fx.principal("np", "NP").await;
    let path = format!("/principals/{principal}/slack_channel_permissions");
    let replaced = fx
        .console(
            Method::PUT,
            &path,
            Some(json!({"data": {"slack_channel_permissions": [
                {"channel_id": " c0123456789 ", "upload_enabled": true},
                {"channel_id": "g0987654321", "history_enabled": true},
                {"channel_id": "d0123456789", "download_enabled": true},
            ]}})),
        )
        .await;
    assert_eq!(replaced.status(), 200, "{}", replaced.text());
    assert_eq!(
        data(&replaced),
        json!([
            {"channel_id": "C0123456789", "upload_enabled": true, "download_enabled": false, "history_enabled": false},
            {"channel_id": "G0987654321", "upload_enabled": false, "download_enabled": false, "history_enabled": true},
        ]),
        "ids are stored upper-cased and trimmed; the client's DM row is dropped"
    );
    // A lower-case spelling of the stored set is still the same set.
    let version = fx.version(&principal).await;
    let same = fx
        .console(
            Method::PUT,
            &path,
            Some(json!({"data": {"slack_channel_permissions": [
                {"channel_id": "c0123456789", "upload_enabled": true},
                {"channel_id": "g0987654321", "history_enabled": true},
            ]}})),
        )
        .await;
    assert_eq!(same.status(), 200, "{}", same.text());
    assert_eq!(fx.version(&principal).await, version, "nothing changed");

    // A role has no direct-message exception: the row is kept, normalized.
    let role = fx.role("nr").await;
    let on_role = fx
        .console(
            Method::PUT,
            &format!("/roles/{role}/slack_channel_permissions"),
            Some(json!({"data": {"slack_channel_permissions": [
                {"channel_id": "d0123456789", "upload_enabled": true},
            ]}})),
        )
        .await;
    assert_eq!(on_role.status(), 200, "{}", on_role.text());
    assert_eq!(
        data(&on_role),
        json!([{"channel_id": "D0123456789", "upload_enabled": true, "download_enabled": false, "history_enabled": false}])
    );
}

#[tokio::test]
async fn one_permission_is_deleted_by_channel() {
    let Some(fx) = Fx::new().await else { return };
    let principal = fx.principal("dp", "DP").await;
    let base = format!("/principals/{principal}/slack_channel_permissions");
    fx.bearer(
        Method::POST,
        &base,
        Some(json!({"data": {"channel_id": "C00000001"}})),
    )
    .await;
    let before = fx.version(&principal).await;
    let removed = fx
        .console(Method::DELETE, &format!("{base}/c00000001"), None)
        .await;
    assert_eq!(removed.status(), 204);
    assert_eq!(fx.version(&principal).await, before + 1);
    assert_eq!(
        fx.console(Method::DELETE, &format!("{base}/C00000001"), None)
            .await
            .status(),
        404
    );
    assert_eq!(
        fx.console(
            Method::DELETE,
            "/principals/prn_unknown/slack_channel_permissions/C00000001",
            None
        )
        .await
        .status(),
        404
    );
    let role = fx.role("dr").await;
    fx.bearer(
        Method::POST,
        &format!("/roles/{role}/slack_channel_permissions"),
        Some(json!({"data": {"channel_id": "C00000001"}})),
    )
    .await;
    let removed = fx
        .console(
            Method::DELETE,
            &format!("/roles/{role}/slack_channel_permissions/C00000001"),
            None,
        )
        .await;
    assert_eq!(removed.status(), 204);
}

// --- effective config and grants ----------------------------------------------------

#[tokio::test]
async fn effective_config_is_redacted_uncacheable_and_names_the_api_server() {
    let Some(fx) = Fx::with_env(&[
        (
            "CENTAUR_API_SERVER_PROXY_HOSTS",
            "API.internal., other.internal",
        ),
        ("CENTAUR_API_URL", "https://api.example.test:8443/base"),
    ])
    .await
    else {
        return;
    };
    let principal = fx.principal("ec", "EC").await;
    for path in [
        format!("/principals/{principal}/effective_config"),
        "/principals/lookup/ec/effective_config".to_owned(),
        "/principals/lookup/default/ec/effective_config".to_owned(),
    ] {
        let response = fx.bearer(Method::GET, &path, None).await;
        assert_eq!(response.status(), 200, "{path}: {}", response.text());
        assert_eq!(response.header("cache-control"), Some("no-store"));
        let config = data(&response);
        assert_eq!(config["id"], principal);
        assert!(config["transforms"].is_array() && config["postgres"].is_array());
        let secrets = config["secrets"].as_array().unwrap();
        let api_server = secrets.last().unwrap();
        assert_eq!(
            api_server["source"],
            json!({"type": "control_plane", "value": "[redacted]"})
        );
        assert_eq!(
            api_server["rules"],
            json!([{"host": "api.internal"}, {"host": "other.internal"}, {"host": "api.example.test"}])
        );
        assert_eq!(api_server["inject"]["header"], "Authorization");
    }
    assert_eq!(
        fx.bearer(
            Method::GET,
            "/principals/prn_unknown/effective_config",
            None
        )
        .await
        .status(),
        404
    );
    assert_eq!(
        fx.bearer(
            Method::GET,
            "/principals/lookup/nobody/effective_config",
            None
        )
        .await
        .status(),
        404
    );
}

#[tokio::test]
async fn effective_config_omits_the_api_server_without_hosts_or_a_signing_secret() {
    let Some(fx) = Fx::new().await else { return };
    let principal = fx.principal("plain", "Plain").await;
    let config = data(
        &fx.bearer(
            Method::GET,
            &format!("/principals/{principal}/effective_config"),
            None,
        )
        .await,
    );
    assert_eq!(config["secrets"], json!([]));

    let Some(hosted) = Fx::with_env(&[("CENTAUR_API_SERVER_PROXY_HOSTS", "api.internal")]).await
    else {
        return;
    };
    let principal = hosted.principal("plain", "Plain").await;
    let config = data(
        &hosted
            .bearer(
                Method::GET,
                &format!("/principals/{principal}/effective_config"),
                None,
            )
            .await,
    );
    assert_eq!(config["secrets"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn effective_grants_show_how_each_secret_is_reached_in_pages() {
    let Some(fx) = Fx::new().await else { return };
    let principal = fx.principal("eg", "EG").await;
    let role_a = fx.role("role-a").await;
    let role_b = fx.role("role-b").await;
    for role in [&role_a, &role_b] {
        fx.console(
            Method::POST,
            &format!("/principals/{principal}/roles"),
            Some(json!({"data": {"role_id": role}})),
        )
        .await;
    }
    let direct_and_role = fx.static_secret("both").await;
    let role_only = fx.static_secret("via-roles").await;
    let direct_only = fx.static_secret("direct").await;
    let pg = centaur_console_db::oid(
        "pgs",
        insert_secret(&fx, "pg_dsn_secrets", fx.admin.id).await,
    );
    for (grantee, column, secret) in [
        ("principal_id", "static_secret_id", &direct_and_role),
        ("role_id", "static_secret_id", &direct_and_role),
        ("principal_id", "static_secret_id", &direct_only),
        ("role_id", "static_secret_id", &role_only),
        ("principal_id", "pg_dsn_secret_id", &pg),
    ] {
        let owner = if grantee == "principal_id" {
            &principal
        } else {
            &role_a
        };
        let response = fx
            .console(
                Method::POST,
                "/grants",
                Some(json!({"data": {grantee: owner, column: secret}})),
            )
            .await;
        assert_eq!(response.status(), 201, "{}", response.text());
    }
    // role-b also reaches the role-only secret.
    fx.console(
        Method::POST,
        "/grants",
        Some(json!({"data": {"role_id": role_b, "static_secret_id": role_only}})),
    )
    .await;

    let all = fx
        .console(
            Method::GET,
            &format!("/principals/{principal}/effective_grants"),
            None,
        )
        .await
        .json();
    assert_eq!(
        all["meta"],
        json!({"page": 1, "limit": 50, "total": 4, "total_pages": 1})
    );
    let rows = all["data"].as_array().unwrap();
    let names: Vec<_> = rows
        .iter()
        .map(|row| row["foreign_id"].as_str().unwrap_or("-"))
        .collect();
    // Kind order, then ascending strongest priority: role-only (0) before
    // the secrets that are also granted directly (100).
    assert_eq!(&names[..3], ["via-roles", "both", "direct"]);
    assert_eq!(rows[3]["kind"], "pg_dsn");
    assert_eq!(rows[0]["direct"], false);
    assert_eq!(rows[0]["roles"].as_array().unwrap().len(), 2);
    assert_eq!(rows[0]["grant_key"], "static_secret_id");
    assert_eq!(rows[1]["direct"], true);
    assert_eq!(rows[1]["roles"][0]["foreign_id"], "role-a");
    assert_eq!(rows[2]["roles"], json!([]));

    let second = fx
        .console(
            Method::GET,
            &format!("/principals/{principal}/effective_grants?limit=3&page=2"),
            None,
        )
        .await
        .json();
    assert_eq!(second["data"].as_array().unwrap().len(), 1);
    assert_eq!(second["data"][0]["kind"], "pg_dsn");
    assert_eq!(
        second["meta"],
        json!({"page": 2, "limit": 3, "total": 4, "total_pages": 2})
    );
    assert_eq!(
        fx.console(
            Method::GET,
            "/principals/prn_unknown/effective_grants",
            None
        )
        .await
        .status(),
        404
    );
}
