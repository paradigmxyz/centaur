//! Unit tests for pyproject parsing, translation, and overlay resolution.

use std::fs;
use std::path::{Path, PathBuf};

use centaur_iron_control::SecretInput;
use centaur_iron_proxy::{SourcePolicy, pg_sandbox_env_var};

use crate::tools::{self, ParsedSecret, SecretMode};
use crate::translate;

fn entry(toml_src: &str) -> toml::Value {
    let v: toml::Value = toml::from_str(&format!("x = {toml_src}")).expect("valid toml");
    v.get("x").expect("x key").clone()
}

// ----- secrets routing -----------------------------------------------------

#[test]
fn secret_type_routes_by_oid_prefix() {
    use crate::secret_type_for_oid;
    assert_eq!(
        secret_type_for_oid("ssr_1").map(|t| t.1),
        Some("static_secrets")
    );
    assert_eq!(
        secret_type_for_oid("ots_2").map(|t| t.1),
        Some("oauth_token_secrets")
    );
    assert_eq!(
        secret_type_for_oid("gas_3").map(|t| t.1),
        Some("gcp_auth_secrets")
    );
    assert_eq!(
        secret_type_for_oid("gid_7").map(|t| t.1),
        Some("gcp_id_token_secrets")
    );
    assert_eq!(secret_type_for_oid("pgs_4").map(|t| t.0), Some("pg_dsn"));
    assert_eq!(secret_type_for_oid("hms_5").map(|t| t.0), Some("hmac"));
    assert_eq!(
        secret_type_for_oid("aas_6").map(|t| t.1),
        Some("aws_auth_secrets")
    );
    // A bare foreign_id is not an OID — callers fall back to lookup.
    assert!(secret_type_for_oid("falconx-hmac").is_none());
}

// ----- parsing -------------------------------------------------------------

