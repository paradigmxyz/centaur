//! DB-backed coverage for [`IronControlClient`] and [`SessionRegistrar`]: the
//! writes land through `centaur_console_db` (including grants, invalidation,
//! and effective config).
//! Without `CONSOLE_DB_TEST_DATABASE_URL` it skips locally and fails in CI,
//! matching the rest of the Console database tests.

use std::collections::BTreeMap;
use std::env;

use centaur_iron_control::{
    AwsAuthSecretInput, GcpAuthSecretInput, GcpIdTokenSecretInput, GrantSecret, Grantee,
    IdentityInput, IronControlClient, IronControlError, OAuthTokenSecretInput, PgDsnSecretInput,
    PrincipalInput, ReplaceConfig, RequestRule, SecretSource, SessionRegistrar,
    SlackChannelPermissionInput, StaticSecretInput,
};
use serde_json::{Value, json};
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;

async fn test_pool() -> Option<PgPool> {
    let Ok(url) = env::var("CONSOLE_DB_TEST_DATABASE_URL") else {
        assert!(
            env::var_os("CI").is_none(),
            "CONSOLE_DB_TEST_DATABASE_URL must be set in CI"
        );
        eprintln!("skipping: set CONSOLE_DB_TEST_DATABASE_URL to a database with Console's schema");
        return None;
    };
    Some(
        PgPoolOptions::new()
            .max_connections(2)
            .connect(&url)
            .await
            .expect("connect to the test database"),
    )
}

fn unique() -> String {
    let mut bytes = [0; 16];
    getrandom::fill(&mut bytes).expect("operating system randomness");
    hex::encode(bytes)
}

