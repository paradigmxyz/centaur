//! Skills: console authoring, the sandbox catalog, editors, and search.

mod misc_common;

use axum::http::Method;
use centaur_console_api::{auth::Access, misc, testing::TestApp};
use misc_common::{browser, console, sandbox, sandbox_call};
use serde_json::{Value, json};

const BASE: &str = "/api/v1/console/skills";

fn skill_body(name: &str) -> Value {
    json!({"data": {
        "name": name,
        "description": "Ship the thing",
        "instructions": "# Steps\n\n1. Build\n2. Ship",
    }})
}

struct World {
    app: TestApp,
    admin: i64,
    owner: i64,
    other: i64,
}

async fn world() -> Option<World> {
    let app = TestApp::new(misc::routes()).await?;
    let admin = app
        .create_user("admin@example.test", true, "active")
        .await
        .id;
    let owner = app
        .create_user("owner@example.test", false, "active")
        .await
        .id;
    let other = app
        .create_user("other@example.test", false, "active")
        .await
        .id;
    Some(World {
        app,
        admin,
        owner,
        other,
    })
}

async fn create(w: &World, user: i64, name: &str) -> Value {
    let mut b = browser(&w.app, user);
    let response = console(&w.app, &mut b, Method::POST, BASE, Some(skill_body(name))).await;
    assert_eq!(response.status(), 201, "{}", response.text());
    response.json()["data"].clone()
}

#[tokio::test]
async fn console_skill_routes_allow_any_active_user() {
    let Some(w) = world().await else { return };
    let app = &w.app;
    let body = skill_body("matrix");
    app.assert_console_access(Access::Active, Method::GET, BASE, None)
        .await;
    app.assert_console_access(Access::Active, Method::POST, BASE, Some(&body))
        .await;
    app.assert_console_access(
        Access::Active,
        Method::GET,
        &format!("{BASE}/editor_candidates"),
        None,
    )
    .await;
    for (method, path) in [
        (Method::GET, "/skl_x"),
        (Method::PATCH, "/skl_x"),
        (Method::DELETE, "/skl_x"),
        (Method::POST, "/skl_x/share"),
        (Method::POST, "/skl_x/unshare"),
    ] {
        app.assert_console_access(
            Access::Active,
            method,
            &format!("{BASE}{path}"),
            Some(&body),
        )
        .await;
    }
}

#[tokio::test]
async fn creating_a_skill_returns_the_detail_the_spa_reads() {
    let Some(w) = world().await else { return };
    let skill = create(&w, w.owner, "release-notes").await;
    assert!(skill["id"].as_str().unwrap().starts_with("skl_"));
    assert_eq!(skill["name"], "release-notes");
    assert_eq!(skill["visibility"], "shared");
    assert_eq!(skill["author"], "owner@example.test");
    assert_eq!(skill["owner"]["email"], "owner@example.test");
    assert_eq!(skill["instructions"], "# Steps\n\n1. Build\n2. Ship");
    assert!(
        skill["content_html"]
            .as_str()
            .unwrap()
            .contains("<h1>Steps</h1>")
    );
    assert_eq!(skill["lock_version"], 0);
    assert_eq!(skill["checksum"].as_str().unwrap().len(), 64);
    assert_eq!(skill["editors"], json!([]));
    assert_eq!(
        skill["viewer"],
        json!({"owner": true, "can_edit": true, "can_manage": true, "can_unshare": true})
    );
    assert!(skill["updated_at"].as_str().unwrap().ends_with('Z'));
    let shared_at: Option<String> = sqlx::query_scalar("SELECT shared_at::text FROM skills")
        .fetch_one(&w.app.db.pool)
        .await
        .unwrap();
    assert!(shared_at.is_some());
}

