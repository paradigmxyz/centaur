//! Credential reconciliation: grants the wrapper static secret of an
//! OAuth-flow credential (a broker credential minted through a non-shared
//! OAuth app) to the user principals that appear to be the same human.
//!
//! A credential matches a user principal when, in order:
//!
//! - Slack credentials agree with the principal on the workspace (when
//!   either side names one);
//! - **console-user principals** match only through their console user:
//!   credential ownership, or the user's email or a verified identity email;
//! - other principals with a provider-native identity (the Slack user id, or
//!   the `google_subject` / `github_subject` label) match on that subject
//!   alone;
//! - otherwise through the credential owner's single Slack identity equal to
//!   the principal's, or by email (`email` / `google_email` labels, the Slack
//!   email).
//!
//! A matched Google credential fills blank `google_subject` / `google_email`
//! labels; a matched Slack credential gives a console-user principal its
//! Slack identity. [`apply_for_principal`], [`apply_for_credential`], and
//! [`reconcile_owner_credentials`] take a pool and a time so any caller can
//! run them; [`super::effects`] adds what follows a changed identity.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::PgPool;

use centaur_console_db::{valid_slack_team_id, valid_slack_user_id};

const USER_KIND: &str = "user";
const CONSOLE_USER_KIND: &str = "console_user";
const SLACK: &str = "slack";
const GOOGLE: &str = "google";
const EMAIL_LABELS: [&str; 2] = ["email", "google_email"];
const SUBJECT_LABELS: [(&str, &str); 2] =
    [("google", "google_subject"), ("github", "github_subject")];
const SLACK_TEAM_LABEL: &str = "slack_team_id";
const DIRECT_PRIORITY: i32 = 100;

/// The OAuth providers whose credentials reconcile.
// TODO(oauth): read the provider registry once the OAuth area exposes it.
pub const PROVIDERS: [&str; 10] = [
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

/// What a run did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Outcome {
    /// Principals the credentials matched (or wrapper grants wanted).
    pub requested: usize,
    /// Grants created.
    pub created: usize,
    /// Principals whose Slack identity (or console user) was filled in.
    pub identity_changed: Vec<i64>,
}

impl Outcome {
    fn absorb(&mut self, other: Outcome) {
        self.requested += other.requested;
        self.created += other.created;
        self.identity_changed.extend(other.identity_changed);
    }
}

#[derive(Clone, Debug, sqlx::FromRow)]
struct Principal {
    id: i64,
    kind: String,
    labels: Value,
    slack_user_id: Option<String>,
    slack_team_id: Option<String>,
    slack_email: Option<String>,
    console_user_id: Option<i64>,
    created_by_id: i64,
}

const PRINCIPAL_SELECT: &str = "SELECT id, kind, labels, slack_user_id, slack_team_id, \
    slack_email, console_user_id, created_by_id FROM principals";

#[derive(Clone, Debug, sqlx::FromRow)]
struct Credential {
    created_by_id: Option<i64>,
    provider: String,
    subject: Option<String>,
    email: Option<String>,
    labels: Value,
    app_labels: Value,
    /// The static secret that wraps the credential.
    secret_id: Option<i64>,
}

const CREDENTIAL_SELECT: &str = "SELECT bc.created_by_id, oa.provider, \
    bc.provider_subject AS subject, bc.provider_email AS email, bc.labels, \
    oa.labels AS app_labels, \
    (SELECT ss.id FROM static_secrets ss WHERE ss.broker_credential_id = bc.id \
     ORDER BY ss.id LIMIT 1) AS secret_id \
    FROM broker_credentials bc JOIN oauth_apps oa ON oa.id = bc.oauth_app_id \
    WHERE NOT oa.shared AND oa.provider = ANY($1)";

fn normalize(value: Option<&str>) -> Option<String> {
    value
        .map(|value| value.trim().to_lowercase())
        .filter(|value| !value.is_empty())
}

fn label(labels: &Value, key: &str) -> Option<String> {
    match labels.get(key)? {
        Value::String(text) => Some(text.clone()),
        Value::Null => None,
        other => Some(other.to_string()),
    }
}

fn present(value: &Option<String>) -> bool {
    value.as_deref().is_some_and(|v| !v.trim().is_empty())
}

fn is_console_user(principal: &Principal) -> bool {
    principal.kind == CONSOLE_USER_KIND
}

/// Whether reconciliation considers the principal a person.
fn is_user_principal(principal: &Principal) -> bool {
    principal.kind == USER_KIND
        || is_console_user(principal)
        || present(&principal.slack_user_id)
        || present(&principal.slack_email)
        || EMAIL_LABELS
            .iter()
            .chain(SUBJECT_LABELS.iter().map(|(_, key)| key))
            .any(|key| label(&principal.labels, key).is_some_and(|v| !v.trim().is_empty()))
}