#[tokio::test]
async fn in_process_client_writes_principal_proxy_and_effective_config() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let unique = unique();
    let client = IronControlClient::new(pool.clone());

    let principal_input = PrincipalInput {
        foreign_id: format!("in-process-principal-{unique}"),
        name: "In-process principal".to_owned(),
        labels: BTreeMap::from([("test".to_owned(), "in-process".to_owned())]),
        kind: Some("slack_channel".to_owned()),
        slack_user_id: None,
        slack_channel_id: Some("C12345678".to_owned()),
        slack_team_id: Some("T12345678".to_owned()),
        slack_email: None,
    };
    let principal = client
        .upsert_principal(&principal_input)
        .await
        .expect("upsert principal in-process");
    assert!(principal.id.starts_with("prn_"));
    // api-rs writes as the Console system user, never as an operator.
    let created_by: String = sqlx::query_scalar(
        "SELECT u.email FROM principals p JOIN users u ON u.id = p.created_by_id \
         WHERE p.foreign_id = $1",
    )
    .bind(&principal_input.foreign_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(created_by, centaur_console_db::SYSTEM_USER_EMAIL);
    assert_eq!(
        principal.foreign_id.as_deref(),
        Some(principal_input.foreign_id.as_str())
    );
    assert_eq!(
        principal.labels.get("test").map(String::as_str),
        Some("in-process")
    );

    // A second refresh must resolve the same row rather than erroring or
    // creating a duplicate.
    let refreshed = client
        .refresh_principal(&principal_input)
        .await
        .expect("refresh principal in-process");
    assert_eq!(refreshed.id, principal.id);

    let permission = SlackChannelPermissionInput {
        channel_id: "C12345678".to_owned(),
        upload_enabled: true,
        download_enabled: true,
        history_enabled: true,
    };
    client
        .upsert_slack_channel_permission(&principal.id, &permission)
        .await
        .expect("write Slack permission in-process");
    client
        .refresh_slack_channel_permission(&principal.id, &permission)
        .await
        .expect("refresh Slack permission in-process");

    let role = client
        .upsert_role(&IdentityInput {
            foreign_id: format!("in-process-role-{unique}"),
            name: "In-process role".to_owned(),
            labels: BTreeMap::new(),
        })
        .await
        .expect("upsert role in-process");
    assert!(role.id.starts_with("role_"));

    let placeholder = format!("IN_PROCESS_TOKEN_{}", unique.to_uppercase());
    let secret = client
        .upsert_static_secret(&StaticSecretInput {
            foreign_id: format!("in-process-secret-{unique}"),
            name: "In-process secret".to_owned(),
            description: None,
            labels: BTreeMap::new(),
            inject_config: None,
            replace_config: Some(ReplaceConfig {
                proxy_value: placeholder.clone(),
                match_headers: vec!["Authorization".to_owned()],
                match_body: false,
                match_path: false,
                match_query: false,
                require: false,
            }),
            source: SecretSource::env(format!("IN_PROCESS_SECRET_{}", unique.to_uppercase())),
            rules: vec![RequestRule::host("api.example.com")],
        })
        .await
        .expect("upsert static secret in-process");
    assert!(secret.id.starts_with("ssr_"));

    let grant = client
        .create_grant(
            &Grantee::Role(role.id.clone()),
            &GrantSecret::Static(secret.id.clone()),
        )
        .await
        .expect("grant secret to role in-process");
    assert!(grant.id.starts_with("grant_"));
    let expected_grant_id = grant.id;
    let role_grants = client
        .list_role_grants(&role.id)
        .await
        .expect("list role grants in-process");
    assert!(
        role_grants
            .iter()
            .any(|grant| grant.id == expected_grant_id)
    );
    client
        .assign_role(&principal.id, &role.id)
        .await
        .expect("assign role in-process");

    let config = client
        .effective_config(&principal.id)
        .await
        .expect("read effective config in-process");
    assert!(
        config.secrets.iter().any(|secret| secret
            .replace
            .as_ref()
            .is_some_and(|replace| replace.proxy_value == placeholder)),
        "effective config should include the granted replace placeholder: {config:?}"
    );

    let proxy = client
        .create_proxy(
            format!("in-process-sandbox-{unique}"),
            principal.id.clone(),
            None,
            BTreeMap::from([("centaur.test".to_owned(), unique.clone())]),
        )
        .await
        .expect("create proxy in-process");
    assert!(proxy.id.starts_with("prx_"));
    assert_eq!(proxy.principal_id, principal.id);
    assert_eq!(proxy.labels.get("centaur.test"), Some(&unique));
    let token = proxy
        .token
        .as_deref()
        .expect("create returns the proxy token");
    assert!(token.starts_with("iprx_"));
    assert_eq!(token.len(), 5 + 64);
    assert!(
        token[5..]
            .chars()
            .all(|character| character.is_ascii_hexdigit() && !character.is_ascii_uppercase())
    );

    let assigned = client
        .assign_proxy_principal(
            &proxy.id,
            &principal.id,
            Some(principal.id.as_str()),
            &BTreeMap::new(),
        )
        .await
        .expect("assign proxy in-process");
    assert_eq!(assigned.id, proxy.id);
    assert_eq!(assigned.principal_id, principal.id);
    assert_eq!(assigned.token, None);

    client
        .delete_proxy(&proxy.id)
        .await
        .expect("delete proxy in-process");
    let gone: bool = sqlx::query_scalar("SELECT NOT EXISTS(SELECT 1 FROM proxies WHERE id = $1)")
        .bind(centaur_console_db::decode_oid("prx", &proxy.id).unwrap())
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(gone);

    let principal_id = centaur_console_db::decode_oid("prn", &principal.id).unwrap();
    let stored_id: i64 = sqlx::query_scalar("SELECT id FROM principals WHERE id = $1")
        .bind(principal_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(centaur_console_db::oid("prn", stored_id), principal.id);
}

#[tokio::test]
async fn in_process_client_upserts_every_secret_kind_the_registrar_uses() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let unique = unique();
    let client = IronControlClient::new(pool.clone());
    let rules = vec![RequestRule::host("api.example.com")];

    let oauth = client
        .upsert_oauth_token_secret(&OAuthTokenSecretInput {
            foreign_id: format!("in-process-oauth-{unique}"),
            name: "OAuth".to_owned(),
            grant: "client_credentials".to_owned(),
            labels: BTreeMap::new(),
            token_endpoint: Some("https://oauth.example.com/token".to_owned()),
            scopes: vec!["read".to_owned()],
            audience: None,
            credentials: BTreeMap::from([
                (
                    "client_id".to_owned(),
                    SecretSource::env(format!("OAUTH_CLIENT_ID_{}", unique.to_uppercase())),
                ),
                (
                    "client_secret".to_owned(),
                    SecretSource::env(format!("OAUTH_CLIENT_SECRET_{}", unique.to_uppercase())),
                ),
            ]),
            token_endpoint_headers: BTreeMap::new(),
            rules: rules.clone(),
        })
        .await
        .expect("upsert oauth token secret in-process");
    assert!(oauth.id.starts_with("ots_"));

    let gcp_auth = client
        .upsert_gcp_auth_secret(&GcpAuthSecretInput {
            foreign_id: Some(format!("in-process-gcp-auth-{unique}")),
            name: Some("GCP Auth".to_owned()),
            labels: BTreeMap::new(),
            scopes: vec!["https://www.googleapis.com/auth/cloud-platform".to_owned()],
            subject: None,
            keyfile: Some(SecretSource::env(format!(
                "GCP_KEYFILE_{}",
                unique.to_uppercase()
            ))),
            credentials_provider: None,
            rules: rules.clone(),
        })
        .await
        .expect("upsert GCP auth secret in-process");
    assert!(gcp_auth.id.starts_with("gas_"));

    let gcp_id = client
        .upsert_gcp_id_token_secret(&GcpIdTokenSecretInput {
            foreign_id: format!("in-process-gcp-id-{unique}"),
            name: Some("GCP ID".to_owned()),
            description: None,
            labels: BTreeMap::new(),
            audience: "https://service.example.com".to_owned(),
            header: None,
            keyfile: SecretSource::env(format!("GCP_KEYFILE_{}", unique.to_uppercase())),
            rules: rules.clone(),
        })
        .await
        .expect("upsert GCP ID token secret in-process");
    assert!(gcp_id.id.starts_with("gid_"));

    let pg = client
        .upsert_pg_dsn_secret(&PgDsnSecretInput {
            foreign_id: format!("in-process-pg-{unique}"),
            name: "Postgres".to_owned(),
            database: "app".to_owned(),
            description: None,
            role: None,
            labels: BTreeMap::new(),
            settings: Vec::new(),
            dsn: SecretSource::env(format!("PG_DSN_{}", unique.to_uppercase())),
        })
        .await
        .expect("upsert Postgres DSN secret in-process");
    assert!(pg.id.starts_with("pgs_"));

    let aws = client
        .upsert_aws_auth_secret(&AwsAuthSecretInput {
            foreign_id: format!("in-process-aws-{unique}"),
            name: Some("AWS".to_owned()),
            description: None,
            labels: BTreeMap::new(),
            access_key_id: SecretSource::env(format!(
                "AWS_ACCESS_KEY_ID_{}",
                unique.to_uppercase()
            )),
            secret_access_key: SecretSource::env(format!(
                "AWS_SECRET_ACCESS_KEY_{}",
                unique.to_uppercase()
            )),
            session_token: None,
            allowed_regions: vec!["us-east-1".to_owned()],
            allowed_services: vec!["logs".to_owned()],
            rules,
        })
        .await
        .expect("upsert AWS auth secret in-process");
    assert!(aws.id.starts_with("aas_"));

    for (table, record) in [
        ("oauth_token_secrets", &oauth),
        ("gcp_auth_secrets", &gcp_auth),
        ("gcp_id_token_secrets", &gcp_id),
        ("pg_dsn_secrets", &pg),
        ("aws_auth_secrets", &aws),
    ] {
        let foreign_id = record
            .foreign_id
            .as_deref()
            .expect("upserts carry a foreign_id");
        let stored: i64 = sqlx::query_scalar(&format!(
            "SELECT count(*) FROM {table} WHERE foreign_id = $1"
        ))
        .bind(foreign_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(stored, 1, "{table} {foreign_id} was written once");
    }
}

/// A registrar, and unique Slack team/user/channel IDs.
async fn registrar(pool: &PgPool) -> (SessionRegistrar, IronControlClient, [String; 3]) {
    let unique = unique();
    let client = IronControlClient::new(pool.clone());
    let suffix = unique[..8].to_uppercase();
    let ids = [
        format!("T{suffix}"),
        format!("U{suffix}"),
        format!("C{suffix}"),
    ];
    (SessionRegistrar::new(client.clone()), client, ids)
}

async fn principal_row(pool: &PgPool, foreign_id: &str) -> Option<(String, Option<String>, Value)> {
    sqlx::query_as("SELECT id::text, slack_email, labels FROM principals WHERE foreign_id = $1")
        .bind(foreign_id)
        .fetch_optional(pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn preapproved_session_rejects_a_missing_principal_without_writing() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let (registrar, _, [team, user, channel]) = registrar(&pool).await;
    let foreign_id = format!(
        "slack-channel-{}-{}",
        team.to_lowercase(),
        channel.to_lowercase()
    );

    let error = registrar
        .resolve_session(
            &format!("slack:{team}:{channel}:1773364194.179929"),
            Some(&json!({ "slack_user_id": user, "slack_team_id": team })),
            false,
        )
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        IronControlError::SessionPrincipalNotPreapproved { foreign_id: ref missing }
            if *missing == foreign_id
    ));
    assert!(
        principal_row(&pool, &foreign_id).await.is_none(),
        "preapproved admission must not create the principal"
    );
}

