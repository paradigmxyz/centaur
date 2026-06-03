//! Unit tests for pyproject parsing, translation, and overlay resolution.

use std::fs;
use std::path::{Path, PathBuf};

use centaur_iron_control::SecretInput;
use centaur_iron_proxy::SourcePolicy;

use crate::tools::{self, ParsedSecret, SecretMode};
use crate::translate;

fn entry(toml_src: &str) -> toml::Value {
    let v: toml::Value = toml::from_str(&format!("x = {toml_src}")).expect("valid toml");
    v.get("x").expect("x key").clone()
}

// ----- parsing -------------------------------------------------------------

#[test]
fn parses_http_replace_secret() {
    let parsed = tools::parse_secret(
        &entry(r#"{type = "http", name = "SLACK_BOT_TOKEN", match_headers = ["Authorization"], hosts = ["slack.com"]}"#),
        &[],
    )
    .unwrap();
    let ParsedSecret::Http(http) = parsed else { panic!("expected http") };
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
        &["api.parallel.ai".to_owned(), "search.parallel.ai".to_owned()],
    )
    .unwrap();
    let ParsedSecret::Http(http) = parsed else { panic!("expected http") };
    assert_eq!(http.hosts, vec!["api.parallel.ai".to_owned(), "search.parallel.ai".to_owned()]);
}

#[test]
fn parses_inject_secret() {
    let parsed = tools::parse_secret(
        &entry(r#"{type = "http", name = "TOK", mode = "inject", inject_header = "Authorization", inject_formatter = "Bearer {{.Value}}", hosts = ["api.example.com"]}"#),
        &[],
    )
    .unwrap();
    let ParsedSecret::Http(http) = parsed else { panic!("expected http") };
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
    let ParsedSecret::OAuthToken(oauth) = parsed else { panic!("expected oauth") };
    assert_eq!(oauth.grant, "refresh_token");
    assert_eq!(oauth.token_endpoint.as_deref(), Some("https://oauth2.googleapis.com/token"));
    assert_eq!(oauth.hosts, vec!["gmail.googleapis.com".to_owned()]);
    assert_eq!(oauth.fields.len(), 2);
    let refresh = oauth.fields.iter().find(|(f, _)| f == "refresh_token").unwrap();
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
fn unsupported_types_are_marked_not_errored() {
    for kind in ["pg_dsn", "hmac_sign", "brokered_token"] {
        let src = format!(r#"{{type = "{kind}", name = "X", hosts = ["x.com"], database = "db"}}"#);
        let parsed = tools::parse_secret(&entry(&src), &[]).unwrap();
        match parsed {
            ParsedSecret::Unsupported { name, kind: k } => {
                assert_eq!(name, "X");
                assert_eq!(k, kind);
            }
            other => panic!("expected unsupported, got {other:?}"),
        }
    }
}

#[test]
fn unknown_type_errors() {
    let err = tools::parse_secret(&entry(r#"{type = "mystery", name = "X"}"#), &[]).unwrap_err();
    assert!(err.to_string().contains("unknown secret type"), "{err}");
}

#[test]
fn legacy_string_shim_is_replace_secret() {
    let parsed = tools::parse_secret(&entry(r#""FOO_TOKEN""#), &["api.example.com".to_owned()]).unwrap();
    let ParsedSecret::Http(http) = parsed else { panic!("expected http") };
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
    assert!(out.skipped.is_empty());
    let SecretInput::Static(input) = &out.inputs[0] else { panic!("expected static") };
    assert_eq!(input.foreign_id, "tool-slack-slack-bot-token");
    assert_eq!(input.name, "SLACK_BOT_TOKEN");
    let replace = input.replace_config.as_ref().unwrap();
    assert_eq!(replace.proxy_value, "SLACK_BOT_TOKEN");
    assert_eq!(replace.match_headers, vec!["Authorization".to_owned()]);
    assert!(input.inject_config.is_none());
    assert_eq!(input.source.source_type, "env");
    assert_eq!(input.source.config, serde_json::json!({ "var": "SLACK_BOT_TOKEN" }));
    assert_eq!(input.rules.len(), 1);
    assert_eq!(input.rules[0].host.as_deref(), Some("slack.com"));
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
    let SecretInput::OAuthToken(input) = &out.inputs[0] else { panic!("expected oauth") };
    assert_eq!(input.foreign_id, "tool-gsuite-oauth-https-oauth2-googleapis-com-token");
    assert_eq!(input.grant, "refresh_token");
    let refresh = input.credentials.get("refresh_token").unwrap();
    assert_eq!(refresh.source_type, "env");
    assert_eq!(refresh.config, serde_json::json!({ "var": "GOOGLE_TOKEN_JSON", "json_key": "refresh_token" }));
}

#[test]
fn unsupported_secret_is_reported_as_skipped() {
    let secrets = vec![ParsedSecret::Unsupported { name: "SIG".to_owned(), kind: "hmac_sign".to_owned() }];
    let out = translate::translate("default", "tool-x", &secrets, &SourcePolicy::env());
    assert!(out.inputs.is_empty());
    assert_eq!(out.skipped, vec![("SIG".to_owned(), "hmac_sign".to_owned())]);
}

#[test]
fn duplicate_secret_names_get_unique_foreign_ids() {
    let secrets = vec![
        tools::parse_secret(&entry(r#"{type="http", name="TOK", match_headers=["Authorization"], hosts=["a.com"]}"#), &[]).unwrap(),
        tools::parse_secret(&entry(r#"{type="http", name="tok", match_headers=["Authorization"], hosts=["b.com"]}"#), &[]).unwrap(),
    ];
    let out = translate::translate("default", "tool-x", &secrets, &SourcePolicy::env());
    let SecretInput::Static(a) = &out.inputs[0] else { panic!() };
    let SecretInput::Static(b) = &out.inputs[1] else { panic!() };
    assert_eq!(a.foreign_id, "tool-x-tok");
    assert_eq!(b.foreign_id, "tool-x-tok-2");
}

// ----- overlay resolution ---------------------------------------------------

fn tmp_root(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("centaur-grants-test-{}-{tag}", std::process::id()));
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
fn missing_tool_errors() {
    let root = tmp_root("missing");
    let base = root.join("tools");
    write_tool(&base, "slack", SLACK_A);
    let err = tools::find_tool(&[base], "nope").unwrap_err();
    assert!(err.to_string().contains("not found"), "{err}");
    fs::remove_dir_all(&root).unwrap();
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
    let Some(tools_dir) = repo_tools_dir() else { return };
    let manifest = tools::find_tool(&[tools_dir], "slack").unwrap();
    assert_eq!(manifest.name, "slack");
    let out = translate::translate("default", "tool-slack", &manifest.all_secrets().cloned().collect::<Vec<_>>(), &SourcePolicy::env());
    assert!(out.skipped.is_empty(), "slack should have no unsupported secrets");
    assert!(
        out.inputs.iter().any(|i| matches!(i, SecretInput::Static(s) if s.foreign_id == "tool-slack-slack-bot-token")),
        "expected the SLACK_BOT_TOKEN static secret"
    );
}

#[test]
fn real_gsuite_tool_parses_oauth() {
    let Some(tools_dir) = repo_tools_dir() else { return };
    let manifest = tools::find_tool(&[tools_dir], "gsuite").unwrap();
    let out = translate::translate("default", "tool-gsuite", &manifest.all_secrets().cloned().collect::<Vec<_>>(), &SourcePolicy::env());
    assert!(
        out.inputs.iter().any(|i| matches!(i, SecretInput::OAuthToken(_))),
        "expected gsuite's oauth_token secret"
    );
}
