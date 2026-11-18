//! The console's read-only view of a secret of any kind: where its value
//! comes from, what it injects, and how it is configured. Never a secret
//! value: an inline source shows as the reference `"inline"`.

use serde_json::{Value, json};
use sqlx::PgConnection;

use super::{
    draft::VALUE_FROM_KEYS,
    kind::Kind,
    parts::{CREDENTIAL_FIELD, StoredSource, is_blank},
    store::Loaded,
};
use crate::ApiError;

/// Kinds the console creates and edits itself.
pub fn editable(kind: Kind) -> bool {
    matches!(
        kind,
        Kind::Static | Kind::PgDsn | Kind::GcpAuth | Kind::GcpIdToken
    )
}

fn source_type_label(source_type: &str) -> String {
    match source_type {
        "env" => "Env",
        "aws_sm" => "AWS-SM",
        "aws_ssm" => "AWS-SSM",
        "1password" => "1Password",
        "1password_connect" => "1Password-Connect",
        "control_plane" => "Inline",
        "token_broker" => "Token-Broker",
        "workload_identity" => "Workload-Identity",
        other => other,
    }
    .to_owned()
}

/// The config key that names the external reference of a source type.
fn reference_key(source_type: &str) -> Option<&'static str> {
    Some(match source_type {
        "env" => "var",
        "aws_sm" => "secret_id",
        "aws_ssm" => "name",
        "1password" | "1password_connect" => "secret_ref",
        "token_broker" => "credential_id",
        _ => return None,
    })
}

struct Segment {
    role: Option<String>,
    source_type: String,
    reference: Value,
}

fn segment(source: &StoredSource, with_role: bool) -> Segment {
    let reference = match reference_key(&source.source_type) {
        Some(key) => source.config.get(key).cloned().unwrap_or(Value::Null),
        None if source.source_type == "control_plane" => "inline".into(),
        None => Value::Null,
    };
    Segment {
        role: with_role.then(|| source.role().unwrap_or_default().to_owned()),
        source_type: source.source_type.clone(),
        reference: if is_blank(&reference) {
            Value::Null
        } else {
            reference
        },
    }
}

fn segments(loaded: &Loaded) -> Vec<Segment> {
    let by_role = |credentials_only: bool| {
        let mut sources: Vec<&StoredSource> = loaded
            .sources
            .iter()
            .filter(|source| !credentials_only || source.role_kind() == Some(CREDENTIAL_FIELD))
            .collect();
        sources.sort_by(|a, b| a.role().cmp(&b.role()));
        sources
            .into_iter()
            .map(|source| segment(source, true))
            .collect()
    };
    match loaded.kind {
        Kind::OauthToken => by_role(true),
        Kind::AwsAuth | Kind::Hmac => by_role(false),
        Kind::GcpAuth if loaded.source().is_none() => {
            // Workload identity has no source, only a provider.
            match loaded
                .row
                .column("credentials_provider")
                .get("type")
                .and_then(Value::as_str)
            {
                Some(provider) => vec![Segment {
                    role: None,
                    source_type: provider.to_owned(),
                    reference: Value::Null,
                }],
                None => Vec::new(),
            }
        }
        _ => loaded
            .source()
            .map(|source| segment(source, false))
            .into_iter()
            .collect(),
    }
}

pub fn source_segments(loaded: &Loaded) -> Value {
    segments(loaded)
        .into_iter()
        .map(|segment| {
            json!({
                "role": segment.role,
                "type": segment.source_type,
                "ref": segment.reference,
                "type_label": source_type_label(&segment.source_type),
            })
        })
        .collect()
}

/// The distinct source type labels, for the list.
pub fn source_types(loaded: &Loaded) -> Vec<String> {
    let mut labels: Vec<String> = Vec::new();
    for segment in segments(loaded) {
        let label = source_type_label(&segment.source_type);
        if !labels.contains(&label) {
            labels.push(label);
        }
    }
    labels
}