#[tokio::test]
async fn existing_dm_principal_refreshes_its_permission() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let (registrar, client, [team, user, _]) = registrar(&pool).await;
    let dm = format!("D{}", &user[1..]);
    let foreign_id = format!("slack-user-{}-{}", team.to_lowercase(), user.to_lowercase());
    let existing = client
        .upsert_principal(&PrincipalInput {
            foreign_id: foreign_id.clone(),
            name: "Existing DM user".to_owned(),
            labels: BTreeMap::new(),
            kind: Some("slack_dm".to_owned()),
            slack_user_id: Some(user.clone()),
            slack_channel_id: None,
            slack_team_id: Some(team.clone()),
            slack_email: None,
        })
        .await
        .expect("create the DM principal");

    let record = registrar
        .register_session(
            &format!("slack:{team}:{dm}:1773364194.179929"),
            Some(&json!({ "slack_user_id": user, "slack_team_id": team })),
        )
        .await
        .expect("register DM session");

    assert_eq!(record.id, existing.id);
    let permission: (bool, bool, bool) = sqlx::query_as(
        "SELECT p.upload_enabled, p.download_enabled, p.history_enabled \
         FROM slack_channel_permissions p JOIN principals pr ON pr.id = p.principal_id \
         WHERE pr.foreign_id = $1 AND p.channel_id = $2",
    )
    .bind(&foreign_id)
    .bind(&dm)
    .fetch_one(&pool)
    .await
    .expect("the DM permission is written on the principal");
    assert_eq!(permission, (true, true, true));
}

