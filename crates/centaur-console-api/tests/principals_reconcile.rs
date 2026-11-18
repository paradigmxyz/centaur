//! Credential reconciliation and console-user provisioning: which
//! OAuth-flow credentials reach which principals, and what a principal
//! write, a new credential, or a new Slack identity triggers.

mod principals_support;

use axum::http::Method;
use centaur_console_api::principals::{
    effects,
    provision::{self, ConsoleUser},
    reconcile,
};
use principals_support::Fx;
use serde_json::{Value, json};

const SLACK_USER: &str = "U00000001";
const SLACK_TEAM: &str = "T00000001";

struct Apps {
    slack: i64,
    google: i64,
    github: i64,
    shared_google: i64,
}

async fn apps(fx: &Fx) -> Apps {
    let admin = fx.admin.id;
    let app = |slug: &str, provider: &str, shared: bool, labels: &str| {
        format!(
            "INSERT INTO oauth_apps (slug, provider, client_id, shared, labels, created_by_id, created_at, updated_at) \
             VALUES ('{slug}', '{provider}', '{slug}-client', {shared}, '{labels}', {admin}, now(), now()) RETURNING id"
        )
    };
    Apps {
        slack: fx.scalar_i64(&app("slack", "slack", false, "{}")).await,
        google: fx.scalar_i64(&app("google", "google", false, "{}")).await,
        github: fx.scalar_i64(&app("github", "github", false, "{}")).await,
        shared_google: fx
            .scalar_i64(&app("shared-google", "google", true, "{}"))
            .await,
    }
}

/// A credential minted through `app`, without its wrapper secret.
async fn credential_only(
    fx: &Fx,
    app: Option<i64>,
    subject: &str,
    email: Option<&str>,
    created_by: Option<i64>,
    labels: Value,
) -> i64 {
    let value = |v: Option<String>| v.map_or("NULL".to_owned(), |v| format!("'{v}'"));
    fx.scalar_i64(&format!(
        "INSERT INTO broker_credentials (oauth_app_id, provider_subject, provider_email, created_by_id, labels, \
         token_endpoint, created_at, updated_at) VALUES ({}, '{subject}', {}, {}, '{labels}', 'https://idp.test/token', now(), now()) RETURNING id",
        app.map_or("NULL".to_owned(), |app| app.to_string()),
        value(email.map(str::to_owned)),
        created_by.map_or("NULL".to_owned(), |id| id.to_string()),
    ))
    .await
}

async fn wrapper(fx: &Fx, credential: i64) -> i64 {
    fx.scalar_i64(&format!(
        "INSERT INTO static_secrets (name, broker_credential_id, created_at, updated_at) \
         VALUES ('wrapper {credential}', {credential}, now(), now()) RETURNING id"
    ))
    .await
}

/// A credential with its wrapper, applied as the secrets/OAuth areas do
/// once a wrapper exists. Returns `(credential, wrapper secret)`.
async fn credential(
    fx: &Fx,
    app: i64,
    subject: &str,
    email: Option<&str>,
    created_by: Option<i64>,
    labels: Value,
) -> (i64, i64) {
    let credential = credential_only(fx, Some(app), subject, email, created_by, labels).await;
    let secret = wrapper(fx, credential).await;
    effects::credential_changed(&fx.app.state, credential).await;
    (credential, secret)
}

async fn granted(fx: &Fx, principal: &str, secret: i64) -> bool {
    let id = centaur_console_db::decode_oid("prn", principal).unwrap();
    fx.scalar_bool(&format!(
        "SELECT EXISTS (SELECT 1 FROM grants WHERE principal_id = {id} AND static_secret_id = {secret})"
    ))
    .await
}

async fn put_principal(fx: &Fx, foreign_id: &str, data: Value) -> String {
    let response = fx
        .bearer(
            Method::PUT,
            &format!("/principals/{foreign_id}"),
            Some(json!({"data": data})),
        )
        .await;
    assert!(
        matches!(response.status(), 200 | 201),
        "{}",
        response.text()
    );
    response.json()["data"]["id"].as_str().unwrap().to_owned()
}

async fn labels(fx: &Fx, principal: &str) -> Value {
    fx.bearer(Method::GET, &format!("/principals/{principal}"), None)
        .await
        .json()["data"]["labels"]
        .clone()
}

