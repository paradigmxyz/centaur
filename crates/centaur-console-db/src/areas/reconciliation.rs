//! Console's principal credential reconciliation.
//!
//! Callers invoke it explicitly, inside the same transaction as the write:
//!
//! - after a principal is created or its identity changes: [`apply_for_principal`]
//! - after a broker credential (or its wrapper static secret) is created or
//!   changes: [`apply_for_credential`]
//! - when validating a direct grant: [`connection_owner`]

use serde_json::Value;
use sqlx::PgConnection;
use time::PrimitiveDateTime;

use crate::Result;
use crate::invalidate::{self, Scope};
use crate::validation::rules::matches;

const USER_KIND: &str = "user";
const CONSOLE_USER_KIND: &str = "console_user";
const SLACK_PROVIDER: &str = "slack";
const GOOGLE_PROVIDER: &str = "google";
const GITHUB_PROVIDER: &str = "github";
const EMAIL_LABELS: [&str; 2] = ["email", "google_email"];
const SLACK_TEAM_LABEL: &str = "slack_team_id";
const DEFAULT_DIRECT_PRIORITY: i32 = 100;
const PROVIDERS: [&str; 10] = [
    "attio",
    "custom",
    "github",
    "google",
    "granola",
    "linear",
    "mercator",
    "microsoft",
    "slack",
    "zoom",
];

const SLACK_USER_ID: &str = r"^(?:[UW][A-Z0-9]{8,}|USLACK)$";
const SLACK_TEAM_ID: &str = r"^[TE][A-Z0-9]{8,}$";

fn normalize(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_lowercase)
}

fn label<'a>(labels: &'a Value, key: &str) -> Option<&'a str> {
    labels
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn now() -> PrimitiveDateTime {
    let utc = time::OffsetDateTime::now_utc();
    PrimitiveDateTime::new(utc.date(), utc.time())
}

fn unique_present(values: impl IntoIterator<Item = Option<String>>) -> Option<String> {
    let mut present: Vec<String> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    for value in values.into_iter().flatten() {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            continue;
        }
        let key = trimmed.to_lowercase();
        if seen.contains(&key) {
            continue;
        }
        seen.push(key);
        present.push(trimmed.to_owned());
    }
    (present.len() == 1).then(|| present.remove(0))
}

#[derive(Clone, Debug, sqlx::FromRow)]
struct Principal {
    id: i64,
    kind: String,
    #[sqlx(json)]
    labels: Value,
    slack_user_id: Option<String>,
    slack_team_id: Option<String>,
    slack_email: Option<String>,
    console_user_id: Option<i64>,
    created_by_id: i64,
}

impl Principal {
    fn console_user(&self) -> bool {
        self.kind == CONSOLE_USER_KIND
    }

    fn user_principal(&self) -> bool {
        if self.kind == USER_KIND || self.kind == CONSOLE_USER_KIND {
            return true;
        }
        if self
            .slack_user_id
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
            || self
                .slack_email
                .as_deref()
                .is_some_and(|value| !value.trim().is_empty())
        {
            return true;
        }
        EMAIL_LABELS
            .iter()
            .chain(["google_subject", "github_subject"].iter())
            .any(|key| label(&self.labels, key).is_some())
    }

    fn slack_identity(&self) -> Option<(String, String)> {
        if self.console_user() {
            return None;
        }
        Some((
            normalize(self.slack_user_id.as_deref())?,
            normalize(self.slack_team_id.as_deref())?,
        ))
    }

    fn subjects(&self, provider: &str) -> Vec<String> {
        if self.console_user() {
            return Vec::new();
        }
        if provider == SLACK_PROVIDER {
            return normalize(self.slack_user_id.as_deref())
                .into_iter()
                .collect();
        }
        let keys: &[&str] = match provider {
            GOOGLE_PROVIDER => &["google_subject"],
            GITHUB_PROVIDER => &["github_subject"],
            _ => &[],
        };
        let mut subjects: Vec<String> = keys
            .iter()
            .filter_map(|key| normalize(label(&self.labels, key)))
            .collect();
        subjects.sort();
        subjects.dedup();
        subjects
    }
}

#[derive(Clone, Debug, sqlx::FromRow)]
struct Credential {
    id: i64,
    created_by_id: Option<i64>,
    provider_subject: Option<String>,
    provider_email: Option<String>,
    #[sqlx(json)]
    labels: Value,
    static_secret_id: Option<i64>,
    provider: String,
    shared: bool,
    owner_only: bool,
    #[sqlx(json)]
    oauth_labels: Value,
}

impl Credential {
    fn supported(&self) -> bool {
        !self.shared && PROVIDERS.contains(&self.provider.as_str())
    }

