//! Principals: list, show, lookup, create, upsert (`PUT`/`PATCH` by oid or
//! `foreign_id`), and destroy.

use crate::identity::provision::valid_email;
use axum::{extract::State, http::StatusCode, response::IntoResponse};
use centaur_console_db::{
    decode_oid, {valid_slack_team_id, valid_slack_user_id},
};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{Map, Value};
use sqlx::{PgConnection, Postgres, QueryBuilder};

use super::{
    Actor, FieldErrors, LabelFilter, check_foreign_id, effects, like_pattern, maybe,
    payload::{self, PRINCIPAL_COLUMNS, PrincipalRow, REPO_CACHE_LABEL},
    permissions::{self, PermissionInput},
};
use crate::{
    ApiError, ApiResult, AppState,
    extract::{Data, Path, Query, data},
    pagination::PageParams,
};
use centaur_console_db::sync_config::bump_sync_config;

pub const KINDS: [&str; 11] = [
    "unknown",
    "user",
    "console_user",
    "workflow",
    "slack_channel",
    "slack_dm",
    "discord_channel",
    "linear_issue",
    "github_user",
    "teams_user",
    "teams_conversation",
];
pub const REPO_CACHE_MODES: [&str; 3] = ["none", "public", "all"];

/// Request body of create and upsert. An absent key leaves the value
/// alone; `null` clears the nullable ones.
#[derive(Default, Deserialize)]
pub struct PrincipalInput {
    /// Create only; an upsert takes the foreign id from the path.
    pub foreign_id: Option<String>,
    #[serde(default, deserialize_with = "maybe")]
    pub name: Option<Option<String>>,
    pub kind: Option<String>,
    #[serde(default, deserialize_with = "maybe")]
    pub slack_user_id: Option<Option<String>>,
    #[serde(default, deserialize_with = "maybe")]
    pub slack_channel_id: Option<Option<String>>,
    #[serde(default, deserialize_with = "maybe")]
    pub slack_team_id: Option<Option<String>>,
    #[serde(default, deserialize_with = "maybe")]
    pub slack_email: Option<Option<String>>,
    #[serde(default, deserialize_with = "maybe")]
    pub console_user_id: Option<Option<i64>>,
    pub sandbox_repo_cache: Option<String>,
    pub sandbox_observability_enabled: Option<bool>,
    pub sandbox_sessions_read_enabled: Option<bool>,
    pub sandbox_workflows_read_enabled: Option<bool>,
    pub sandbox_workflows_write_enabled: Option<bool>,
    pub labels: Option<Map<String, Value>>,
    /// Replaces the principal's own permissions when present.
    pub slack_channel_permissions: Option<Vec<PermissionInput>>,
}

/// The columns a write can change.
#[derive(Clone, Debug, PartialEq, sqlx::FromRow)]
pub struct Fields {
    pub name: Option<String>,
    pub kind: String,
    pub labels: Value,
    pub slack_user_id: Option<String>,
    pub slack_channel_id: Option<String>,
    pub slack_team_id: Option<String>,
    pub slack_email: Option<String>,
    pub console_user_id: Option<i64>,
    pub sandbox_repo_cache: String,
    pub sandbox_observability_enabled: bool,
    pub sandbox_sessions_read_enabled: bool,
    pub sandbox_workflows_read_enabled: bool,
    pub sandbox_workflows_write_enabled: bool,
}

const FIELD_COLUMNS: &str = "name, kind, labels, slack_user_id, slack_channel_id, slack_team_id, \
    slack_email, console_user_id, sandbox_repo_cache, sandbox_observability_enabled, \
    sandbox_sessions_read_enabled, sandbox_workflows_read_enabled, sandbox_workflows_write_enabled";