/// The broker credential a static secret wraps, with its OAuth app.
pub async fn managed_by(conn: &mut PgConnection, loaded: &Loaded) -> Result<Value, ApiError> {
    let credential_id = loaded.row.column("broker_credential_id").as_i64();
    let (Kind::Static, Some(credential_id)) = (loaded.kind, credential_id) else {
        return Ok(Value::Null);
    };
    let found: Option<(i64, Option<i64>, Option<String>)> = sqlx::query_as(
        "SELECT bc.id, oa.id, oa.slug FROM broker_credentials bc \
         LEFT JOIN oauth_apps oa ON oa.id = bc.oauth_app_id WHERE bc.id = $1",
    )
    .bind(credential_id)
    .fetch_optional(conn)
    .await?;
    Ok(found.map_or(Value::Null, |(credential, app, slug)| {
        json!({
            "broker_credential_id": centaur_console_db::oid("bcr", credential),
            "oauth_app_id": app.map(|app| centaur_console_db::oid("oap", app)),
            "oauth_app_slug": slug,
        })
    }))
}

fn text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn present_text(loaded: &Loaded, column: &str) -> Option<String> {
    let value = loaded.row.column(column);
    (!is_blank(value)).then(|| text(value))
}

fn lines(value: &Value) -> String {
    match value {
        Value::Array(items) => items.iter().map(text).collect::<Vec<_>>().join("\n"),
        other => text(other),
    }
}

/// The header or query parameter the secret is injected into.
pub fn injection(loaded: &Loaded) -> Option<String> {
    let row = &loaded.row;
    match loaded.kind {
        Kind::Static => {
            let inject = row.column("inject_config");
            let replace = row.column("replace_config");
            if !is_blank(inject) {
                if let Some(header) = inject.get("header").filter(|v| !is_blank(v)) {
                    return Some(text(header));
                }
                inject
                    .get("query_param")
                    .filter(|v| !is_blank(v))
                    .map(|param| format!("?{}", text(param)))
            } else {
                let headers: Vec<String> = replace
                    .get("match_headers")
                    .and_then(Value::as_array)
                    .map(|headers| headers.iter().map(text).collect())
                    .unwrap_or_default();
                (!headers.is_empty()).then(|| headers.join(", "))
            }
        }
        Kind::GcpAuth => Some("Authorization: Bearer".into()),
        Kind::GcpIdToken => Some(format!(
            "{}: Bearer",
            present_text(loaded, "header").unwrap_or_else(|| "authorization".into())
        )),
        Kind::AwsAuth => Some("Authorization: AWS4-HMAC-SHA256".into()),
        Kind::OauthToken => {
            let header = present_text(loaded, "header").unwrap_or_else(|| "Authorization".into());
            Some(match present_text(loaded, "value_prefix") {
                Some(prefix) => format!("{header}: {} …", prefix.trim()),
                None => header,
            })
        }
        Kind::Hmac => {
            let names: Vec<String> = signed_header_pairs(loaded)
                .into_iter()
                .filter(|(name, _)| !is_blank(name))
                .map(|(name, _)| text(&name))
                .collect();
            (!names.is_empty()).then(|| names.join(", "))
        }
        Kind::PgDsn | Kind::SshKey => None,
    }
}

/// A pg_dsn setting as `(name, kind, value)`.
fn setting_row(setting: &Value) -> (String, String, String) {
    let name = setting.get("name").map(text).unwrap_or_default();
    if let Some(reference) = setting.get("value_from").and_then(Value::as_object)
        && let Some(key) = VALUE_FROM_KEYS
            .iter()
            .find(|key| reference.contains_key(**key))
    {
        return (name, (*key).to_owned(), text(&reference[*key]));
    }
    (
        name,
        "literal".into(),
        setting.get("value").map(text).unwrap_or_default(),
    )
}

