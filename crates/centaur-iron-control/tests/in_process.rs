//! DB-backed coverage for the in-process iron-control path.
//!
//! The test drives the same [`IronControlClient`] callers use, with the
//! writable Console pool configured, and asserts the writes land through
//! `centaur_console_db` (including grants, invalidation, and effective config).
//! Without `CONSOLE_DB_TEST_DATABASE_URL` it skips locally and fails in CI,
//! matching the rest of the Console database tests.

use std::collections::BTreeMap;
use std::env;

use centaur_iron_control::{
    AwsAuthSecretInput, GcpAuthSecretInput, GcpIdTokenSecretInput, GrantSecret, Grantee,
    IdentityInput, IronControlClient, OAuthTokenSecretInput, PgDsnSecretInput, PrincipalInput,
    ReplaceConfig, RequestRule, SecretSource, SlackChannelPermissionInput, StaticSecretInput,
};
use sha2::{Digest, Sha256};
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

async fn insert_user(pool: &PgPool, email: &str) -> i64 {
    sqlx::query_scalar(
        "INSERT INTO users (email, admin, status, created_at, updated_at) \
         VALUES ($1, true, 'active', now(), now()) RETURNING id",
    )
    .bind(email)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn insert_api_key(pool: &PgPool, user_id: i64, token: &str) {
    let token_hash = hex::encode(Sha256::digest(token.as_bytes()));
    sqlx::query(
        "INSERT INTO api_keys (name, token_hash, user_id, created_at, updated_at) \
         VALUES ($1, $2, $3, now(), now())",
    )
    .bind(format!("in-process-{token_hash}"))
    .bind(&token_hash)
    .bind(user_id)
    .execute(pool)
    .await
    .unwrap();
}

#[tokio::test]
async fn in_process_client_writes_principal_proxy_and_effective_config() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let unique = unique();
    let token = format!("iak_{unique}");
    let admin = insert_user(&pool, &format!("in-process-{unique}@example.com")).await;
    insert_api_key(&pool, admin, &token).await;

    // The URL is deliberately unreachable: every successful call below proves
    // it did not use the HTTP admin API.
    let client =
        IronControlClient::new("http://127.0.0.1:1", &token).with_console_write_db(pool.clone());
    assert!(client.is_in_process());

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
    assert_eq!(
        principal.foreign_id.as_deref(),
        Some(principal_input.foreign_id.as_str())
    );
    assert_eq!(
        principal.labels.get("test").map(String::as_str),
        Some("in-process")
    );

    // A second refresh must resolve the same row rather than erroring or
    // creating a duplicate through the unreachable HTTP URL.
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
    let token = format!("iak_{unique}");
    let admin = insert_user(&pool, &format!("in-process-secrets-{unique}@example.com")).await;
    insert_api_key(&pool, admin, &token).await;
    let client =
        IronControlClient::new("http://127.0.0.1:1", &token).with_console_write_db(pool.clone());
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

    for (collection, oid) in [
        ("oauth_token_secrets", oauth.id),
        ("gcp_auth_secrets", gcp_auth.id),
        ("gcp_id_token_secrets", gcp_id.id),
        ("pg_dsn_secrets", pg.id),
        ("aws_auth_secrets", aws.id),
    ] {
        let found = client
            .get_secret(collection, &oid)
            .await
            .unwrap_or_else(|error| panic!("read {collection} {oid}: {error}"));
        assert_eq!(found.id, oid);
        let listed = client
            .list_secrets(
                collection,
                &[("managed-by".to_owned(), "centaur".to_owned())],
            )
            .await
            .unwrap_or_else(|error| panic!("list {collection}: {error}"));
        assert!(
            listed.iter().all(|secret| secret.id != oid),
            "these test secrets carry no managed-by label"
        );
    }
}