impl Fields {
    /// A new principal's values: the system's default sandbox capabilities.
    pub async fn defaults(conn: &mut PgConnection) -> Result<Self, sqlx::Error> {
        let settings: Option<(String, bool, bool, bool, bool)> = sqlx::query_as(
            "SELECT default_sandbox_repo_cache, default_sandbox_observability_enabled, \
             default_sandbox_sessions_read_enabled, default_sandbox_workflows_read_enabled, \
             default_sandbox_workflows_write_enabled FROM system_settings ORDER BY id LIMIT 1",
        )
        .fetch_optional(conn)
        .await?;
        let (repo_cache, observability, sessions, workflows_read, workflows_write) =
            settings.unwrap_or(("all".into(), true, false, false, false));
        Ok(Self {
            name: None,
            kind: "unknown".into(),
            labels: Value::Object(Map::new()),
            slack_user_id: None,
            slack_channel_id: None,
            slack_team_id: None,
            slack_email: None,
            console_user_id: None,
            sandbox_repo_cache: repo_cache,
            sandbox_observability_enabled: observability,
            sandbox_sessions_read_enabled: sessions,
            sandbox_workflows_read_enabled: workflows_read,
            sandbox_workflows_write_enabled: workflows_write,
        })
    }

    fn apply(&mut self, input: &PrincipalInput) {
        fn set<T: Clone>(slot: &mut T, value: &Option<T>) {
            if let Some(value) = value {
                *slot = value.clone();
            }
        }
        set(&mut self.name, &input.name);
        set(&mut self.slack_user_id, &input.slack_user_id);
        set(&mut self.slack_channel_id, &input.slack_channel_id);
        set(&mut self.slack_team_id, &input.slack_team_id);
        set(&mut self.slack_email, &input.slack_email);
        set(&mut self.console_user_id, &input.console_user_id);
        set(&mut self.kind, &input.kind);
        set(&mut self.sandbox_repo_cache, &input.sandbox_repo_cache);
        set(
            &mut self.sandbox_observability_enabled,
            &input.sandbox_observability_enabled,
        );
        set(
            &mut self.sandbox_sessions_read_enabled,
            &input.sandbox_sessions_read_enabled,
        );
        set(
            &mut self.sandbox_workflows_read_enabled,
            &input.sandbox_workflows_read_enabled,
        );
        set(
            &mut self.sandbox_workflows_write_enabled,
            &input.sandbox_workflows_write_enabled,
        );
        if let Some(labels) = &input.labels {
            self.labels = Value::Object(labels.clone());
        }
    }

    /// `labels` always mirrors `sandbox_repo_cache`.
    pub fn sync_repo_cache_label(&mut self) {
        let mut labels = self.labels.as_object().cloned().unwrap_or_default();
        labels.insert(
            REPO_CACHE_LABEL.into(),
            self.sandbox_repo_cache.clone().into(),
        );
        self.labels = Value::Object(labels);
    }

    /// Checks the values that are new or changed relative to `before`
    /// (older rows may hold values that would no longer pass).
    pub fn validate(&self, before: Option<&Fields>) -> FieldErrors {
        let mut errors = FieldErrors::new();
        if !REPO_CACHE_MODES.contains(&self.sandbox_repo_cache.as_str()) {
            errors.add("sandbox_repo_cache", "is not included in the list");
        }
        if self.kind.trim().is_empty() {
            errors.add("kind", "can't be blank");
        }
        if !KINDS.contains(&self.kind.as_str()) {
            errors.add("kind", format!("must be one of {}", KINDS.join(", ")));
        }
        let mut check = |field: &str,
                         value: &Option<String>,
                         old: Option<&Option<String>>,
                         valid: fn(&str) -> bool,
                         message: &str| {
            if let Some(text) = value
                && old != Some(value)
                && !valid(text)
            {
                errors.add(field, message);
            }
        };
        check(
            "slack_user_id",
            &self.slack_user_id,
            before.map(|b| &b.slack_user_id),
            valid_slack_user_id,
            "is not a valid Slack user ID",
        );
        check(
            "slack_channel_id",
            &self.slack_channel_id,
            before.map(|b| &b.slack_channel_id),
            permissions::valid_channel_id,
            "is not a valid Slack channel ID",
        );
        check(
            "slack_team_id",
            &self.slack_team_id,
            before.map(|b| &b.slack_team_id),
            valid_slack_team_id,
            "is not a valid Slack scope ID",
        );
        check(
            "slack_email",
            &self.slack_email,
            before.map(|b| &b.slack_email),
            valid_email,
            "is not a valid email address",
        );
        errors
    }
}