#[test]
fn parses_http_replace_secret() {
    let parsed = tools::parse_secret(
        &entry(r#"{type = "http", name = "SLACK_BOT_TOKEN", match_headers = ["Authorization"], hosts = ["slack.com"]}"#),
        &[],
    )
    .unwrap();
    let ParsedSecret::Http(http) = parsed else {
        panic!("expected http")
    };
    assert_eq!(http.name, "SLACK_BOT_TOKEN");
    assert_eq!(http.secret_ref, "SLACK_BOT_TOKEN");
    assert_eq!(http.mode, SecretMode::Replace);
    assert_eq!(http.replacer, "SLACK_BOT_TOKEN");
    assert_eq!(http.match_headers, vec!["Authorization".to_owned()]);
    assert_eq!(http.hosts, vec!["slack.com".to_owned()]);
}

#[test]
fn http_inherits_tool_level_hosts() {
    let parsed = tools::parse_secret(
        &entry(r#"{type = "http", name = "PARALLEL_API_KEY", match_headers = ["x-api-key"]}"#),
        &[
            "api.parallel.ai".to_owned(),
            "search.parallel.ai".to_owned(),
        ],
    )
    .unwrap();
    let ParsedSecret::Http(http) = parsed else {
        panic!("expected http")
    };
    assert_eq!(
        http.hosts,
        vec![
            "api.parallel.ai".to_owned(),
            "search.parallel.ai".to_owned()
        ]
    );
}

#[test]
fn parses_inject_secret() {
    let parsed = tools::parse_secret(
        &entry(r#"{type = "http", name = "TOK", mode = "inject", inject_header = "Authorization", inject_formatter = "Bearer {{.Value}}", hosts = ["api.example.com"]}"#),
        &[],
    )
    .unwrap();
    let ParsedSecret::Http(http) = parsed else {
        panic!("expected http")
    };
    assert_eq!(http.mode, SecretMode::Inject);
    assert_eq!(http.inject_header.as_deref(), Some("Authorization"));
    assert_eq!(http.inject_formatter.as_deref(), Some("Bearer {{.Value}}"));
}

#[test]
fn inject_secret_requires_exactly_one_target() {
    let err = tools::parse_secret(
        &entry(r#"{type = "http", name = "TOK", mode = "inject", hosts = ["api.example.com"]}"#),
        &[],
    )
    .unwrap_err();
    assert!(err.to_string().contains("exactly one"), "{err}");
}

#[test]
fn replace_secret_requires_a_scan_location() {
    let err = tools::parse_secret(
        &entry(r#"{type = "http", name = "TOK", hosts = ["api.example.com"]}"#),
        &[],
    )
    .unwrap_err();
    assert!(err.to_string().contains("scans for it"), "{err}");
}

#[test]
fn parses_oauth_token_secret() {
    let parsed = tools::parse_secret(
        &entry(
            r#"{ type = "oauth_token", grant = "refresh_token", name = "GOOGLE_TOKEN_JSON", token_endpoint = "https://oauth2.googleapis.com/token", hosts = ["gmail.googleapis.com"], fields = { refresh_token = { secret_ref = "GOOGLE_TOKEN_JSON", json_key = "refresh_token" }, client_id = { secret_ref = "GOOGLE_TOKEN_JSON", json_key = "client_id" } } }"#,
        ),
        &[],
    )
    .unwrap();
    let ParsedSecret::OAuthToken(oauth) = parsed else {
        panic!("expected oauth")
    };
    assert_eq!(oauth.grant, "refresh_token");
    assert_eq!(
        oauth.token_endpoint.as_deref(),
        Some("https://oauth2.googleapis.com/token")
    );
    assert_eq!(oauth.hosts, vec!["gmail.googleapis.com".to_owned()]);
    assert_eq!(oauth.fields.len(), 2);
    let refresh = oauth
        .fields
        .iter()
        .find(|(f, _)| f == "refresh_token")
        .unwrap();
    assert_eq!(refresh.1.secret_ref, "GOOGLE_TOKEN_JSON");
    assert_eq!(refresh.1.json_key.as_deref(), Some("refresh_token"));
}

#[test]
fn oauth_missing_required_field_errors() {
    let err = tools::parse_secret(
        &entry(r#"{ type = "oauth_token", grant = "refresh_token", name = "T", hosts = ["x.com"], fields = { client_id = "CID" } }"#),
        &[],
    )
    .unwrap_err();
    assert!(err.to_string().contains("requires field"), "{err}");
}

#[test]
fn brokered_token_parses_consumer_side_with_defaults() {
    let parsed = tools::parse_secret(
        &entry(r#"{type = "brokered_token", name = "openai-codex", hosts = ["chatgpt.com"]}"#),
        &[],
    )
    .unwrap();
    match parsed {
        ParsedSecret::BrokerToken(broker) => {
            assert_eq!(broker.name, "openai-codex");
            // credential defaults to name; inject defaults to Bearer Authorization.
            assert_eq!(broker.credential, "openai-codex");
            assert_eq!(broker.hosts, vec!["chatgpt.com".to_owned()]);
            assert_eq!(broker.inject_header, "Authorization");
            assert_eq!(broker.inject_formatter, "Bearer {{.Value}}");
        }
        other => panic!("expected broker token, got {other:?}"),
    }
}

#[test]
fn brokered_token_honors_credential_and_inject_overrides() {
    let parsed = tools::parse_secret(
        &entry(
            r#"{type = "brokered_token", name = "CODEX", credential = "openai-codex", hosts = ["chatgpt.com"], inject_header = "X-Token", inject_formatter = "{{.Value}}"}"#,
        ),
        &[],
    )
    .unwrap();
    let ParsedSecret::BrokerToken(broker) = parsed else {
        panic!("expected broker token");
    };
    assert_eq!(broker.credential, "openai-codex");
    assert_eq!(broker.inject_header, "X-Token");
    assert_eq!(broker.inject_formatter, "{{.Value}}");
}

const FALCONX_HMAC: &str = r#"{ type = "hmac_sign", name = "FALCONX_P1", hosts = ["api.falconx.io"], algorithm = "sha256", key_encoding = "base64", output_encoding = "base64", timestamp_format = "unix_seconds", message = "{{.Timestamp}}{{.Method}}{{.PathWithQuery}}{{.Body}}", credentials = { key = "FALCONX_P1_API_KEY", secret = "FALCONX_P1_SECRET_KEY", passphrase = "FALCONX_P1_PASSPHRASE" }, headers = [ { name = "FX-ACCESS-KEY", value = "{{.Credentials.key}}" }, { name = "FX-ACCESS-SIGN", value = "{{.Signature}}" }, { name = "FX-ACCESS-TIMESTAMP", value = "{{.Timestamp}}" }, { name = "FX-ACCESS-PASSPHRASE", value = "{{.Credentials.passphrase}}" } ] }"#;

#[test]
fn parses_hmac_sign_secret() {
    let parsed = tools::parse_secret(&entry(FALCONX_HMAC), &[]).unwrap();
    let ParsedSecret::Hmac(hmac) = parsed else {
        panic!("expected hmac")
    };
    assert_eq!(hmac.name, "FALCONX_P1");
    assert_eq!(hmac.hosts, vec!["api.falconx.io".to_owned()]);
    assert_eq!(hmac.algorithm, "sha256");
    assert_eq!(hmac.key_encoding, "base64");
    assert_eq!(hmac.output_encoding, "base64");
    assert_eq!(hmac.timestamp_format, "unix_seconds");
    assert_eq!(
        hmac.message,
        "{{.Timestamp}}{{.Method}}{{.PathWithQuery}}{{.Body}}"
    );
    assert!(!hmac.allow_chunked_body);
    // The signing key plus the two user-named credentials.
    let secret = hmac
        .credentials
        .iter()
        .find(|(f, _)| f == "secret")
        .unwrap();
    assert_eq!(secret.1.secret_ref, "FALCONX_P1_SECRET_KEY");
    assert_eq!(hmac.credentials.len(), 3);
    assert_eq!(hmac.headers.len(), 4);
    assert_eq!(hmac.headers[0].name, "FX-ACCESS-KEY");
    assert_eq!(hmac.headers[0].value, "{{.Credentials.key}}");
}

#[test]
fn hmac_requires_secret_credential() {
    let err = tools::parse_secret(
        &entry(r#"{ type = "hmac_sign", name = "X", hosts = ["x.com"], algorithm = "sha256", key_encoding = "hex", output_encoding = "hex", timestamp_format = "unix_seconds", message = "{{.Body}}", credentials = { key = "K" }, headers = [ { name = "Sig", value = "{{.Signature}}" } ] }"#),
        &[],
    )
    .unwrap_err();
    assert!(err.to_string().contains("must include \"secret\""), "{err}");
}

#[test]
fn hmac_rejects_unknown_algorithm() {
    let err = tools::parse_secret(
        &entry(r#"{ type = "hmac_sign", name = "X", hosts = ["x.com"], algorithm = "md5", key_encoding = "hex", output_encoding = "hex", timestamp_format = "unix_seconds", message = "{{.Body}}", credentials = { secret = "S" }, headers = [ { name = "Sig", value = "{{.Signature}}" } ] }"#),
        &[],
    )
    .unwrap_err();
    assert!(
        err.to_string().contains(r#""algorithm" must be one of"#),
        "{err}"
    );
}

#[test]
fn hmac_requires_hosts() {
    let err = tools::parse_secret(
        &entry(r#"{ type = "hmac_sign", name = "X", algorithm = "sha256", key_encoding = "hex", output_encoding = "hex", timestamp_format = "unix_seconds", message = "{{.Body}}", credentials = { secret = "S" }, headers = [ { name = "Sig", value = "{{.Signature}}" } ] }"#),
        &[],
    )
    .unwrap_err();
    assert!(
        err.to_string().contains("'hosts' must be a non-empty"),
        "{err}"
    );
}

const CLOUDWATCH_AWS: &str = r#"{ type = "aws_auth", name = "cloudwatch", access_key_id = "AWS_ACCESS_KEY_ID", secret_access_key = "AWS_SECRET_ACCESS_KEY", hosts = ["logs.*.amazonaws.com", "monitoring.*.amazonaws.com"], allowed_services = ["logs", "monitoring"] }"#;

#[test]
fn parses_aws_auth_secret() {
    let parsed = tools::parse_secret(&entry(CLOUDWATCH_AWS), &[]).unwrap();
    let ParsedSecret::AwsAuth(aws) = parsed else {
        panic!("expected aws_auth")
    };
    assert_eq!(aws.name, "cloudwatch");
    assert_eq!(
        aws.hosts,
        vec![
            "logs.*.amazonaws.com".to_owned(),
            "monitoring.*.amazonaws.com".to_owned()
        ]
    );
    assert_eq!(aws.access_key_id_ref, "AWS_ACCESS_KEY_ID");
    assert_eq!(aws.secret_access_key_ref, "AWS_SECRET_ACCESS_KEY");
    assert_eq!(aws.session_token_ref, None);
    assert_eq!(
        aws.allowed_services,
        vec!["logs".to_owned(), "monitoring".to_owned()]
    );
    assert!(aws.allowed_regions.is_empty());
}

#[test]
fn parses_aws_auth_with_session_token_and_regions() {
    let parsed = tools::parse_secret(
        &entry(
            r#"{ type = "aws_auth", name = "cw", access_key_id = "AKID", secret_access_key = "SAK", session_token = "STS_TOKEN", hosts = ["logs.us-east-1.amazonaws.com"], allowed_regions = ["us-east-1"] }"#,
        ),
        &[],
    )
    .unwrap();
    let ParsedSecret::AwsAuth(aws) = parsed else {
        panic!("expected aws_auth")
    };
    assert_eq!(aws.session_token_ref.as_deref(), Some("STS_TOKEN"));
    assert_eq!(aws.allowed_regions, vec!["us-east-1".to_owned()]);
}

#[test]
fn parses_gcp_id_token_secret() {
    let parsed = tools::parse_secret(
        &entry(
            r#"{ type = "gcp_id_token", name = "CLOUD_RUN_KEYFILE", audience = "https://my-service-abc123-uc.a.run.app", header = "X-Serverless-Authorization", hosts = ["my-service-abc123-uc.a.run.app"] }"#,
        ),
        &[],
    )
    .unwrap();
    let ParsedSecret::GcpIdToken(gcp) = parsed else {
        panic!("expected gcp_id_token")
    };
    assert_eq!(gcp.name, "CLOUD_RUN_KEYFILE");
    assert_eq!(gcp.secret_ref, "CLOUD_RUN_KEYFILE");
    assert_eq!(gcp.audience, "https://my-service-abc123-uc.a.run.app");
    assert_eq!(gcp.header.as_deref(), Some("x-serverless-authorization"));
    assert_eq!(gcp.hosts, vec!["my-service-abc123-uc.a.run.app"]);
}

#[test]
fn gcp_id_token_requires_audience() {
    let err = tools::parse_secret(
        &entry(
            r#"{ type = "gcp_id_token", name = "CLOUD_RUN_KEYFILE", hosts = ["my-service-abc123-uc.a.run.app"] }"#,
        ),
        &[],
    )
    .unwrap_err();
    assert!(err.to_string().contains("audience"), "{err}");
}

#[test]
fn aws_auth_requires_access_key_id() {
    let err = tools::parse_secret(
        &entry(
            r#"{ type = "aws_auth", name = "X", secret_access_key = "SAK", hosts = ["logs.amazonaws.com"] }"#,
        ),
        &[],
    )
    .unwrap_err();
    assert!(
        err.to_string()
            .contains("requires a non-empty 'access_key_id'"),
        "{err}"
    );
}

#[test]
fn aws_auth_requires_hosts() {
    let err = tools::parse_secret(
        &entry(
            r#"{ type = "aws_auth", name = "X", access_key_id = "AKID", secret_access_key = "SAK" }"#,
        ),
        &[],
    )
    .unwrap_err();
    assert!(
        err.to_string().contains("'hosts' must be a non-empty"),
        "{err}"
    );
}

#[test]
fn parses_pg_dsn_secret() {
    let parsed = tools::parse_secret(
        &entry(r#"{ type = "pg_dsn", name = "RESHIFT_DSN", database = "pmadmin", secret_ref = "RESHIFT_DSN", role = "centaur_slack_reader", settings = [{ name = "centaur.slack_channel_id", value_from = { principal_label = "slack_channel_id" } }, { name = "centaur.slack_user_id", value_from = { proxy_label = "centaur.slack_user_id" } }] }"#),
        &[],
    )
    .unwrap();
    let ParsedSecret::PgDsn(pg) = parsed else {
        panic!("expected pg_dsn")
    };
    assert_eq!(pg.name, "RESHIFT_DSN");
    assert_eq!(pg.database, "pmadmin");
    assert_eq!(pg.secret_ref, "RESHIFT_DSN");
    assert_eq!(pg.role.as_deref(), Some("centaur_slack_reader"));
    assert_eq!(pg.settings.len(), 2);
    assert_eq!(pg.settings[0].name, "centaur.slack_channel_id");
    assert_eq!(
        pg.settings[0]
            .value_from
            .as_ref()
            .and_then(|value_from| value_from.principal_label.as_deref()),
        Some("slack_channel_id")
    );
    assert_eq!(pg.settings[1].name, "centaur.slack_user_id");
    assert_eq!(
        pg.settings[1]
            .value_from
            .as_ref()
            .and_then(|value_from| value_from.proxy_label.as_deref()),
        Some("centaur.slack_user_id")
    );
}

#[test]
fn rejects_pg_dsn_value_from_with_multiple_selectors() {
    let err = tools::parse_secret(
        &entry(r#"{ type = "pg_dsn", name = "RESHIFT_DSN", database = "pmadmin", secret_ref = "RESHIFT_DSN", settings = [{ name = "centaur.slack_user_id", value_from = { principal_label = "slack_user_id", proxy_label = "centaur.slack_user_id" } }] }"#),
        &[],
    )
    .unwrap_err();

    assert!(
        err.to_string().contains("must declare exactly one"),
        "{err}"
    );
}

#[test]
fn pg_dsn_requires_database() {
    let err = tools::parse_secret(&entry(r#"{ type = "pg_dsn", name = "RESHIFT_DSN" }"#), &[])
        .unwrap_err();
    assert!(err.to_string().contains("database"), "{err}");
}

#[test]
fn unknown_type_errors() {
    let err = tools::parse_secret(&entry(r#"{type = "mystery", name = "X"}"#), &[]).unwrap_err();
    assert!(err.to_string().contains("unknown secret type"), "{err}");
}

#[test]
fn legacy_string_shim_is_replace_secret() {
    let parsed =
        tools::parse_secret(&entry(r#""FOO_TOKEN""#), &["api.example.com".to_owned()]).unwrap();
    let ParsedSecret::Http(http) = parsed else {
        panic!("expected http")
    };
    assert_eq!(http.name, "FOO_TOKEN");
    assert_eq!(http.mode, SecretMode::Replace);
    assert!(http.match_headers.contains(&"Authorization".to_owned()));
    assert_eq!(http.hosts, vec!["api.example.com".to_owned()]);
}

// ----- translation ---------------------------------------------------------

#[test]
fn translates_http_replace_to_static_input() {
    let secrets = vec![
        tools::parse_secret(
            &entry(r#"{type = "http", name = "SLACK_BOT_TOKEN", match_headers = ["Authorization"], hosts = ["slack.com"]}"#),
            &[],
        )
        .unwrap(),
    ];
    let out = translate::translate("default", "tool-slack", &secrets, &SourcePolicy::env());
    let SecretInput::Static(input) = &out.inputs[0] else {
        panic!("expected static")
    };
    assert_eq!(input.foreign_id, "tool-slack-slack-bot-token");
    assert_eq!(input.name, "SLACK_BOT_TOKEN");
    let replace = input.replace_config.as_ref().unwrap();
    assert_eq!(replace.proxy_value, "SLACK_BOT_TOKEN");
    assert_eq!(replace.match_headers, vec!["Authorization".to_owned()]);
    assert!(input.inject_config.is_none());
    assert_eq!(input.source.source_type, "env");
    assert_eq!(
        input.source.config,
        serde_json::json!({ "var": "SLACK_BOT_TOKEN" })
    );
    assert_eq!(input.rules.len(), 1);
    assert_eq!(input.rules[0].host.as_deref(), Some("slack.com"));
}

#[test]
fn translates_gcp_auth_defaults_scopes_when_unset() {
    let secrets = vec![
        tools::parse_secret(
            &entry(
                r#"{ type = "gcp_auth", name = "GCP_CRED", hosts = ["storage.googleapis.com"] }"#,
            ),
            &[],
        )
        .unwrap(),
    ];
    let out = translate::translate("default", "tool-gcs", &secrets, &SourcePolicy::env());
    let SecretInput::GcpAuth(input) = &out.inputs[0] else {
        panic!("expected gcp_auth")
    };
    // No scopes declared -> the single default cloud-platform scope.
    assert_eq!(
        input.scopes,
        vec!["https://www.googleapis.com/auth/cloud-platform".to_owned()]
    );
}

#[test]
fn translates_gcp_id_token_to_input() {
    let secrets = vec![
        tools::parse_secret(
            &entry(
                r#"{ type = "gcp_id_token", name = "CLOUD_RUN_KEYFILE", audience = "https://my-service-abc123-uc.a.run.app", header = "x-serverless-authorization", hosts = ["my-service-abc123-uc.a.run.app"] }"#,
            ),
            &[],
        )
        .unwrap(),
    ];
    let out = translate::translate("default", "tool-cloudrun", &secrets, &SourcePolicy::env());
    let SecretInput::GcpIdToken(input) = &out.inputs[0] else {
        panic!("expected gcp_id_token")
    };
    assert_eq!(
        input.foreign_id,
        "tool-cloudrun-gcp-id-token-cloud-run-keyfile-https-my-service-abc123-uc-a-run-app-x-serverless-authorization"
    );
    assert_eq!(input.name.as_deref(), Some("GCP ID Token (tool-cloudrun)"));
    assert_eq!(
        input.audience,
        "https://my-service-abc123-uc.a.run.app".to_owned()
    );
    assert_eq!(input.header.as_deref(), Some("x-serverless-authorization"));
    assert_eq!(input.keyfile.source_type, "env");
    assert_eq!(
        input.keyfile.config,
        serde_json::json!({ "var": "CLOUD_RUN_KEYFILE" })
    );
    assert_eq!(input.rules.len(), 1);
    assert_eq!(
        input.rules[0].host.as_deref(),
        Some("my-service-abc123-uc.a.run.app")
    );
}

#[test]
fn translates_oauth_with_json_key_fields() {
    let secrets = vec![
        tools::parse_secret(
            &entry(
                r#"{ type = "oauth_token", grant = "refresh_token", name = "GOOGLE_TOKEN_JSON", token_endpoint = "https://oauth2.googleapis.com/token", hosts = ["gmail.googleapis.com"], fields = { refresh_token = { secret_ref = "GOOGLE_TOKEN_JSON", json_key = "refresh_token" }, client_id = { secret_ref = "GOOGLE_TOKEN_JSON", json_key = "client_id" } } }"#,
            ),
            &[],
        )
        .unwrap(),
    ];
    let out = translate::translate("default", "tool-gsuite", &secrets, &SourcePolicy::env());
    let SecretInput::OAuthToken(input) = &out.inputs[0] else {
        panic!("expected oauth")
    };
    assert_eq!(
        input.foreign_id,
        "tool-gsuite-oauth-https-oauth2-googleapis-com-token"
    );
    assert_eq!(input.grant, "refresh_token");
    let refresh = input.credentials.get("refresh_token").unwrap();
    assert_eq!(refresh.source_type, "env");
    assert_eq!(
        refresh.config,
        serde_json::json!({ "var": "GOOGLE_TOKEN_JSON", "json_key": "refresh_token" })
    );
}

#[test]
fn translates_pg_dsn_to_input_with_roundtrip_foreign_id() {
    let secrets = vec![
        tools::parse_secret(
            &entry(r#"{ type = "pg_dsn", name = "RESHIFT_DSN", database = "pmadmin", secret_ref = "RESHIFT_DSN", role = "centaur_slack_reader", settings = [{ name = "centaur.slack_channel_id", value_from = { principal_label = "slack_channel_id" } }, { name = "centaur.slack_user_id", value_from = { proxy_label = "centaur.slack_user_id" } }] }"#),
            &[],
        )
        .unwrap(),
    ];
    let out = translate::translate("default", "tool-reshift", &secrets, &SourcePolicy::env());
    let SecretInput::PgDsn(input) = &out.inputs[0] else {
        panic!("expected pg_dsn")
    };
    // The foreign_id is not role-prefixed: it must round-trip back to the
    // sandbox DSN env var (`RESHIFT_DSN`) that api-rs derives from it.
    assert_eq!(input.foreign_id, "reshift");
    assert_eq!(pg_sandbox_env_var(&input.foreign_id), "RESHIFT_DSN");
    assert_eq!(input.name, "RESHIFT_DSN");
    assert_eq!(input.database, "pmadmin");
    assert_eq!(input.role.as_deref(), Some("centaur_slack_reader"));
    assert_eq!(input.settings.len(), 2);
    assert_eq!(
        input.settings[0]
            .value_from
            .as_ref()
            .and_then(|value_from| value_from.principal_label.as_deref()),
        Some("slack_channel_id")
    );
    assert_eq!(
        input.settings[1]
            .value_from
            .as_ref()
            .and_then(|value_from| value_from.proxy_label.as_deref()),
        Some("centaur.slack_user_id")
    );
    assert_eq!(input.dsn.source_type, "env");
    assert_eq!(
        input.dsn.config,
        serde_json::json!({ "var": "RESHIFT_DSN" })
    );
}

#[test]
fn translates_hmac_to_input() {
    let secrets = vec![tools::parse_secret(&entry(FALCONX_HMAC), &[]).unwrap()];
    let out = translate::translate("default", "tool-falconx", &secrets, &SourcePolicy::env());
    let SecretInput::Hmac(input) = &out.inputs[0] else {
        panic!("expected hmac")
    };
    assert_eq!(input.foreign_id, "tool-falconx-hmac-falconx-p1");
    assert_eq!(input.name, "FALCONX_P1");
    assert_eq!(input.signature_algorithm, "sha256");
    assert_eq!(input.signature_key_encoding, "base64");
    assert_eq!(input.signature_output_encoding, "base64");
    assert_eq!(input.timestamp_format, "unix_seconds");
    assert_eq!(
        input.signature_message,
        "{{.Timestamp}}{{.Method}}{{.PathWithQuery}}{{.Body}}"
    );
    assert!(!input.allow_chunked_body);
    assert_eq!(input.headers.len(), 4);
    assert_eq!(input.headers[1].name, "FX-ACCESS-SIGN");
    assert_eq!(input.headers[1].value, "{{.Signature}}");
    // The HMAC key resolves via the deployment source policy (env here).
    let secret = input.credentials.get("secret").unwrap();
    assert_eq!(secret.source_type, "env");
    assert_eq!(
        secret.config,
        serde_json::json!({ "var": "FALCONX_P1_SECRET_KEY" })
    );
    assert_eq!(input.rules.len(), 1);
    assert_eq!(input.rules[0].host.as_deref(), Some("api.falconx.io"));
}

#[test]
fn translates_aws_auth_to_input() {
    let secrets = vec![tools::parse_secret(&entry(CLOUDWATCH_AWS), &[]).unwrap()];
    let out = translate::translate("default", "tool-cloudwatch", &secrets, &SourcePolicy::env());
    let SecretInput::AwsAuth(input) = &out.inputs[0] else {
        panic!("expected aws_auth")
    };
    assert_eq!(input.foreign_id, "tool-cloudwatch-aws-cloudwatch");
    assert_eq!(input.name.as_deref(), Some("AWS Auth (tool-cloudwatch)"));
    // Credential refs resolve via the deployment source policy (env here).
    assert_eq!(input.access_key_id.source_type, "env");
    assert_eq!(
        input.access_key_id.config,
        serde_json::json!({ "var": "AWS_ACCESS_KEY_ID" })
    );
    assert_eq!(
        input.secret_access_key.config,
        serde_json::json!({ "var": "AWS_SECRET_ACCESS_KEY" })
    );
    assert!(input.session_token.is_none());
    assert_eq!(
        input.allowed_services,
        vec!["logs".to_owned(), "monitoring".to_owned()]
    );
    assert!(input.allowed_regions.is_empty());
    let hosts: Vec<_> = input
        .rules
        .iter()
        .filter_map(|r| r.host.as_deref())
        .collect();
    assert_eq!(
        hosts,
        vec!["logs.*.amazonaws.com", "monitoring.*.amazonaws.com"]
    );
}

#[test]
fn translates_aws_auth_session_token_through_policy() {
    let secrets = vec![
        tools::parse_secret(
            &entry(
                r#"{ type = "aws_auth", name = "cw", access_key_id = "AKID", secret_access_key = "SAK", session_token = "STS_TOKEN", hosts = ["logs.us-east-1.amazonaws.com"] }"#,
            ),
            &[],
        )
        .unwrap(),
    ];
    let out = translate::translate("default", "tool-cw", &secrets, &SourcePolicy::env());
    let SecretInput::AwsAuth(input) = &out.inputs[0] else {
        panic!("expected aws_auth")
    };
    let session = input.session_token.as_ref().unwrap();
    assert_eq!(session.source_type, "env");
    assert_eq!(session.config, serde_json::json!({ "var": "STS_TOKEN" }));
}

#[test]
fn translates_brokered_token_to_token_broker_static_secret() {
    let secrets = vec![
        tools::parse_secret(
            &entry(r#"{type = "brokered_token", name = "openai-codex", hosts = ["chatgpt.com"]}"#),
            &[],
        )
        .unwrap(),
    ];
    let out = translate::translate("default", "tool-codex", &secrets, &SourcePolicy::env());
    let SecretInput::Static(input) = &out.inputs[0] else {
        panic!("expected static")
    };
    assert_eq!(input.foreign_id, "tool-codex-openai-codex");
    assert_eq!(input.name, "openai-codex");
    // Sourced from the broker credential (created out of band), not env/1password.
    assert_eq!(input.source.source_type, "token_broker");
    assert_eq!(
        input.source.config,
        serde_json::json!({ "credential_id": "openai-codex", "credential_namespace": "default" })
    );
    let inject = input.inject_config.as_ref().unwrap();
    assert_eq!(inject.header.as_deref(), Some("Authorization"));
    assert_eq!(inject.formatter.as_deref(), Some("Bearer {{.Value}}"));
    assert!(input.replace_config.is_none());
    assert_eq!(input.rules[0].host.as_deref(), Some("chatgpt.com"));
}

#[test]
fn duplicate_secret_names_get_unique_foreign_ids() {
    let secrets = vec![
        tools::parse_secret(
            &entry(
                r#"{type="http", name="TOK", match_headers=["Authorization"], hosts=["a.com"]}"#,
            ),
            &[],
        )
        .unwrap(),
        tools::parse_secret(
            &entry(
                r#"{type="http", name="tok", match_headers=["Authorization"], hosts=["b.com"]}"#,
            ),
            &[],
        )
        .unwrap(),
    ];
    let out = translate::translate("default", "tool-x", &secrets, &SourcePolicy::env());
    let SecretInput::Static(a) = &out.inputs[0] else {
        panic!()
    };
    let SecretInput::Static(b) = &out.inputs[1] else {
        panic!()
    };
    assert_eq!(a.foreign_id, "tool-x-tok");
    assert_eq!(b.foreign_id, "tool-x-tok-2");
}

#[test]
fn translate_for_tool_adds_tool_identity_labels() {
    let secrets = vec![tools::parse_secret(
        &entry(
            r#"{ type = "http", name = "SLACK_BOT_TOKEN", match_headers = ["Authorization"], hosts = ["slack.com"] }"#,
        ),
        &[],
    )
    .unwrap()];
    let labels = translate::ToolLabels {
        tool: "slack".to_owned(),
        overlay: "centaur-paradigm".to_owned(),
    };
    let out = translate::translate_for_tool(
        "default",
        "tool-slack",
        &labels,
        &secrets,
        &SourcePolicy::env(),
    );
    let SecretInput::Static(secret) = &out.inputs[0] else {
        panic!()
    };
    assert_eq!(
        secret.labels.get("managed-by").map(String::as_str),
        Some("centaur")
    );
    assert_eq!(
        secret.labels.get("centaur-tool").map(String::as_str),
        Some("slack")
    );
    assert_eq!(
        secret
            .labels
            .get("centaur-tool-overlay")
            .map(String::as_str),
        Some("centaur-paradigm")
    );
}

// ----- overlay resolution ---------------------------------------------------

fn tmp_root(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("centaur-perms-test-{}-{tag}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_tool(base: &Path, rel: &str, body: &str) {
    let dir = base.join(rel);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("pyproject.toml"), body).unwrap();
}

const SLACK_A: &str = r#"
[tool.centaur]
secrets = [ {type = "http", name = "SLACK_BOT_TOKEN", match_headers = ["Authorization"], hosts = ["slack.com"]} ]
"#;

const SLACK_B: &str = r#"
[tool.centaur]
secrets = [ {type = "http", name = "SLACK_OVERLAY_TOKEN", match_headers = ["Authorization"], hosts = ["slack.com"]} ]
"#;

#[test]
fn later_dir_shadows_earlier() {
    let root = tmp_root("shadow");
    let base = root.join("base");
    let overlay = root.join("overlay");
    write_tool(&base, "slack", SLACK_A);
    write_tool(&overlay, "slack", SLACK_B);

    let dirs = vec![base.clone(), overlay.clone()];
    let manifest = tools::find_tool(&dirs, "slack").unwrap();
    assert_eq!(manifest.dir, overlay.join("slack"));
    assert_eq!(manifest.secrets[0].name(), "SLACK_OVERLAY_TOKEN");

    fs::remove_dir_all(&root).unwrap();
}

#[test]
fn finds_tool_in_category_subdir() {
    let root = tmp_root("category");
    let base = root.join("tools");
    write_tool(&base, "productivity/slack", SLACK_A);

    let manifest = tools::find_tool(&[base], "slack").unwrap();
    assert_eq!(manifest.name, "slack");
    assert_eq!(manifest.secrets[0].name(), "SLACK_BOT_TOKEN");

    fs::remove_dir_all(&root).unwrap();
}

#[test]
fn overlay_name_uses_parent_when_root_is_tools_dir() {
    let root = tmp_root("overlay-name");
    let tools_dir = root.join("centaur-paradigm").join("tools");
    write_tool(&tools_dir, "productivity/slack", SLACK_A);

    let manifest = tools::find_tool(std::slice::from_ref(&tools_dir), "slack").unwrap();
    assert_eq!(
        tools::overlay_name_for_tool_dir(&manifest.dir, &[tools_dir]),
        "centaur-paradigm"
    );

    fs::remove_dir_all(&root).unwrap();
}

#[test]
fn missing_tool_errors() {
    let root = tmp_root("missing");
    let base = root.join("tools");
    write_tool(&base, "slack", SLACK_A);
    let err = tools::find_tool(&[base], "nope").unwrap_err();
    assert!(err.to_string().contains("not found"), "{err}");
    fs::remove_dir_all(&root).unwrap();
}

// ----- label parsing --------------------------------------------------------

#[test]
fn parses_label_filters() {
    assert_eq!(
        crate::parse_label("managed-by=centaur").unwrap(),
        ("managed-by".to_owned(), "centaur".to_owned())
    );
    // empty value is allowed (matches an explicitly-empty label)
    assert_eq!(
        crate::parse_label("k=").unwrap(),
        ("k".to_owned(), String::new())
    );
    assert!(crate::parse_label("noequals").is_err());
    assert!(crate::parse_label("=v").is_err());
}

// ----- secret selection -----------------------------------------------------

fn http(name: &str) -> ParsedSecret {
    tools::parse_secret(
        &entry(&format!(r#"{{type = "http", name = "{name}", match_headers = ["Authorization"], hosts = ["x.com"]}}"#)),
        &[],
    )
    .unwrap()
}

#[test]
fn select_secrets_empty_names_keeps_all() {
    let all = vec![http("A"), http("B")];
    let out = crate::select_secrets(all.clone(), &[]).unwrap();
    assert_eq!(out.len(), 2);
}

#[test]
fn select_secrets_filters_by_name() {
    let all = vec![http("A"), http("B"), http("C")];
    let out = crate::select_secrets(all, &["B".to_owned()]).unwrap();
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].name(), "B");
}

#[test]
fn select_secrets_unknown_name_errors() {
    let all = vec![http("A")];
    let err = crate::select_secrets(all, &["NOPE".to_owned()]).unwrap_err();
    assert!(err.to_string().contains("no secret named"), "{err}");
}

// ----- fidelity against the real in-repo tools ------------------------------

/// The repo `tools/` directory, relative to this crate. `None` when the crate
/// is built outside the monorepo checkout (the fidelity tests then no-op).
fn repo_tools_dir() -> Option<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../../tools");
    dir.is_dir().then_some(dir)
}

#[test]
fn real_slack_tool_parses_and_translates() {
    let Some(tools_dir) = repo_tools_dir() else {
        return;
    };
    let manifest = tools::find_tool(&[tools_dir], "slack").unwrap();
    assert_eq!(manifest.name, "slack");
    let out = translate::translate(
        "default",
        "tool-slack",
        &manifest.all_secrets().cloned().collect::<Vec<_>>(),
        &SourcePolicy::env(),
    );
    assert!(
        out.inputs.iter().any(
            |i| matches!(i, SecretInput::Static(s) if s.foreign_id == "tool-slack-slack-bot-token")
        ),
        "expected the SLACK_BOT_TOKEN static secret"
    );
}

#[test]
fn real_gsuite_tool_parses_oauth() {
    let Some(tools_dir) = repo_tools_dir() else {
        return;
    };
    let manifest = tools::find_tool(&[tools_dir], "gsuite").unwrap();
    let out = translate::translate(
        "default",
        "tool-gsuite",
        &manifest.all_secrets().cloned().collect::<Vec<_>>(),
        &SourcePolicy::env(),
    );
    assert!(
        out.inputs
            .iter()
            .any(|i| matches!(i, SecretInput::OAuthToken(_))),
        "expected gsuite's oauth_token secret"
    );
}