    fn team(&self) -> Option<String> {
        normalize(label(&self.labels, SLACK_TEAM_LABEL))
            .or_else(|| normalize(label(&self.oauth_labels, SLACK_TEAM_LABEL)))
    }
}

const CREDENTIAL_QUERY: &str = "SELECT c.id, c.created_by_id, c.provider_subject, c.provider_email, \
        c.labels, s.id AS static_secret_id, a.provider, a.shared, a.owner_only, a.labels AS oauth_labels \
     FROM broker_credentials c \
     JOIN oauth_apps a ON a.id = c.oauth_app_id \
     LEFT JOIN static_secrets s ON s.broker_credential_id = c.id";

const PRINCIPAL_PROJECTION: &str = "SELECT id, kind, labels, slack_user_id, slack_team_id, slack_email, \
            console_user_id, created_by_id";

async fn load_principal(conn: &mut PgConnection, id: i64) -> Result<Option<Principal>> {
    Ok(sqlx::query_as::<_, Principal>(&format!(
        "{PRINCIPAL_PROJECTION} FROM principals WHERE id = $1"
    ))
    .bind(id)
    .fetch_optional(&mut *conn)
    .await?)
}

async fn principal_emails(conn: &mut PgConnection, principal: &Principal) -> Result<Vec<String>> {
    if principal.console_user() {
        let Some(user_id) = principal.console_user_id else {
            return Ok(Vec::new());
        };
        let rows: Vec<(Option<String>,)> = sqlx::query_as(
            "SELECT email FROM (SELECT email FROM users WHERE id = $1 \
             UNION SELECT email FROM user_identities WHERE user_id = $1 AND email_verified) emails",
        )
        .bind(user_id)
        .fetch_all(&mut *conn)
        .await?;
        return Ok(rows
            .into_iter()
            .filter_map(|(email,)| normalize(email.as_deref()))
            .collect::<Vec<_>>());
    }
    let mut emails: Vec<String> = EMAIL_LABELS
        .iter()
        .filter_map(|key| normalize(label(&principal.labels, key)))
        .collect();
    if let Some(email) = normalize(principal.slack_email.as_deref()) {
        emails.push(email);
    }
    emails.sort();
    emails.dedup();
    Ok(emails)
}

/// `UserIdentity.unambiguous_slack_identity` for one owner.
async fn owner_slack_identity(
    conn: &mut PgConnection,
    user_id: i64,
) -> Result<Option<(String, String)>> {
    let rows: Vec<(Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT subject, team_id FROM user_identities WHERE provider = 'slack' AND user_id = $1",
    )
    .bind(user_id)
    .fetch_all(&mut *conn)
    .await?;
    let mut pairs: Vec<(String, String)> = Vec::new();
    for (subject, team_id) in rows {
        let (Some(subject), Some(team_id)) = (subject, team_id) else {
            continue;
        };
        if subject.trim().is_empty() || team_id.trim().is_empty() {
            continue;
        }
        let pair = (subject, team_id);
        if !pairs.contains(&pair) {
            pairs.push(pair);
        }
    }
    if pairs.len() != 1 {
        return Ok(None);
    }
    let (subject, team_id) = pairs.remove(0);
    if !matches(SLACK_USER_ID, &subject) || !matches(SLACK_TEAM_ID, &team_id) {
        return Ok(None);
    }
    Ok(Some((subject.to_lowercase(), team_id.to_lowercase())))
}

async fn owner_matches(
    conn: &mut PgConnection,
    principal: &Principal,
    credential: &Credential,
) -> Result<bool> {
    let Some(identity) = principal.slack_identity() else {
        return Ok(false);
    };
    let Some(owner_id) = credential.created_by_id else {
        return Ok(false);
    };
    Ok(owner_slack_identity(conn, owner_id).await?.as_ref() == Some(&identity))
}

fn slack_team_matches(principal: &Principal, credential: &Credential) -> bool {
    let principal_team = normalize(principal.slack_team_id.as_deref());
    let credential_team = credential.team();
    if principal.console_user() && principal_team.is_none() {
        return true;
    }
    if principal_team.is_none() && credential_team.is_none() {
        return true;
    }
    principal_team.is_some() && principal_team == credential_team
}

async fn matching_credential(
    conn: &mut PgConnection,
    principal: &Principal,
    credential: &Credential,
) -> Result<bool> {
    if !credential.supported() {
        return Ok(false);
    }
    if credential.owner_only {
        return connection_owner_for(conn, principal, credential).await;
    }
    if credential.provider == SLACK_PROVIDER && !slack_team_matches(principal, credential) {
        return Ok(false);
    }
    let emails = principal_emails(conn, principal).await?;
    if principal.console_user() {
        return Ok(credential.created_by_id == principal.console_user_id
            || normalize(credential.provider_email.as_deref())
                .is_some_and(|email| emails.contains(&email)));
    }
    let subjects = principal.subjects(&credential.provider);
    if !subjects.is_empty() {
        return Ok(normalize(credential.provider_subject.as_deref())
            .is_some_and(|subject| subjects.contains(&subject)));
    }
    if owner_matches(conn, principal, credential).await? {
        return Ok(true);
    }
    Ok(
        normalize(credential.provider_email.as_deref())
            .is_some_and(|email| emails.contains(&email)),
    )
}