/// What a write did, for what follows the commit.
pub struct Saved {
    pub id: i64,
    pub created: bool,
    /// Fields the personal-key and credential matching read changed.
    pub identity_changed: bool,
    pub slack_channel_id: Option<String>,
}

/// A principal's console user, linked through the Slack email of a DM
/// principal.
async fn link_console_user(
    conn: &mut PgConnection,
    fields: &mut Fields,
    email: &Option<Option<String>>,
) -> Result<(), sqlx::Error> {
    let Some(Some(email)) = email else {
        return Ok(());
    };
    if email.trim().is_empty() || fields.kind != "slack_dm" {
        return Ok(());
    }
    fields.console_user_id =
        sqlx::query_scalar("SELECT id FROM users WHERE email = lower(btrim($1))")
            .bind(email)
            .fetch_optional(conn)
            .await?;
    Ok(())
}

/// Inserts a principal and assigns the default roles; `None` when the
/// `foreign_id` is taken.
pub async fn insert(
    conn: &mut PgConnection,
    created_by: i64,
    foreign_id: Option<&str>,
    fields: &Fields,
    now: DateTime<Utc>,
) -> Result<Option<i64>, sqlx::Error> {
    let now = now.naive_utc();
    let id: Option<i64> = sqlx::query_scalar(
        "INSERT INTO principals (foreign_id, name, kind, labels, slack_user_id, slack_channel_id, \
         slack_team_id, slack_email, console_user_id, sandbox_repo_cache, \
         sandbox_observability_enabled, sandbox_sessions_read_enabled, \
         sandbox_workflows_read_enabled, sandbox_workflows_write_enabled, created_by_id, \
         created_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $16) \
         ON CONFLICT (foreign_id) DO NOTHING RETURNING id",
    )
    .bind(foreign_id)
    .bind(&fields.name)
    .bind(&fields.kind)
    .bind(&fields.labels)
    .bind(&fields.slack_user_id)
    .bind(&fields.slack_channel_id)
    .bind(&fields.slack_team_id)
    .bind(&fields.slack_email)
    .bind(fields.console_user_id)
    .bind(&fields.sandbox_repo_cache)
    .bind(fields.sandbox_observability_enabled)
    .bind(fields.sandbox_sessions_read_enabled)
    .bind(fields.sandbox_workflows_read_enabled)
    .bind(fields.sandbox_workflows_write_enabled)
    .bind(created_by)
    .bind(now)
    .fetch_optional(&mut *conn)
    .await?;
    if let Some(id) = id {
        sqlx::query(
            "INSERT INTO principal_roles (principal_id, role_id, created_at, updated_at) \
             SELECT $1, id, $2, $2 FROM roles WHERE assign_by_default ORDER BY id",
        )
        .bind(id)
        .bind(now)
        .execute(conn)
        .await?;
    }
    Ok(id)
}