async fn slack_identity(fx: &Fx, user: i64, subject: &str, team: Option<&str>) {
    fx.exec(&format!(
        "INSERT INTO user_identities (user_id, provider, subject, team_id, email_verified, created_at, updated_at) \
         VALUES ({user}, 'slack', '{subject}', {}, true, now(), now())",
        team.map_or("NULL".to_owned(), |t| format!("'{t}'"))
    ))
    .await;
}

#[tokio::test]
async fn a_new_credential_is_granted_to_the_principals_it_matches() {
    let Some(fx) = Fx::new().await else { return };
    let apps = apps(&fx).await;
    let alice = put_principal(
        &fx,
        "alice",
        json!({"kind": "user", "slack_user_id": SLACK_USER, "slack_team_id": SLACK_TEAM,
               "labels": {"google_email": "Alice@Example.test"}}),
    )
    .await;
    let other_user = put_principal(
        &fx,
        "bob",
        json!({"kind": "user", "slack_user_id": "U00000002", "slack_team_id": SLACK_TEAM}),
    )
    .await;
    let other_team = put_principal(
        &fx,
        "carol",
        json!({"kind": "user", "slack_user_id": SLACK_USER, "slack_team_id": "T00000002"}),
    )
    .await;
    let version = fx.version(&alice).await;

    let (_, slack_secret) = credential(
        &fx,
        apps.slack,
        "u00000001",
        None,
        None,
        json!({"slack_team_id": SLACK_TEAM}),
    )
    .await;
    assert!(granted(&fx, &alice, slack_secret).await);
    assert!(
        !granted(&fx, &other_user, slack_secret).await,
        "another Slack user"
    );
    assert!(
        !granted(&fx, &other_team, slack_secret).await,
        "another workspace"
    );
    assert_eq!(
        fx.version(&alice).await,
        version + 1,
        "a grant refreshes the principal's config"
    );
    assert_eq!(
        fx.scalar_i64("SELECT priority::bigint FROM grants LIMIT 1")
            .await,
        100
    );

    // Google matches the email label (case-insensitively) and teaches the
    // principal the credential's subject.
    let (_, google_secret) = credential(
        &fx,
        apps.google,
        "google-sub-1",
        Some("alice@example.test"),
        None,
        json!({}),
    )
    .await;
    assert!(granted(&fx, &alice, google_secret).await);
    assert!(!granted(&fx, &other_user, google_secret).await);
    let alice_labels = labels(&fx, &alice).await;
    assert_eq!(alice_labels["google_subject"], "google-sub-1");
    assert_eq!(
        alice_labels["google_email"], "Alice@Example.test",
        "an existing label is kept"
    );

    // Running it again changes nothing.
    let grants = fx.scalar_i64("SELECT COUNT(*) FROM grants").await;
    effects::credential_changed(&fx.app.state, 1).await;
    assert_eq!(fx.scalar_i64("SELECT COUNT(*) FROM grants").await, grants);
}

#[tokio::test]
async fn shared_apps_and_credentials_without_an_app_never_match() {
    let Some(fx) = Fx::new().await else { return };
    let apps = apps(&fx).await;
    let alice = put_principal(
        &fx,
        "alice",
        json!({"kind": "user", "labels": {"google_email": "alice@example.test"}}),
    )
    .await;
    let (_, shared) = credential(
        &fx,
        apps.shared_google,
        "s",
        Some("alice@example.test"),
        None,
        json!({}),
    )
    .await;
    assert!(!granted(&fx, &alice, shared).await);
    let orphan = credential_only(&fx, None, "o", Some("alice@example.test"), None, json!({})).await;
    let orphan_secret = wrapper(&fx, orphan).await;
    let outcome = reconcile::apply_for_credential(&fx.app.state.db, fx.app.now(), orphan)
        .await
        .unwrap();
    assert_eq!((outcome.requested, outcome.created), (0, 0));
    assert!(!granted(&fx, &alice, orphan_secret).await);
}

#[tokio::test]
async fn writing_a_principal_grants_credentials_that_already_exist() {
    let Some(fx) = Fx::new().await else { return };
    let apps = apps(&fx).await;
    let (_, google_secret) = credential(
        &fx,
        apps.google,
        "g1",
        Some("dana@example.test"),
        None,
        json!({}),
    )
    .await;
    // The principal is not a user yet: no labels, no Slack identity.
    let dana = put_principal(&fx, "dana", json!({"name": "Dana"})).await;
    assert!(!granted(&fx, &dana, google_secret).await);
    // An email label makes it a user and brings the grant in.
    put_principal(
        &fx,
        "dana",
        json!({"labels": {"email": "dana@example.test"}}),
    )
    .await;
    assert!(granted(&fx, &dana, google_secret).await);
    assert_eq!(labels(&fx, &dana).await["google_subject"], "g1");
}