/// Each user's single, well-formed Slack identity `(user id, workspace)`.
/// A user with several Slack identities, or a malformed one, has none.
pub async fn slack_identities<'e>(
    db: impl sqlx::PgExecutor<'e>,
    user_ids: &[i64],
) -> Result<HashMap<i64, (String, String)>, sqlx::Error> {
    let rows: Vec<(i64, String, Option<String>)> = sqlx::query_as(
        "SELECT user_id, subject, team_id FROM user_identities \
         WHERE provider = 'slack' AND user_id = ANY($1) ORDER BY id",
    )
    .bind(user_ids)
    .fetch_all(db)
    .await?;
    let mut pairs: HashMap<i64, HashSet<(String, String)>> = HashMap::new();
    for (user_id, subject, team_id) in rows {
        if let Some(team_id) = team_id
            && !subject.trim().is_empty()
            && !team_id.trim().is_empty()
        {
            pairs.entry(user_id).or_default().insert((subject, team_id));
        }
    }
    Ok(pairs
        .into_iter()
        .filter_map(|(user_id, pairs)| {
            let mut pairs = pairs.into_iter();
            let (subject, team) = pairs.next()?;
            let single = pairs.next().is_none();
            (single && valid_slack_user_id(&subject) && valid_slack_team_id(&team))
                .then_some((user_id, (subject, team)))
        })
        .collect())
}

/// What matching needs to know about people, loaded once per run.
#[derive(Default)]
struct Directory {
    /// A credential owner's single, well-formed Slack identity
    /// (normalized subject and team).
    owner_slack: HashMap<i64, (String, String)>,
    /// A console user's email and verified identity emails (normalized).
    console_emails: HashMap<i64, Vec<String>>,
}

impl Directory {
    async fn load(
        db: &PgPool,
        principals: &[Principal],
        credentials: &[Credential],
    ) -> Result<Self, sqlx::Error> {
        let owners: Vec<i64> = credentials.iter().filter_map(|c| c.created_by_id).collect();
        let owner_slack = slack_identities(db, &owners)
            .await?
            .into_iter()
            .filter_map(|(user_id, (subject, team))| {
                Some((
                    user_id,
                    (normalize(Some(&subject))?, normalize(Some(&team))?),
                ))
            })
            .collect();

        let users: Vec<i64> = principals
            .iter()
            .filter(|p| is_console_user(p))
            .filter_map(|p| p.console_user_id)
            .collect();
        let emails: Vec<(i64, Option<String>)> = sqlx::query_as(
            "SELECT id, email FROM users WHERE id = ANY($1) \
             UNION ALL SELECT user_id, email FROM user_identities \
                       WHERE user_id = ANY($1) AND email_verified",
        )
        .bind(&users)
        .fetch_all(db)
        .await?;
        let mut console_emails: HashMap<i64, Vec<String>> = HashMap::new();
        for (user_id, email) in emails {
            if let Some(email) = normalize(email.as_deref()) {
                let list = console_emails.entry(user_id).or_default();
                if !list.contains(&email) {
                    list.push(email);
                }
            }
        }
        Ok(Self {
            owner_slack,
            console_emails,
        })
    }

    fn principal_emails(&self, principal: &Principal) -> Vec<String> {
        if is_console_user(principal) {
            return principal
                .console_user_id
                .and_then(|id| self.console_emails.get(&id))
                .cloned()
                .unwrap_or_default();
        }
        let mut emails: Vec<String> = Vec::new();
        for email in EMAIL_LABELS
            .iter()
            .map(|key| label(&principal.labels, key))
            .chain([principal.slack_email.clone()])
        {
            if let Some(email) = normalize(email.as_deref())
                && !emails.contains(&email)
            {
                emails.push(email);
            }
        }
        emails
    }

    fn matches(&self, principal: &Principal, credential: &Credential) -> bool {
        if credential.provider == SLACK && !slack_team_matches(principal, credential) {
            return false;
        }
        let email_matches = || {
            normalize(credential.email.as_deref())
                .is_some_and(|email| self.principal_emails(principal).contains(&email))
        };
        if is_console_user(principal) {
            return credential
                .created_by_id
                .is_some_and(|owner| Some(owner) == principal.console_user_id)
                || email_matches();
        }
        let subjects = principal_subjects(principal, &credential.provider);
        if !subjects.is_empty() {
            return normalize(credential.subject.as_deref())
                .is_some_and(|subject| subjects.contains(&subject));
        }
        let owner_identity = credential
            .created_by_id
            .and_then(|owner| self.owner_slack.get(&owner));
        let principal_identity = normalize(principal.slack_user_id.as_deref())
            .zip(normalize(principal.slack_team_id.as_deref()));
        (principal_identity.is_some() && owner_identity == principal_identity.as_ref())
            || email_matches()
    }
}