/// Writes only the columns that differ, bumps the sync config when the
/// principal's served config can change, and returns the changed columns.
pub async fn update(
    conn: &mut PgConnection,
    id: i64,
    before: &Fields,
    after: &Fields,
    now: DateTime<Utc>,
) -> ApiResult<Vec<&'static str>> {
    let mut changed: Vec<&'static str> = Vec::new();
    let mut query = QueryBuilder::<Postgres>::new("UPDATE principals SET updated_at = ");
    query.push_bind(now.naive_utc());
    macro_rules! set {
        ($($column:ident),*) => {$(
            if before.$column != after.$column {
                query.push(concat!(", ", stringify!($column), " = "));
                query.push_bind(after.$column.clone());
                changed.push(stringify!($column));
            }
        )*};
    }
    set!(
        name,
        kind,
        labels,
        slack_user_id,
        slack_channel_id,
        slack_team_id,
        slack_email,
        console_user_id,
        sandbox_repo_cache,
        sandbox_observability_enabled,
        sandbox_sessions_read_enabled,
        sandbox_workflows_read_enabled,
        sandbox_workflows_write_enabled
    );
    if changed.is_empty() {
        return Ok(changed);
    }
    query.push(" WHERE id = ");
    query.push_bind(id);
    query.build().execute(&mut *conn).await?;
    if changed != ["sandbox_observability_enabled"] {
        bump_sync_config(conn, &[id]).await?;
    }
    Ok(changed)
}

pub fn identity_changed(changed: &[&str]) -> bool {
    changed.iter().any(|c| {
        matches!(
            *c,
            "kind" | "console_user_id" | "slack_user_id" | "slack_team_id"
        )
    })
}

/// Applies `input` to a new principal or to the one `target` names, in one
/// transaction.
pub(super) async fn write(
    state: &AppState,
    actor: i64,
    target: Target<'_>,
    input: &PrincipalInput,
) -> ApiResult<Saved> {
    let now = state.now();
    let mut tx = state.db.begin().await?;
    let mut attempts = 0;
    let saved = loop {
        attempts += 1;
        let existing = match target {
            Target::New(_) => None,
            Target::Id(_) | Target::ForeignId(_) => load(&mut tx, target).await?,
        };
        if let (Target::Id(_), None) = (target, &existing) {
            return Err(ApiError::not_found());
        }
        if let Some((id, before)) = existing {
            let mut after = before.clone();
            after.apply(input);
            link_console_user(&mut tx, &mut after, &input.slack_email).await?;
            after.sync_repo_cache_label();
            let errors = after.validate(Some(&before));
            check_console_user(
                &mut tx,
                errors,
                after.console_user_id,
                before.console_user_id,
            )
            .await?;
            let changed = update(&mut tx, id, &before, &after, now).await?;
            break Saved {
                id,
                created: false,
                identity_changed: identity_changed(&changed),
                slack_channel_id: None,
            };
        }
        let foreign_id = match target {
            Target::New(foreign_id) => foreign_id,
            Target::ForeignId(foreign_id) => Some(foreign_id),
            Target::Id(_) => unreachable!("an oid target resolves or is a 404"),
        };
        let mut fields = Fields::defaults(&mut tx).await?;
        fields.apply(input);
        link_console_user(&mut tx, &mut fields, &input.slack_email).await?;
        fields.sync_repo_cache_label();
        let mut errors = fields.validate(None);
        check_foreign_id(&mut errors, foreign_id, "prn");
        check_console_user(&mut tx, errors, fields.console_user_id, None).await?;
        match insert(&mut tx, actor, foreign_id, &fields, now).await? {
            Some(id) => {
                break Saved {
                    id,
                    created: true,
                    identity_changed: true,
                    slack_channel_id: fields.slack_channel_id.clone(),
                };
            }
            // Lost a race for the foreign id: an upsert updates the winner,
            // a plain create reports the conflict.
            None if matches!(target, Target::ForeignId(_)) && attempts < 2 => {}
            None => return Err(ApiError::field("foreign_id", "has already been taken")),
        }
    };
    if let Some(rows) = &input.slack_channel_permissions {
        permissions::replace_in(&mut tx, super::Grantee::Principal(saved.id), rows, now).await?;
    }
    tx.commit().await?;
    Ok(saved)
}