#[tokio::test]
async fn a_provider_native_identity_wins_over_email() {
    let Some(fx) = Fx::new().await else { return };
    let apps = apps(&fx).await;
    let eve = put_principal(
        &fx,
        "eve",
        json!({"kind": "user", "labels": {"google_subject": "sub-eve", "google_email": "eve@example.test"}}),
    )
    .await;
    let (_, by_subject) = credential(
        &fx,
        apps.google,
        "SUB-EVE",
        Some("someone@else.test"),
        None,
        json!({}),
    )
    .await;
    let (_, by_email) = credential(
        &fx,
        apps.google,
        "sub-other",
        Some("eve@example.test"),
        None,
        json!({}),
    )
    .await;
    assert!(granted(&fx, &eve, by_subject).await);
    assert!(
        !granted(&fx, &eve, by_email).await,
        "with a subject, the email does not count"
    );

    let gina = put_principal(
        &fx,
        "gina",
        json!({"kind": "user", "labels": {"github_subject": "4242"}}),
    )
    .await;
    let (_, github) = credential(&fx, apps.github, "4242", None, None, json!({})).await;
    let (_, other_github) = credential(&fx, apps.github, "9999", None, None, json!({})).await;
    assert!(granted(&fx, &gina, github).await);
    assert!(!granted(&fx, &gina, other_github).await);
}

#[tokio::test]
async fn a_credential_reaches_a_principal_through_its_owners_slack_identity() {
    let Some(fx) = Fx::new().await else { return };
    let apps = apps(&fx).await;
    let owner = fx
        .app
        .create_user("owner@example.test", false, "active")
        .await;
    slack_identity(&fx, owner.id, SLACK_USER, Some(SLACK_TEAM)).await;
    let person = put_principal(
        &fx,
        "person",
        json!({"kind": "slack_dm", "slack_user_id": SLACK_USER, "slack_team_id": SLACK_TEAM}),
    )
    .await;
    let stranger = put_principal(
        &fx,
        "stranger",
        json!({"kind": "slack_dm", "slack_user_id": "U00000003", "slack_team_id": SLACK_TEAM}),
    )
    .await;
    // A GitHub credential carries no usable identity of its own.
    let (_, github) = credential(
        &fx,
        apps.github,
        "gh-owner",
        None,
        Some(owner.id),
        json!({}),
    )
    .await;
    assert!(granted(&fx, &person, github).await);
    assert!(!granted(&fx, &stranger, github).await);

    // An owner with two Slack identities is ambiguous, one without a
    // workspace is incomplete: neither identifies anyone.
    let ambiguous = fx
        .app
        .create_user("ambiguous@example.test", false, "active")
        .await;
    slack_identity(&fx, ambiguous.id, "U00000004", Some("T00000004")).await;
    slack_identity(&fx, ambiguous.id, "U00000009", Some("T00000009")).await;
    let incomplete = fx
        .app
        .create_user("incomplete@example.test", false, "active")
        .await;
    slack_identity(&fx, incomplete.id, "U00000008", None).await;
    for (user, slack_user, slack_team) in [
        (ambiguous.id, "U00000004", "T00000004"),
        (incomplete.id, "U00000008", "T00000008"),
    ] {
        let principal = put_principal(
            &fx,
            &format!("dm-{slack_user}"),
            json!({"kind": "slack_dm", "slack_user_id": slack_user, "slack_team_id": slack_team}),
        )
        .await;
        let (_, secret) = credential(
            &fx,
            apps.github,
            &format!("gh-{user}"),
            None,
            Some(user),
            json!({}),
        )
        .await;
        assert!(!granted(&fx, &principal, secret).await, "user {user}");
    }
}

#[tokio::test]
async fn owner_credentials_are_reconciled_when_the_slack_identity_arrives_last() {
    let Some(fx) = Fx::new().await else { return };
    let apps = apps(&fx).await;
    let owner = fx
        .app
        .create_user("late@example.test", false, "active")
        .await;
    let person = put_principal(
        &fx,
        "late",
        json!({"kind": "slack_dm", "slack_user_id": SLACK_USER, "slack_team_id": SLACK_TEAM}),
    )
    .await;
    let (_, github) =
        credential(&fx, apps.github, "gh-late", None, Some(owner.id), json!({})).await;
    assert!(!granted(&fx, &person, github).await);
    slack_identity(&fx, owner.id, SLACK_USER, Some(SLACK_TEAM)).await;
    effects::slack_identity_linked(&fx.app.state, owner.id).await;
    assert!(granted(&fx, &person, github).await);
}