async fn connection_owner_for(
    conn: &mut PgConnection,
    principal: &Principal,
    credential: &Credential,
) -> Result<bool> {
    let Some(owner_id) = credential.created_by_id else {
        return Ok(false);
    };
    let active: Option<bool> =
        sqlx::query_scalar("SELECT status = 'active' FROM users WHERE id = $1")
            .bind(owner_id)
            .fetch_optional(&mut *conn)
            .await?;
    if active != Some(true) {
        return Ok(false);
    }
    if principal.console_user() {
        return Ok(principal.console_user_id == Some(owner_id));
    }
    owner_matches(conn, principal, credential).await
}

async fn grant_credential(
    conn: &mut PgConnection,
    principal: &Principal,
    credential: &Credential,
) -> Result<bool> {
    let Some(secret_id) = credential.static_secret_id else {
        return Ok(false);
    };
    let inserted = sqlx::query(
        "INSERT INTO grants (principal_id, static_secret_id, priority, created_by_id, \
          created_at, updated_at) VALUES ($1,$2,$3,$4,$5,$5) \
         ON CONFLICT (principal_id, static_secret_id) \
           WHERE principal_id IS NOT NULL AND static_secret_id IS NOT NULL DO NOTHING",
    )
    .bind(principal.id)
    .bind(secret_id)
    .bind(DEFAULT_DIRECT_PRIORITY)
    .bind(principal.created_by_id)
    .bind(now())
    .execute(&mut *conn)
    .await?
    .rows_affected()
        == 1;
    if inserted {
        invalidate::invalidate(conn, Scope::Principal(principal.id)).await?;
    }
    Ok(inserted)
}

async fn update_labels(
    conn: &mut PgConnection,
    principal: &mut Principal,
    updates: &[(&str, String)],
) -> Result<()> {
    if updates.is_empty() {
        return Ok(());
    }
    let mut labels = principal.labels.as_object().cloned().unwrap_or_default();
    for (key, value) in updates {
        labels.insert((*key).to_owned(), Value::String(value.clone()));
    }
    let labels = Value::Object(labels);
    sqlx::query("UPDATE principals SET labels = $1, updated_at = $2 WHERE id = $3")
        .bind(&labels)
        .bind(now())
        .bind(principal.id)
        .execute(&mut *conn)
        .await?;
    principal.labels = labels;
    invalidate::invalidate(conn, Scope::Principal(principal.id)).await?;
    Ok(())
}

async fn sync_provider_labels(
    conn: &mut PgConnection,
    principal: &mut Principal,
    credentials: &[Credential],
) -> Result<()> {
    if principal.console_user() {
        let slack: Vec<&Credential> = credentials
            .iter()
            .filter(|credential| credential.provider == SLACK_PROVIDER)
            .collect();
        if slack.is_empty() {
            return Ok(());
        }
        let Some(subject) = unique_present(
            slack
                .iter()
                .map(|credential| credential.provider_subject.clone()),
        ) else {
            return Ok(());
        };
        let Some(team) = unique_present(slack.iter().map(|credential| credential.team())) else {
            return Ok(());
        };
        if !matches(SLACK_USER_ID, &subject) || !matches(SLACK_TEAM_ID, &team) {
            return Ok(());
        }
        if principal.slack_user_id.as_deref() == Some(subject.as_str())
            && principal.slack_team_id.as_deref() == Some(team.as_str())
        {
            return Ok(());
        }
        sqlx::query(
            "UPDATE principals SET slack_user_id = $1, slack_team_id = $2, updated_at = $3 \
             WHERE id = $4",
        )
        .bind(&subject)
        .bind(&team)
        .bind(now())
        .bind(principal.id)
        .execute(&mut *conn)
        .await?;
        principal.slack_user_id = Some(subject);
        principal.slack_team_id = Some(team);
        invalidate::invalidate(conn, Scope::Principal(principal.id)).await?;
        return Ok(());
    }

    let google: Vec<&Credential> = credentials
        .iter()
        .filter(|credential| credential.provider == GOOGLE_PROVIDER)
        .collect();
    if google.is_empty() {
        return Ok(());
    }
    let subject = unique_present(
        google
            .iter()
            .map(|credential| credential.provider_subject.clone()),
    );
    let email = unique_present(
        google
            .iter()
            .map(|credential| credential.provider_email.clone()),
    );
    let mut updates: Vec<(&str, String)> = Vec::new();
    if let Some(subject) = subject
        && label(&principal.labels, "google_subject").is_none()
    {
        updates.push(("google_subject", subject));
    }
    if let Some(email) = email
        && label(&principal.labels, "google_email").is_none()
    {
        updates.push(("google_email", email));
    }
    update_labels(conn, principal, &updates).await
}