/// The provider-native identities of the principal for `provider`.
fn principal_subjects(principal: &Principal, provider: &str) -> Vec<String> {
    if provider == SLACK {
        return normalize(principal.slack_user_id.as_deref())
            .into_iter()
            .collect();
    }
    let mut subjects = Vec::new();
    for (key_provider, key) in SUBJECT_LABELS {
        if key_provider == provider
            && let Some(subject) = normalize(label(&principal.labels, key).as_deref())
            && !subjects.contains(&subject)
        {
            subjects.push(subject);
        }
    }
    subjects
}

/// The workspace a credential belongs to: its own label, else its app's.
fn credential_team(credential: &Credential) -> Option<String> {
    label(&credential.labels, SLACK_TEAM_LABEL)
        .filter(|team| !team.trim().is_empty())
        .or_else(|| {
            label(&credential.app_labels, SLACK_TEAM_LABEL).filter(|team| !team.trim().is_empty())
        })
}

/// Slack user ids are workspace-scoped: when either side names a
/// workspace, both must agree (a console-user principal without one
/// accepts any).
fn slack_team_matches(principal: &Principal, credential: &Credential) -> bool {
    let principal_team = normalize(principal.slack_team_id.as_deref());
    let credential_team = normalize(credential_team(credential).as_deref());
    if principal_team.is_none() && (is_console_user(principal) || credential_team.is_none()) {
        return true;
    }
    principal_team.is_some() && principal_team == credential_team
}

fn unique_present(values: impl IntoIterator<Item = Option<String>>) -> Option<String> {
    let mut seen: Vec<String> = Vec::new();
    for value in values.into_iter().flatten() {
        let value = value.trim().to_owned();
        if !value.is_empty()
            && !seen
                .iter()
                .any(|s| s.to_lowercase() == value.to_lowercase())
        {
            seen.push(value);
        }
    }
    (seen.len() == 1).then(|| seen.remove(0))
}

async fn load_credentials(db: &PgPool, only: Option<i64>) -> Result<Vec<Credential>, sqlx::Error> {
    let providers: Vec<&str> = PROVIDERS.to_vec();
    sqlx::query_as(&format!(
        "{CREDENTIAL_SELECT} AND ($2::bigint IS NULL OR bc.id = $2) ORDER BY bc.id"
    ))
    .bind(providers)
    .bind(only)
    .fetch_all(db)
    .await
}

/// Directly grants `secret_id` to the principal; whether a grant was made.
async fn grant(
    db: &PgPool,
    now: DateTime<Utc>,
    principal: &Principal,
    secret_id: i64,
) -> Result<bool, sqlx::Error> {
    let created = sqlx::query(
        "WITH inserted AS (\
           INSERT INTO grants (principal_id, static_secret_id, priority, created_by_id, created_at, updated_at) \
           VALUES ($1, $2, $3, $4, $5, $5) ON CONFLICT DO NOTHING RETURNING principal_id) \
         UPDATE principals SET sync_config_cache_version = sync_config_cache_version + 1 \
         WHERE id IN (SELECT principal_id FROM inserted)",
    )
    .bind(principal.id)
    .bind(secret_id)
    .bind(DIRECT_PRIORITY)
    .bind(principal.created_by_id)
    .bind(now.naive_utc())
    .execute(db)
    .await?
    .rows_affected();
    Ok(created > 0)
}