#[tokio::test]
async fn console_user_principals_match_only_through_their_user() {
    let Some(fx) = Fx::new().await else { return };
    let apps = apps(&fx).await;
    let user = fx
        .app
        .create_user("pat@example.test", false, "active")
        .await;
    fx.exec(&format!(
        "INSERT INTO user_identities (user_id, provider, subject, email, email_verified, created_at, updated_at) VALUES \
         ({id}, 'google', 'g-pat', 'pat@work.test', true, now(), now()), \
         ({id}, 'github', 'gh-pat', 'pat@unverified.test', false, now(), now())",
        id = user.id
    ))
    .await;
    let principal = provision::provision(
        &fx.app.state,
        &ConsoleUser {
            id: user.id,
            email: &user.email,
            name: Some("Pat"),
        },
    )
    .await
    .unwrap();
    let principal_oid = centaur_console_db::oid("prn", principal);
    // Labels never widen a console-user principal's reach.
    fx.exec(&format!(
        "UPDATE principals SET labels = labels || '{{\"google_email\": \"elsewhere@example.test\", \"google_subject\": \"g-else\"}}' WHERE id = {principal}"
    ))
    .await;

    let (_, owned) = credential(
        &fx,
        apps.google,
        "any-subject",
        None,
        Some(user.id),
        json!({}),
    )
    .await;
    let (_, by_email) = credential(
        &fx,
        apps.google,
        "x1",
        Some("PAT@example.test"),
        None,
        json!({}),
    )
    .await;
    let (_, by_verified) = credential(
        &fx,
        apps.google,
        "x2",
        Some("pat@work.test"),
        None,
        json!({}),
    )
    .await;
    let (_, unverified) = credential(
        &fx,
        apps.google,
        "x3",
        Some("pat@unverified.test"),
        None,
        json!({}),
    )
    .await;
    let (_, by_label) = credential(
        &fx,
        apps.google,
        "g-else",
        Some("elsewhere@example.test"),
        None,
        json!({}),
    )
    .await;
    let someone_else = fx
        .app
        .create_user("else@example.test", false, "active")
        .await;
    let (_, not_owned) = credential(
        &fx,
        apps.google,
        "x4",
        None,
        Some(someone_else.id),
        json!({}),
    )
    .await;
    assert!(granted(&fx, &principal_oid, owned).await);
    assert!(granted(&fx, &principal_oid, by_email).await);
    assert!(granted(&fx, &principal_oid, by_verified).await);
    assert!(
        !granted(&fx, &principal_oid, unverified).await,
        "unverified emails adopt nothing"
    );
    assert!(!granted(&fx, &principal_oid, by_label).await);
    assert!(!granted(&fx, &principal_oid, not_owned).await);
    // Google credentials never fill a console-user principal's labels.
    assert!(
        labels(&fx, &principal_oid)
            .await
            .get("google_subject")
            .is_some_and(|v| v == "g-else")
    );
}

#[tokio::test]
async fn a_console_user_principal_learns_a_unique_well_formed_slack_identity() {
    let Some(fx) = Fx::new().await else { return };
    let apps = apps(&fx).await;
    let user = fx
        .app
        .create_user("sam@example.test", false, "active")
        .await;
    let principal = provision::provision(
        &fx.app.state,
        &ConsoleUser {
            id: user.id,
            email: &user.email,
            name: None,
        },
    )
    .await
    .unwrap();
    let slack_of = |fx: &Fx| {
        let sql = format!(
            "SELECT slack_user_id || '/' || slack_team_id FROM principals WHERE id = {principal}"
        );
        let fx = fx.app.db.pool.clone();
        async move {
            sqlx::query_scalar::<_, Option<String>>(&sql)
                .fetch_one(&fx)
                .await
                .unwrap()
        }
    };
    // Malformed or ambiguous: nothing is learned.
    credential(
        &fx,
        apps.slack,
        "not-a-slack-id",
        None,
        Some(user.id),
        json!({"slack_team_id": SLACK_TEAM}),
    )
    .await;
    credential(
        &fx,
        apps.slack,
        "U00000002",
        None,
        Some(user.id),
        json!({"slack_team_id": "bad-team"}),
    )
    .await;
    assert_eq!(slack_of(&fx).await, None);
    credential(
        &fx,
        apps.slack,
        SLACK_USER,
        None,
        Some(user.id),
        json!({"slack_team_id": SLACK_TEAM}),
    )
    .await;
    assert_eq!(
        slack_of(&fx).await.as_deref(),
        Some(&*format!("{SLACK_USER}/{SLACK_TEAM}"))
    );
    let before = fx.version(&centaur_console_db::oid("prn", principal)).await;
    effects::credential_changed(&fx.app.state, 3).await;
    assert_eq!(
        fx.version(&centaur_console_db::oid("prn", principal)).await,
        before,
        "an unchanged identity is not rewritten"
    );
}