fn kind_configuration(loaded: &Loaded) -> Vec<(&'static str, String)> {
    let row = &loaded.row;
    let present = |column: &str| !is_blank(row.column(column));
    let mut rows: Vec<(&'static str, String)> = Vec::new();
    let mut add = |label: &'static str, value: String| rows.push((label, value));
    match loaded.kind {
        Kind::Static => {
            add("Credential profile", text(row.column("kind")));
            add(
                "Mode",
                if present("replace_config") {
                    "replace"
                } else {
                    "inject"
                }
                .into(),
            );
        }
        Kind::GcpAuth => {
            if present("subject") {
                add("Subject", text(row.column("subject")));
            }
            if present("scopes") {
                add("Scopes", lines(row.column("scopes")));
            }
        }
        Kind::GcpIdToken => {
            add("Audience", text(row.column("audience")));
            add(
                "Header",
                present_text(loaded, "header").unwrap_or_else(|| "authorization".into()),
            );
        }
        Kind::AwsAuth => {
            if present("allowed_regions") {
                add("Allowed regions", lines(row.column("allowed_regions")));
            }
            if present("allowed_services") {
                add("Allowed services", lines(row.column("allowed_services")));
            }
        }
        Kind::OauthToken => {
            add("Grant", text(row.column("grant")));
            add("Token endpoint", text(row.column("token_endpoint")));
            if present("audience") {
                add("Audience", text(row.column("audience")));
            }
            if present("scopes") {
                add("Scopes", lines(row.column("scopes")));
            }
        }
        Kind::PgDsn => {
            if present("database") {
                add("Database", text(row.column("database")));
            }
            if present("role") {
                add("Role", text(row.column("role")));
            }
            let settings = row
                .column("settings")
                .as_array()
                .cloned()
                .unwrap_or_default();
            if !settings.is_empty() {
                let lines: Vec<String> = settings
                    .iter()
                    .map(|setting| {
                        let (name, kind, value) = setting_row(setting);
                        if kind == "literal" {
                            format!("{name} = {value}")
                        } else {
                            format!("{name} = {kind}({value})")
                        }
                    })
                    .collect();
                add("Session settings", lines.join("\n"));
            }
        }
        Kind::Hmac => {
            for (label, column) in [
                ("Timestamp format", "timestamp_format"),
                ("Algorithm", "signature_algorithm"),
                ("Key encoding", "signature_key_encoding"),
                ("Output encoding", "signature_output_encoding"),
                ("Message", "signature_message"),
            ] {
                add(label, text(row.column(column)));
            }
        }
        Kind::SshKey => {}
    }
    rows
}

pub fn configuration(loaded: &Loaded) -> Value {
    let mut rows = Vec::new();
    if let Some(injection) = injection(loaded).filter(|value| !value.trim().is_empty()) {
        rows.push(("Injects into", injection));
    }
    rows.extend(kind_configuration(loaded));
    rows.into_iter()
        .map(|(label, value)| json!({"label": label, "value": value}))
        .collect()
}

fn signed_header_pairs(loaded: &Loaded) -> Vec<(Value, Value)> {
    loaded
        .row
        .column("headers")
        .as_array()
        .map(|headers| {
            headers
                .iter()
                .filter(|header| header.is_object())
                .map(|header| {
                    (
                        header.get("name").cloned().unwrap_or(Value::Null),
                        header.get("value").cloned().unwrap_or(Value::Null),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// HMAC secrets: the headers the proxy signs with; `null` for other kinds.
pub fn signed_headers(loaded: &Loaded) -> Value {
    if loaded.kind != Kind::Hmac {
        return Value::Null;
    }
    signed_header_pairs(loaded)
        .into_iter()
        .map(|(name, value)| json!({"name": name, "value": value}))
        .collect()
}

/// `null` for kinds without rules.
pub fn rules(loaded: &Loaded) -> Value {
    if loaded.kind.has_rules() {
        loaded.rule_payloads().into()
    } else {
        Value::Null
    }
}