/// Copies provider identities from matched credentials into the principal:
/// a console-user principal gets a unique, well-formed Slack identity; any
/// other principal gets blank Google labels filled. Whether the Slack
/// identity changed.
async fn sync_identity(
    db: &PgPool,
    now: DateTime<Utc>,
    principal: &Principal,
    credentials: &[&Credential],
) -> Result<bool, sqlx::Error> {
    if is_console_user(principal) {
        let slack: Vec<_> = credentials.iter().filter(|c| c.provider == SLACK).collect();
        let user_id = unique_present(slack.iter().map(|c| c.subject.clone()));
        let team_id = unique_present(slack.iter().map(|c| credential_team(c)));
        let (Some(user_id), Some(team_id)) = (user_id, team_id) else {
            return Ok(false);
        };
        if !valid_slack_user_id(&user_id)
            || !valid_slack_team_id(&team_id)
            || (principal.slack_user_id.as_deref() == Some(&user_id)
                && principal.slack_team_id.as_deref() == Some(&team_id))
        {
            return Ok(false);
        }
        sqlx::query(
            "UPDATE principals SET slack_user_id = $2, slack_team_id = $3, updated_at = $4, \
             sync_config_cache_version = sync_config_cache_version + 1 WHERE id = $1",
        )
        .bind(principal.id)
        .bind(user_id)
        .bind(team_id)
        .bind(now.naive_utc())
        .execute(db)
        .await?;
        return Ok(true);
    }
    let google: Vec<_> = credentials
        .iter()
        .filter(|c| c.provider == GOOGLE)
        .collect();
    let blank = |key: &str| label(&principal.labels, key).is_none_or(|v| v.trim().is_empty());
    let mut fill = serde_json::Map::new();
    if let Some(subject) = unique_present(google.iter().map(|c| c.subject.clone()))
        && blank("google_subject")
    {
        fill.insert("google_subject".into(), subject.into());
    }
    if let Some(email) = unique_present(google.iter().map(|c| c.email.clone()))
        && blank("google_email")
    {
        fill.insert("google_email".into(), email.into());
    }
    if !fill.is_empty() {
        sqlx::query(
            "UPDATE principals SET labels = labels || $2, updated_at = $3, \
             sync_config_cache_version = sync_config_cache_version + 1 WHERE id = $1",
        )
        .bind(principal.id)
        .bind(Value::Object(fill))
        .bind(now.naive_utc())
        .execute(db)
        .await?;
    }
    Ok(false)
}

/// Grants the matching credentials' wrappers to one principal (after the
/// principal was written). Whether its Slack identity changed.
pub async fn apply_for_principal(
    db: &PgPool,
    now: DateTime<Utc>,
    principal_id: i64,
) -> Result<bool, sqlx::Error> {
    let principal: Option<Principal> = sqlx::query_as(&format!("{PRINCIPAL_SELECT} WHERE id = $1"))
        .bind(principal_id)
        .fetch_optional(db)
        .await?;
    let Some(principal) = principal.filter(is_user_principal) else {
        return Ok(false);
    };
    let credentials = load_credentials(db, None).await?;
    let directory = Directory::load(db, std::slice::from_ref(&principal), &credentials).await?;
    let matched: Vec<&Credential> = credentials
        .iter()
        .filter(|credential| directory.matches(&principal, credential))
        .collect();
    for secret_id in matched.iter().filter_map(|c| c.secret_id) {
        grant(db, now, &principal, secret_id).await?;
    }
    sync_identity(db, now, &principal, &matched).await
}

/// Grants one credential's wrapper to every user principal it matches (a
/// wrapper or a credential identity just appeared).
pub async fn apply_for_credential(
    db: &PgPool,
    now: DateTime<Utc>,
    credential_id: i64,
) -> Result<Outcome, sqlx::Error> {
    let credentials = load_credentials(db, Some(credential_id)).await?;
    let mut outcome = Outcome::default();
    let Some(credential) = credentials.first().filter(|c| c.secret_id.is_some()) else {
        return Ok(outcome);
    };
    let principals: Vec<Principal> = sqlx::query_as(&format!("{PRINCIPAL_SELECT} ORDER BY id"))
        .fetch_all(db)
        .await?;
    let principals: Vec<Principal> = principals.into_iter().filter(is_user_principal).collect();
    let directory = Directory::load(db, &principals, &credentials).await?;
    for principal in principals
        .iter()
        .filter(|principal| directory.matches(principal, credential))
    {
        outcome.requested += 1;
        if let Some(secret_id) = credential.secret_id
            && grant(db, now, principal, secret_id).await?
        {
            outcome.created += 1;
        }
        if sync_identity(db, now, principal, &[credential]).await? {
            outcome.identity_changed.push(principal.id);
        }
    }
    Ok(outcome)
}

/// A user's Slack identity arrived: reconcile every OAuth credential they
/// created.
pub async fn reconcile_owner_credentials(
    db: &PgPool,
    now: DateTime<Utc>,
    user_id: i64,
) -> Result<Outcome, sqlx::Error> {
    let ids: Vec<i64> = sqlx::query_scalar(
        "SELECT bc.id FROM broker_credentials bc JOIN oauth_apps oa ON oa.id = bc.oauth_app_id \
         WHERE bc.created_by_id = $1 ORDER BY bc.id",
    )
    .bind(user_id)
    .fetch_all(db)
    .await?;
    let mut outcome = Outcome::default();
    for id in ids {
        outcome.absorb(apply_for_credential(db, now, id).await?);
    }
    Ok(outcome)
}