// --- provisioning -------------------------------------------------------------------

#[tokio::test]
async fn provisioning_creates_one_principal_with_the_user_mcp_role_and_reuses_it() {
    let Some(fx) = Fx::new().await else { return };
    fx.exec(&format!(
        "INSERT INTO roles (foreign_id, assign_by_default, created_by_id, created_at, updated_at) \
         VALUES ('everyone', true, {}, now(), now())",
        fx.admin.id
    ))
    .await;
    let user = fx
        .app
        .create_user("Pro.Visioned@Example.test", false, "active")
        .await;
    let account = ConsoleUser {
        id: user.id,
        email: &user.email,
        name: Some("Pro V"),
    };
    let foreign_id = provision::console_user_foreign_id(&user.email);

    let id = provision::provision(&fx.app.state, &account).await.unwrap();
    let row: (String, String, Option<String>, i64, Value) = sqlx::query_as(
        "SELECT foreign_id, kind, name, console_user_id, labels FROM principals WHERE id = $1",
    )
    .bind(id)
    .fetch_one(&fx.app.db.pool)
    .await
    .unwrap();
    assert_eq!(row.0, foreign_id);
    assert_eq!(
        (row.1.as_str(), row.2.as_deref(), row.3),
        ("console_user", Some("Pro V"), user.id)
    );
    assert_eq!(
        row.4,
        json!({"managed-by": "centaur", "centaur.sandbox_repo_cache": "all"})
    );
    let roles: Vec<String> = sqlx::query_scalar(
        "SELECT r.foreign_id FROM principal_roles pr JOIN roles r ON r.id = pr.role_id WHERE pr.principal_id = $1 ORDER BY r.foreign_id",
    )
    .bind(id)
    .fetch_all(&fx.app.db.pool)
    .await
    .unwrap();
    assert_eq!(roles, ["everyone", "user-mcp"]);

    // Again: nothing is written.
    fx.exec(&format!(
        "UPDATE principals SET updated_at = '2001-01-01' WHERE id = {id}"
    ))
    .await;
    let version = fx
        .scalar_i64(&format!(
            "SELECT sync_config_cache_version FROM principals WHERE id = {id}"
        ))
        .await;
    assert_eq!(
        provision::provision(&fx.app.state, &account).await.unwrap(),
        id
    );
    assert!(
        fx.scalar_bool(&format!(
            "SELECT updated_at = '2001-01-01' FROM principals WHERE id = {id}"
        ))
        .await
    );
    assert_eq!(
        fx.scalar_i64(&format!(
            "SELECT sync_config_cache_version FROM principals WHERE id = {id}"
        ))
        .await,
        version
    );
    assert_eq!(
        fx.scalar_i64("SELECT COUNT(*) FROM principals WHERE kind = 'console_user'")
            .await,
        1
    );
    assert_eq!(
        fx.scalar_i64("SELECT COUNT(*) FROM roles WHERE foreign_id = 'user-mcp'")
            .await,
        1
    );

    // A renamed user updates the principal and refreshes its config.
    let renamed = ConsoleUser {
        name: Some("Renamed"),
        ..account
    };
    provision::provision(&fx.app.state, &renamed).await.unwrap();
    assert_eq!(
        fx.scalar_string(&format!("SELECT name FROM principals WHERE id = {id}"))
            .await
            .as_deref(),
        Some("Renamed")
    );
    assert_eq!(
        fx.scalar_i64(&format!(
            "SELECT sync_config_cache_version FROM principals WHERE id = {id}"
        ))
        .await,
        version + 1
    );
}