#[tokio::test]
async fn skill_validation_reports_fields() {
    let Some(w) = world().await else { return };
    create(&w, w.owner, "taken").await;
    let mut b = browser(&w.app, w.other);
    let bad = json!({"data": {"name": "Not Valid", "description": " ", "instructions": ""}});
    let response = console(&w.app, &mut b, Method::POST, BASE, Some(bad)).await;
    assert_eq!(response.status(), 422);
    let details = &response.json()["error"]["details"];
    assert!(details["name"].is_array() && details["description"].is_array());
    assert!(details["instructions"].is_array());
    let dup = console(
        &w.app,
        &mut b,
        Method::POST,
        BASE,
        Some(skill_body("taken")),
    )
    .await;
    assert_eq!(dup.status(), 422);
    assert_eq!(
        dup.json()["error"]["details"]["name"],
        json!(["is already used by another skill"])
    );
    let reserved = console(
        &w.app,
        &mut b,
        Method::POST,
        BASE,
        Some(skill_body("search")),
    )
    .await;
    assert_eq!(reserved.status(), 422);
    let huge =
        json!({"data": {"name": "huge", "description": "d", "instructions": "x".repeat(70_000)}});
    let response = console(&w.app, &mut b, Method::POST, BASE, Some(huge)).await;
    assert_eq!(response.status(), 422);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM skills")
        .fetch_one(&w.app.db.pool)
        .await
        .unwrap();
    assert_eq!(count, 1, "refused requests write nothing");
}

#[tokio::test]
async fn visibility_and_ownership_scope_what_users_see() {
    let Some(w) = world().await else { return };
    let skill = create(&w, w.owner, "mine-and-shared").await;
    let id = skill["id"].as_str().unwrap();
    let mut other = browser(&w.app, w.other);
    // The directory lists shared skills; "mine" only what the caller owns or edits.
    let directory = console(&w.app, &mut other, Method::GET, BASE, None).await;
    assert_eq!(directory.json()["data"].as_array().unwrap().len(), 1);
    assert_eq!(
        directory.json()["data"][0]["viewer"],
        json!({"owner": false})
    );
    let mine = console(
        &w.app,
        &mut other,
        Method::GET,
        &format!("{BASE}?tab=mine"),
        None,
    )
    .await;
    assert_eq!(mine.json()["data"], json!([]));
    // Another user cannot edit, archive, or share it: 404.
    let path = format!("{BASE}/{id}");
    for (method, path) in [
        (Method::PATCH, path.clone()),
        (Method::DELETE, path.clone()),
        (Method::POST, format!("{path}/share")),
        (Method::POST, format!("{path}/unshare")),
    ] {
        let body = (method == Method::PATCH).then(|| skill_body("renamed"));
        let response = console(&w.app, &mut other, method.clone(), &path, body).await;
        assert_eq!(response.status(), 404, "{method} {path}");
    }
    // The owner makes it private: it disappears for others.
    let mut owner = browser(&w.app, w.owner);
    let response = console(
        &w.app,
        &mut owner,
        Method::POST,
        &format!("{path}/unshare"),
        None,
    )
    .await;
    assert_eq!(response.json()["data"]["visibility"], "private");
    let shared_at: Option<String> = sqlx::query_scalar("SELECT shared_at::text FROM skills")
        .fetch_one(&w.app.db.pool)
        .await
        .unwrap();
    assert!(shared_at.is_none());
    assert_eq!(
        console(&w.app, &mut other, Method::GET, &path, None)
            .await
            .status(),
        404
    );
    assert_eq!(
        console(&w.app, &mut other, Method::GET, BASE, None)
            .await
            .json()["data"],
        json!([])
    );
    // An admin may unshare someone else's shared skill, but not read a private one.
    let mut owner = browser(&w.app, w.owner);
    console(
        &w.app,
        &mut owner,
        Method::POST,
        &format!("{path}/share"),
        None,
    )
    .await;
    let mut admin = browser(&w.app, w.admin);
    let response = console(
        &w.app,
        &mut admin,
        Method::POST,
        &format!("{path}/unshare"),
        None,
    )
    .await;
    assert_eq!(response.status(), 200, "{}", response.text());
    assert_eq!(response.json()["data"]["visibility"], "private");
    assert_eq!(
        console(&w.app, &mut admin, Method::GET, &path, None)
            .await
            .status(),
        404
    );
}