/// Rejects a `console_user_id` that names no user (when it changed).
async fn check_console_user(
    tx: &mut PgConnection,
    mut errors: FieldErrors,
    new: Option<i64>,
    old: Option<i64>,
) -> ApiResult<()> {
    if let Some(id) = new
        && new != old
        && !sqlx::query_scalar::<_, bool>("SELECT EXISTS (SELECT 1 FROM users WHERE id = $1)")
            .bind(id)
            .fetch_one(tx)
            .await?
    {
        errors.add("console_user_id", "must exist");
    }
    errors.finish()
}

/// How a write finds its principal.
#[derive(Clone, Copy)]
pub(super) enum Target<'a> {
    /// A new principal with this (optional) foreign id.
    New(Option<&'a str>),
    Id(i64),
    /// By foreign id, created when unknown.
    ForeignId(&'a str),
}

#[derive(sqlx::FromRow)]
struct Locked {
    id: i64,
    #[sqlx(flatten)]
    fields: Fields,
}

/// The principal's id and editable values, row-locked.
async fn load(
    tx: &mut PgConnection,
    target: Target<'_>,
) -> Result<Option<(i64, Fields)>, sqlx::Error> {
    let select = format!("SELECT id, {FIELD_COLUMNS} FROM principals");
    let row: Option<Locked> = match target {
        Target::Id(id) => {
            sqlx::query_as(&format!("{select} WHERE id = $1 FOR UPDATE"))
                .bind(id)
                .fetch_optional(tx)
                .await?
        }
        Target::ForeignId(foreign_id) => {
            sqlx::query_as(&format!("{select} WHERE foreign_id = $1 FOR UPDATE"))
                .bind(foreign_id)
                .fetch_optional(tx)
                .await?
        }
        Target::New(_) => None,
    };
    Ok(row.map(|row| (row.id, row.fields)))
}

/// After a write committed: reconcile credentials, personal key, catalog;
/// then render the principal.
pub async fn respond(state: &AppState, saved: Saved, created_status: bool) -> ApiResult {
    effects::principal_committed(state, (&saved).into()).await;
    let body = data(payload::principal_json(&state.db, saved.id).await?);
    let status = if created_status {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok((status, body).into_response())
}

pub fn principal_id(oid: &str) -> ApiResult<i64> {
    decode_oid("prn", oid).ok_or_else(ApiError::not_found)
}

// --- handlers -------------------------------------------------------------------

#[derive(Default, Deserialize)]
pub struct ListQuery {
    q: Option<String>,
    /// `name` sorts by name; the default is oldest first.
    sort: Option<String>,
}

/// `GET /principals[?q=&sort=name&labels[k]=v&page=&limit=]`.
pub async fn index(
    State(state): State<AppState>,
    Query(filter): Query<ListQuery>,
    Query(page): Query<PageParams>,
    Query(pairs): Query<Vec<(String, String)>>,
) -> ApiResult {
    let by_name = match filter.sort.as_deref() {
        None | Some("") => false,
        Some("name") => true,
        Some(_) => return Err(ApiError::BadRequest("sort must be name".into())),
    };
    let page = page.resolve();
    let pattern = filter.q.as_deref().and_then(like_pattern);
    let labels = LabelFilter::from_pairs(pairs).as_json();
    let filters = |query: &mut QueryBuilder<Postgres>| {
        query.push(" WHERE TRUE");
        if let Some(pattern) = &pattern {
            query.push(" AND (name ILIKE ").push_bind(pattern.clone());
            query
                .push(" OR foreign_id ILIKE ")
                .push_bind(pattern.clone());
            query.push(")");
        }
        if let Some(labels) = &labels {
            query.push(" AND labels @> ").push_bind(labels.clone());
        }
    };
    let mut count = QueryBuilder::<Postgres>::new("SELECT COUNT(*) FROM principals");
    filters(&mut count);
    let total: i64 = count.build_query_scalar().fetch_one(&state.db).await?;
    let mut rows =
        QueryBuilder::<Postgres>::new(format!("SELECT {PRINCIPAL_COLUMNS} FROM principals"));
    filters(&mut rows);
    rows.push(if by_name {
        " ORDER BY name ASC NULLS LAST, created_at ASC, id ASC"
    } else {
        " ORDER BY created_at ASC, id ASC"
    });
    rows.push(" LIMIT ").push_bind(page.limit());
    rows.push(" OFFSET ").push_bind(page.offset());
    let rows: Vec<PrincipalRow> = rows.build_query_as().fetch_all(&state.db).await?;
    let data = payload::principals_json(&state.db, rows).await?;
    Ok(axum::Json(serde_json::json!({"data": data, "meta": page.meta(total)})).into_response())
}

pub async fn show(State(state): State<AppState>, Path(id): Path<String>) -> ApiResult {
    let id = principal_id(&id)?;
    let row = sqlx::query_as(&format!(
        "SELECT {PRINCIPAL_COLUMNS} FROM principals WHERE id = $1"
    ))
    .bind(id)
    .fetch_optional(&state.db)
    .await?;
    render(&state, row).await
}

pub async fn lookup(State(state): State<AppState>, Path(foreign_id): Path<String>) -> ApiResult {
    let row = sqlx::query_as(&format!(
        "SELECT {PRINCIPAL_COLUMNS} FROM principals WHERE foreign_id = $1"
    ))
    .bind(foreign_id)
    .fetch_optional(&state.db)
    .await?;
    render(&state, row).await
}

async fn render(state: &AppState, row: Option<PrincipalRow>) -> ApiResult {
    let row = row.ok_or_else(ApiError::not_found)?;
    let mut rendered = payload::principals_json(&state.db, vec![row]).await?;
    Ok(data(rendered.remove(0)).into_response())
}

/// `POST /principals`.
pub async fn create(
    State(state): State<AppState>,
    Actor(actor): Actor,
    Data(input): Data<PrincipalInput>,
) -> ApiResult {
    let saved = write(
        &state,
        actor,
        Target::New(input.foreign_id.as_deref()),
        &input,
    )
    .await?;
    respond(&state, saved, true).await
}

/// `PUT|PATCH /principals/{id}`: `id` is an oid (`prn_...`, must exist) or
/// a `foreign_id`, which is created when unknown.
pub async fn upsert(
    State(state): State<AppState>,
    Actor(actor): Actor,
    Path(id): Path<String>,
    Data(input): Data<PrincipalInput>,
) -> ApiResult {
    let target = if id.starts_with("prn_") {
        Target::Id(principal_id(&id)?)
    } else {
        Target::ForeignId(&id)
    };
    let saved = write(&state, actor, target, &input).await?;
    let created = saved.created;
    respond(&state, saved, created).await
}

/// `DELETE /principals/{id}`: the principal with everything that hangs off
/// it (the schema has no cascades for most of these).
pub async fn destroy(State(state): State<AppState>, Path(id): Path<String>) -> ApiResult {
    let id = principal_id(&id)?;
    let mut tx = state.db.begin().await?;
    let found: Option<i64> =
        sqlx::query_scalar("SELECT id FROM principals WHERE id = $1 FOR UPDATE")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?;
    if found.is_none() {
        return Err(ApiError::not_found());
    }
    for statement in [
        "DELETE FROM grants WHERE principal_id = $1",
        "UPDATE proxies SET principal_id = NULL WHERE principal_id = $1",
        "UPDATE proxies SET requester_principal_id = NULL WHERE requester_principal_id = $1",
        "DELETE FROM principal_roles WHERE principal_id = $1",
        "DELETE FROM slack_channel_permissions WHERE principal_id = $1",
        "DELETE FROM scheduled_tasks WHERE principal_id = $1",
        "DELETE FROM mcp_oauth_authorization_codes WHERE principal_id = $1",
        "DELETE FROM mcp_oauth_refresh_tokens WHERE principal_id = $1",
        "DELETE FROM principals WHERE id = $1",
    ] {
        sqlx::query(statement).bind(id).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}