#[tokio::test]
async fn provisioning_copies_an_unambiguous_slack_identity_and_ignores_an_ambiguous_one() {
    let Some(fx) = Fx::new().await else { return };
    let clear = fx
        .app
        .create_user("clear@example.test", false, "active")
        .await;
    slack_identity(&fx, clear.id, SLACK_USER, Some(SLACK_TEAM)).await;
    let id = provision::provision(
        &fx.app.state,
        &ConsoleUser {
            id: clear.id,
            email: &clear.email,
            name: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(
        fx.scalar_string(&format!(
            "SELECT slack_user_id || '/' || slack_team_id FROM principals WHERE id = {id}"
        ))
        .await
        .as_deref(),
        Some(&*format!("{SLACK_USER}/{SLACK_TEAM}"))
    );
    assert_eq!(
        fx.scalar_string(&format!("SELECT name FROM principals WHERE id = {id}"))
            .await
            .as_deref(),
        Some("clear@example.test")
    );

    let two = fx
        .app
        .create_user("two@example.test", false, "active")
        .await;
    slack_identity(&fx, two.id, "U00000005", Some("T00000005")).await;
    slack_identity(&fx, two.id, "U00000006", Some("T00000006")).await;
    let id = provision::provision(
        &fx.app.state,
        &ConsoleUser {
            id: two.id,
            email: &two.email,
            name: None,
        },
    )
    .await
    .unwrap();
    assert!(
        fx.scalar_bool(&format!(
            "SELECT slack_user_id IS NULL FROM principals WHERE id = {id}"
        ))
        .await
    );
}

#[tokio::test]
async fn provisioning_grants_matching_credentials() {
    let Some(fx) = Fx::new().await else { return };
    let apps = apps(&fx).await;
    let user = fx
        .app
        .create_user("owner2@example.test", false, "active")
        .await;
    let (_, secret) = credential(&fx, apps.google, "g", None, Some(user.id), json!({})).await;
    let id = provision::provision(
        &fx.app.state,
        &ConsoleUser {
            id: user.id,
            email: &user.email,
            name: None,
        },
    )
    .await
    .unwrap();
    assert!(granted(&fx, &centaur_console_db::oid("prn", id), secret).await);
}

/// What the scheduled-task claim does: provision the author's principal in
/// its own transaction, and only after that commits run the follow-ups.
#[tokio::test]
async fn a_provisioning_inside_a_larger_transaction_reconciles_after_it_commits() {
    let Some(fx) = Fx::new().await else { return };
    let apps = apps(&fx).await;
    let author = fx
        .app
        .create_user("author@example.test", false, "active")
        .await;
    let (_, secret) = credential(
        &fx,
        apps.google,
        "g-author",
        None,
        Some(author.id),
        json!({}),
    )
    .await;
    let account = ConsoleUser {
        id: author.id,
        email: &author.email,
        name: None,
    };
    let foreign_id = provision::console_user_foreign_id(&author.email);
    let principal_of = || {
        let sql = format!("SELECT id FROM principals WHERE foreign_id = '{foreign_id}'");
        let pool = fx.app.db.pool.clone();
        async move {
            sqlx::query_scalar::<_, i64>(&sql)
                .fetch_optional(&pool)
                .await
                .unwrap()
        }
    };

    // A rolled-back claim leaves no principal and no grant.
    let mut tx = fx.app.db.pool.begin().await.unwrap();
    provision::provision_in(&mut tx, &account, fx.app.now())
        .await
        .unwrap();
    tx.rollback().await.unwrap();
    assert_eq!(principal_of().await, None);

    let mut tx = fx.app.db.pool.begin().await.unwrap();
    let provisioned = provision::provision_in(&mut tx, &account, fx.app.now())
        .await
        .unwrap();
    assert!(provisioned.created && provisioned.saved);
    let oid = centaur_console_db::oid("prn", provisioned.principal_id);
    assert!(
        !granted(&fx, &oid, secret).await,
        "nothing is reconciled before the commit"
    );
    tx.commit().await.unwrap();
    effects::after_write(
        &fx.app.db.pool,
        fx.app.now(),
        provisioned.principal_id,
        true,
    )
    .await;
    assert!(granted(&fx, &oid, secret).await);

    // A second claim finds the principal unchanged.
    let mut tx = fx.app.db.pool.begin().await.unwrap();
    let again = provision::provision_in(&mut tx, &account, fx.app.now())
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert!(!again.saved && !again.created);
    assert_eq!(again.principal_id, provisioned.principal_id);
}