#[tokio::test]
async fn updates_check_lock_version_and_change_only_submitted_fields() {
    let Some(w) = world().await else { return };
    let skill = create(&w, w.owner, "tuned").await;
    let path = format!("{BASE}/{}", skill["id"].as_str().unwrap());
    let mut owner = browser(&w.app, w.owner);
    // A private skill stays private across an unrelated edit.
    sqlx::query("UPDATE skills SET visibility = 'private', shared_at = NULL")
        .execute(&w.app.db.pool)
        .await
        .unwrap();
    let patch = json!({"data": {"description": "Changed", "lock_version": 0}});
    let response = console(&w.app, &mut owner, Method::PATCH, &path, Some(patch)).await;
    assert_eq!(response.status(), 200, "{}", response.text());
    let data = &response.json()["data"];
    assert_eq!(
        (data["description"].as_str(), data["lock_version"].as_i64()),
        (Some("Changed"), Some(1))
    );
    assert_eq!(data["name"], "tuned");
    assert_eq!(data["visibility"], "private");
    // The old version is stale.
    let stale = json!({"data": {"description": "Late edit", "lock_version": 0}});
    let response = console(&w.app, &mut owner, Method::PUT, &path, Some(stale)).await;
    assert_eq!(response.status(), 409);
    assert!(
        response
            .error_message()
            .unwrap()
            .contains("changed since you opened it")
    );
    // An unchanged save does not bump the version.
    let same = json!({"data": {"description": "Changed", "lock_version": 1}});
    let response = console(&w.app, &mut owner, Method::PATCH, &path, Some(same)).await;
    assert_eq!(response.json()["data"]["lock_version"], 1);
    // Whitespace is trimmed and CRLF normalized.
    let messy = json!({"data": {"instructions": "  a\r\nb  \r\n", "name": " tuned "}});
    let response = console(&w.app, &mut owner, Method::PATCH, &path, Some(messy)).await;
    assert_eq!(response.json()["data"]["instructions"], "a\nb");
    assert_eq!(response.json()["data"]["name"], "tuned");
    // Invalid values leave the row alone.
    let invalid = json!({"data": {"name": "Bad Name"}});
    let response = console(&w.app, &mut owner, Method::PATCH, &path, Some(invalid)).await;
    assert_eq!(response.status(), 422);
    let name: String = sqlx::query_scalar("SELECT name FROM skills")
        .fetch_one(&w.app.db.pool)
        .await
        .unwrap();
    assert_eq!(name, "tuned");
}

#[tokio::test]
async fn editors_can_edit_but_only_the_owner_manages_them() {
    let Some(w) = world().await else { return };
    let skill = create(&w, w.owner, "shared-edit").await;
    let id = skill["id"].as_str().unwrap().to_owned();
    let path = format!("{BASE}/{id}");
    let editor_oid = centaur_console_db::oid("usr", w.other);
    let mut owner = browser(&w.app, w.owner);
    // Candidates exclude the caller; search matches email.
    let candidates = console(
        &w.app,
        &mut owner,
        Method::GET,
        &format!("{BASE}/editor_candidates?q=OTHER"),
        None,
    )
    .await;
    let emails: Vec<_> = candidates.json()["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|u| u["email"].clone())
        .collect();
    assert_eq!(emails, [json!("other@example.test")]);
    // Unavailable users (unknown, pending, the owner) are refused whole.
    let pending = app_pending(&w).await;
    for bad in [
        centaur_console_db::oid("usr", 9_999),
        centaur_console_db::oid("usr", pending),
        centaur_console_db::oid("usr", w.owner),
        "nonsense".to_owned(),
    ] {
        let body = json!({"data": {"editor_ids": [editor_oid, bad]}});
        let response = console(&w.app, &mut owner, Method::PATCH, &path, Some(body)).await;
        assert_eq!(response.status(), 422, "{bad}");
        assert!(response.json()["error"]["details"]["editor_ids"].is_array());
    }
    let body = json!({"data": {"editor_ids": [editor_oid, editor_oid]}});
    let response = console(&w.app, &mut owner, Method::PATCH, &path, Some(body)).await;
    assert_eq!(response.status(), 200, "{}", response.text());
    let data = &response.json()["data"];
    assert_eq!(data["editors"][0]["email"], "other@example.test");
    assert_eq!(
        data["lock_version"], 1,
        "an editor change bumps the version"
    );
    // The editor can edit but not manage, and cannot replace editors.
    let mut editor = browser(&w.app, w.other);
    let detail = console(&w.app, &mut editor, Method::GET, &path, None).await;
    assert_eq!(
        detail.json()["data"]["viewer"],
        json!({"owner": false, "can_edit": true, "can_manage": false, "can_unshare": false})
    );
    let body = json!({"data": {"description": "By editor", "editor_ids": []}});
    let response = console(&w.app, &mut editor, Method::PATCH, &path, Some(body)).await;
    assert_eq!(response.status(), 200);
    assert_eq!(
        response.json()["data"]["editors"].as_array().unwrap().len(),
        1
    );
    assert_eq!(
        console(&w.app, &mut editor, Method::DELETE, &path, None)
            .await
            .status(),
        404
    );
    let mine = console(
        &w.app,
        &mut editor,
        Method::GET,
        &format!("{BASE}?tab=mine"),
        None,
    )
    .await;
    assert_eq!(mine.json()["data"].as_array().unwrap().len(), 1);
}

async fn app_pending(w: &World) -> i64 {
    w.app
        .create_user("pending@example.test", false, "pending")
        .await
        .id
}

#[tokio::test]
async fn archiving_frees_the_name() {
    let Some(w) = world().await else { return };
    let skill = create(&w, w.owner, "old-name").await;
    let path = format!("{BASE}/{}", skill["id"].as_str().unwrap());
    let mut owner = browser(&w.app, w.owner);
    assert_eq!(
        console(&w.app, &mut owner, Method::DELETE, &path, None)
            .await
            .status(),
        204
    );
    assert_eq!(
        console(&w.app, &mut owner, Method::GET, &path, None)
            .await
            .status(),
        404
    );
    assert_eq!(
        console(&w.app, &mut owner, Method::GET, BASE, None)
            .await
            .json()["data"],
        json!([])
    );
    create(&w, w.other, "old-name").await;
    let archived: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM skills WHERE archived_at IS NOT NULL AND visibility = 'private'",
    )
    .fetch_one(&w.app.db.pool)
    .await
    .unwrap();
    assert_eq!(archived, 1);
}