fn dedupe(credentials: Vec<Credential>) -> Vec<Credential> {
    let mut seen: Vec<i64> = Vec::new();
    let mut out = Vec::new();
    for credential in credentials {
        if seen.contains(&credential.id) {
            continue;
        }
        seen.push(credential.id);
        out.push(credential);
    }
    out
}

async fn credentials_for_principal(
    conn: &mut PgConnection,
    principal: &Principal,
) -> Result<Vec<Credential>> {
    let mut matched: Vec<Credential> = Vec::new();
    for provider in PROVIDERS {
        let credentials: Vec<Credential> = sqlx::query_as(&format!(
            "{CREDENTIAL_QUERY} WHERE a.provider = $1 AND a.shared = false ORDER BY c.id"
        ))
        .bind(provider)
        .fetch_all(&mut *conn)
        .await?;

        if principal.console_user() {
            let mut selected = Vec::new();
            for credential in &credentials {
                if matching_credential(conn, principal, credential).await? {
                    selected.push(credential.clone());
                }
            }
            matched.extend(selected);
            continue;
        }

        let subjects = principal.subjects(provider);
        let mut native: Vec<Credential> = Vec::new();
        if !subjects.is_empty() {
            for credential in &credentials {
                if normalize(credential.provider_subject.as_deref())
                    .is_some_and(|subject| subjects.contains(&subject))
                    && matching_credential(conn, principal, credential).await?
                {
                    native.push(credential.clone());
                }
            }
        }
        if !native.is_empty() {
            matched.extend(dedupe(native));
            continue;
        }

        let mut selected = Vec::new();
        for credential in &credentials {
            if matching_credential(conn, principal, credential).await? {
                selected.push(credential.clone());
            }
        }
        matched.extend(dedupe(selected));
    }
    Ok(dedupe(matched))
}

async fn apply_entry(conn: &mut PgConnection, principal: &mut Principal) -> Result<()> {
    if !principal.user_principal() {
        return Ok(());
    }
    let credentials = credentials_for_principal(conn, principal).await?;
    if credentials.is_empty() {
        return Ok(());
    }
    for credential in &credentials {
        grant_credential(conn, principal, credential).await?;
    }
    sync_provider_labels(conn, principal, &credentials).await
}

/// Grants matching OAuth-flow credentials to a user principal.
pub async fn apply_for_principal(conn: &mut PgConnection, principal_id: i64) -> Result<()> {
    let Some(mut principal) = load_principal(conn, principal_id).await? else {
        return Ok(());
    };
    apply_entry(conn, &mut principal).await
}

/// Grants a broker credential's wrapper static secret to matching principals.
pub async fn apply_for_credential(
    conn: &mut PgConnection,
    broker_credential_id: i64,
) -> Result<()> {
    let Some(credential) =
        sqlx::query_as::<_, Credential>(&format!("{CREDENTIAL_QUERY} WHERE c.id = $1"))
            .bind(broker_credential_id)
            .fetch_optional(&mut *conn)
            .await?
    else {
        return Ok(());
    };
    if credential.static_secret_id.is_none() || !credential.supported() {
        return Ok(());
    }
    let ids: Vec<i64> = sqlx::query_scalar("SELECT id FROM principals ORDER BY id")
        .fetch_all(&mut *conn)
        .await?;
    for id in ids {
        let Some(mut principal) = load_principal(conn, id).await? else {
            continue;
        };
        if !principal.user_principal() {
            continue;
        }
        if !matching_credential(conn, &principal, &credential).await? {
            continue;
        }
        grant_credential(conn, &principal, &credential).await?;
        sync_provider_labels(conn, &mut principal, std::slice::from_ref(&credential)).await?;
    }
    Ok(())
}

/// Whether `principal_id` owns the connection behind `broker_credential_id`.
pub async fn connection_owner(
    conn: &mut PgConnection,
    principal_id: i64,
    broker_credential_id: i64,
) -> Result<bool> {
    let Some(principal) = load_principal(conn, principal_id).await? else {
        return Ok(false);
    };
    let Some(credential) =
        sqlx::query_as::<_, Credential>(&format!("{CREDENTIAL_QUERY} WHERE c.id = $1"))
            .bind(broker_credential_id)
            .fetch_optional(&mut *conn)
            .await?
    else {
        return Ok(false);
    };
    connection_owner_for(conn, &principal, &credential).await
}