#[tokio::test]
async fn unknown_console_requester_is_an_error_not_an_omitted_requester() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let (registrar, _, _) = registrar(&pool).await;

    let error = registrar
        .register_requester(
            "console:9f1b7a3c-2d4e-4f6a-8b0c-1d2e3f4a5b6c",
            Some(&json!({ "requester_principal_foreign_id": format!("console-user-ghost-{}", unique()) })),
        )
        .await
        .expect_err("a console requester the console never provisioned must fail");

    assert!(error.is_not_found(), "{error:?}");
}

#[tokio::test]
async fn channel_requester_upsert_keeps_existing_labels_and_sets_email() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let (registrar, client, [team, user, channel]) = registrar(&pool).await;
    let foreign_id = format!("slack-user-{}-{}", team.to_lowercase(), user.to_lowercase());
    let existing = client
        .upsert_principal(&PrincipalInput {
            foreign_id: foreign_id.clone(),
            name: "Existing requester".to_owned(),
            labels: BTreeMap::from([("team".to_owned(), "finance".to_owned())]),
            kind: Some("slack_dm".to_owned()),
            slack_user_id: Some(user.clone()),
            slack_channel_id: None,
            slack_team_id: Some(team.clone()),
            slack_email: None,
        })
        .await
        .expect("create the requester principal");

    let requester = registrar
        .register_requester(
            &format!("slack:{team}:{channel}:1773364194.179929"),
            Some(&json!({
                "slack_user_id": user,
                "slack_team_id": team,
                "slack_home_team_id": team,
                "slack_user_email": " ada@example.com "
            })),
        )
        .await
        .expect("register requester")
        .expect("home-team channel requester resolves");

    assert_eq!(requester.id, existing.id);
    let (_, email, labels) = principal_row(&pool, &foreign_id).await.unwrap();
    assert_eq!(email.as_deref(), Some("ada@example.com"));
    assert_eq!(labels["team"], "finance");
    assert_eq!(labels["managed-by"], "centaur");
}