#[tokio::test]
async fn directory_search_matches_any_term() {
    let Some(w) = world().await else { return };
    create(&w, w.owner, "deploy-service").await;
    let other = json!({"data": {"name": "cooking", "description": "Pasta recipes", "instructions": "Boil water"}});
    let mut b = browser(&w.app, w.owner);
    console(&w.app, &mut b, Method::POST, BASE, Some(other)).await;
    let found = console(
        &w.app,
        &mut b,
        Method::GET,
        &format!("{BASE}?q=pasta+unrelated"),
        None,
    )
    .await;
    let names: Vec<_> = found.json()["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["name"].clone())
        .collect();
    assert_eq!(names, [json!("cooking")]);
    let all = console(&w.app, &mut b, Method::GET, &format!("{BASE}?q=%20"), None).await;
    assert_eq!(all.json()["data"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn sandbox_catalog_is_scoped_by_the_linked_user() {
    let Some(w) = world().await else { return };
    let shared = create(&w, w.owner, "shared-skill").await;
    let private = create(&w, w.owner, "private-skill").await;
    let mut owner = browser(&w.app, w.owner);
    console(
        &w.app,
        &mut owner,
        Method::POST,
        &format!("{BASE}/{}/unshare", private["id"].as_str().unwrap()),
        None,
    )
    .await;
    let unlinked = sandbox(&w.app, "unlinked", None, w.admin).await;
    let linked = sandbox(&w.app, "linked", Some(w.owner), w.admin).await;
    let base = "/api/v1/sandbox/skills";
    // An unlinked sandbox reads shared skills only.
    let response = sandbox_call(&w.app, &unlinked, Method::GET, base, None).await;
    assert_eq!(response.status(), 200);
    let names: Vec<_> = response.json()["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["name"].clone())
        .collect();
    assert_eq!(names, [json!("shared-skill")]);
    assert!(response.json()["data"][0].get("document").is_none());
    let response = sandbox_call(&w.app, &linked, Method::GET, base, None).await;
    assert_eq!(response.json()["data"].as_array().unwrap().len(), 2);
    let response = sandbox_call(
        &w.app,
        &linked,
        Method::GET,
        &format!("{base}?scope=private"),
        None,
    )
    .await;
    assert_eq!(response.json()["data"].as_array().unwrap().len(), 1);
    // Show by name or oid carries the SKILL.md and is never cached.
    let response = sandbox_call(
        &w.app,
        &unlinked,
        Method::GET,
        &format!("{base}/shared-skill"),
        None,
    )
    .await;
    assert_eq!(response.status(), 200);
    assert_eq!(response.header("cache-control"), Some("no-store"));
    let data = &response.json()["data"];
    let document = data["document"].as_str().unwrap();
    assert!(
        document
            .starts_with("---\nname: shared-skill\ndescription: Ship the thing\n---\n\n# Steps")
    );
    assert_eq!(data["checksum"].as_str().unwrap().len(), 64);
    let by_oid = sandbox_call(
        &w.app,
        &unlinked,
        Method::GET,
        &format!("{base}/{}", shared["id"].as_str().unwrap()),
        None,
    )
    .await;
    assert_eq!(by_oid.status(), 200);
    assert_eq!(
        sandbox_call(
            &w.app,
            &unlinked,
            Method::GET,
            &format!("{base}/private-skill"),
            None
        )
        .await
        .status(),
        404
    );
    // Limits clamp; garbage is a 400.
    assert_eq!(
        sandbox_call(
            &w.app,
            &linked,
            Method::GET,
            &format!("{base}?limit=0"),
            None
        )
        .await
        .json()["data"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        sandbox_call(
            &w.app,
            &linked,
            Method::GET,
            &format!("{base}?limit=lots"),
            None
        )
        .await
        .status(),
        400
    );
    // Search needs a query.
    assert_eq!(
        sandbox_call(
            &w.app,
            &linked,
            Method::GET,
            &format!("{base}/search"),
            None
        )
        .await
        .status(),
        400
    );
    let found = sandbox_call(
        &w.app,
        &linked,
        Method::GET,
        &format!("{base}/search?q=ship"),
        None,
    )
    .await;
    assert_eq!(found.json()["data"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn sandbox_authoring_needs_an_active_linked_user() {
    let Some(w) = world().await else { return };
    let unlinked = sandbox(&w.app, "unlinked", None, w.admin).await;
    let base = "/api/v1/sandbox/skills";
    let body = skill_body("from-sandbox");
    for (method, path) in [
        (Method::POST, base.to_owned()),
        (Method::PATCH, format!("{base}/skl_x")),
        (Method::DELETE, format!("{base}/skl_x")),
        (Method::POST, format!("{base}/skl_x/share")),
        (Method::POST, format!("{base}/skl_x/editors")),
    ] {
        let body = if path.ends_with("/editors") {
            json!({"data": {"user": "x"}})
        } else {
            body.clone()
        };
        let response = sandbox_call(&w.app, &unlinked, method.clone(), &path, Some(body)).await;
        assert_eq!(response.status(), 403, "{method} {path}");
    }
    // A disabled owner is not "linked".
    let gone = w
        .app
        .create_user("gone@example.test", false, "disabled")
        .await
        .id;
    let token = sandbox(&w.app, "gone", Some(gone), w.admin).await;
    assert_eq!(
        sandbox_call(&w.app, &token, Method::POST, base, Some(body))
            .await
            .status(),
        403
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM skills")
        .fetch_one(&w.app.db.pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[tokio::test]
async fn sandbox_authoring_round_trip() {
    let Some(w) = world().await else { return };
    let token = sandbox(&w.app, "author", Some(w.owner), w.admin).await;
    let base = "/api/v1/sandbox/skills";
    let mut body = skill_body("sandbox-made");
    body["data"]["visibility"] = json!("private");
    let created = sandbox_call(&w.app, &token, Method::POST, base, Some(body)).await;
    assert_eq!(created.status(), 201, "{}", created.text());
    let data = created.json()["data"].clone();
    assert_eq!(
        (data["visibility"].as_str(), data["lock_version"].as_i64()),
        (Some("private"), Some(0))
    );
    assert!(data["document"].is_string());
    let path = format!("{base}/{}", data["id"].as_str().unwrap());
    let mut bad = skill_body("x");
    bad["data"]["visibility"] = json!("secret");
    let response = sandbox_call(&w.app, &token, Method::POST, base, Some(bad)).await;
    assert_eq!(response.status(), 422);
    assert!(response.json()["error"]["details"]["visibility"].is_array());
    // Updates take a lock version; visibility changes go through share.
    let patch =
        json!({"data": {"instructions": "New steps", "visibility": "shared", "lock_version": 0}});
    let updated = sandbox_call(&w.app, &token, Method::PATCH, &path, Some(patch)).await;
    assert_eq!(updated.status(), 200, "{}", updated.text());
    assert_eq!(updated.json()["data"]["visibility"], "private");
    assert_eq!(updated.json()["data"]["lock_version"], 1);
    let stale = json!({"data": {"instructions": "Stale", "lock_version": 0}});
    assert_eq!(
        sandbox_call(&w.app, &token, Method::PATCH, &path, Some(stale))
            .await
            .status(),
        409
    );
    let shared = sandbox_call(&w.app, &token, Method::POST, &format!("{path}/share"), None).await;
    assert_eq!(shared.json()["data"]["visibility"], "shared");
    let unshared = sandbox_call(
        &w.app,
        &token,
        Method::POST,
        &format!("{path}/unshare"),
        None,
    )
    .await;
    assert_eq!(unshared.json()["data"]["visibility"], "private");
    // A skill owned by someone else is not editable here.
    let other = create(&w, w.other, "not-yours").await;
    let theirs = format!("{base}/{}", other["id"].as_str().unwrap());
    let patch = json!({"data": {"description": "Hijack"}});
    assert_eq!(
        sandbox_call(&w.app, &token, Method::PATCH, &theirs, Some(patch))
            .await
            .status(),
        404
    );
    assert_eq!(
        sandbox_call(&w.app, &token, Method::DELETE, &theirs, None)
            .await
            .status(),
        404
    );
    assert_eq!(
        sandbox_call(&w.app, &token, Method::DELETE, &path, None)
            .await
            .status(),
        204
    );
    assert_eq!(
        sandbox_call(&w.app, &token, Method::GET, &path, None)
            .await
            .status(),
        404
    );
}

#[tokio::test]
async fn sandbox_editors_by_email_or_id() {
    let Some(w) = world().await else { return };
    let token = sandbox(&w.app, "author", Some(w.owner), w.admin).await;
    let skill = create(&w, w.owner, "collab").await;
    let path = format!(
        "/api/v1/sandbox/skills/{}/editors",
        skill["id"].as_str().unwrap()
    );
    let add = |user: &str| json!({"data": {"user": user}});
    let response = sandbox_call(
        &w.app,
        &token,
        Method::POST,
        &path,
        Some(add("Other@Example.test")),
    )
    .await;
    assert_eq!(response.status(), 200, "{}", response.text());
    let data = &response.json()["data"];
    assert_eq!(data["editors"][0]["email"], "other@example.test");
    assert_eq!(data["lock_version"], 1);
    // Adding again changes nothing.
    let again = sandbox_call(
        &w.app,
        &token,
        Method::POST,
        &path,
        Some(add(&centaur_console_db::oid("usr", w.other))),
    )
    .await;
    assert_eq!(again.json()["data"]["lock_version"], 1);
    let owner_ref = sandbox_call(
        &w.app,
        &token,
        Method::POST,
        &path,
        Some(add("owner@example.test")),
    )
    .await;
    assert_eq!(owner_ref.status(), 422);
    assert_eq!(
        sandbox_call(
            &w.app,
            &token,
            Method::POST,
            &path,
            Some(add("nobody@example.test"))
        )
        .await
        .status(),
        404
    );
    let listed = sandbox_call(&w.app, &token, Method::GET, &path, None).await;
    assert_eq!(
        listed.json()["data"]["editors"].as_array().unwrap().len(),
        1
    );
    let removed = sandbox_call(
        &w.app,
        &token,
        Method::DELETE,
        &path,
        Some(add("other@example.test")),
    )
    .await;
    assert_eq!(removed.json()["data"]["editors"], json!([]));
    assert_eq!(removed.json()["data"]["lock_version"], 2);
    assert_eq!(
        sandbox_call(
            &w.app,
            &token,
            Method::DELETE,
            &path,
            Some(add("other@example.test"))
        )
        .await
        .status(),
        404
    );
}

#[tokio::test]
async fn sandbox_routes_refuse_other_credentials() {
    let Some(w) = world().await else { return };
    let base = "/api/v1/sandbox/skills";
    assert_eq!(w.app.get(base).send().await.status(), 401);
    assert_eq!(
        w.app.get(base).bearer("not-a-token").send().await.status(),
        401
    );
    let key = w.app.create_api_key(w.admin).await;
    assert_eq!(w.app.get(base).bearer(&key).send().await.status(), 401);
    let mut b = browser(&w.app, w.admin);
    assert_eq!(b.send(w.app.get(base)).await.status(), 401);
}
