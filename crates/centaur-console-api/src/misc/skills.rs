//! Skills: the storage rules shared by the console authoring pages (any
//! active user) and the sandbox catalog (`sandbox_skills`), and the
//! console handlers.
//!
//! Every write locks the skill row, merges only the submitted fields,
//! validates the result, and updates only the columns that changed. A
//! submitted `lock_version` must match the locked row (409 otherwise).

use axum::{extract::State, response::IntoResponse};
use centaur_console_db::{decode_oid, oid};
use chrono::{DateTime, Utc};
use http::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{PgConnection, PgPool};

use crate::{
    ApiError, ApiResult, AppState, FieldErrors,
    auth::ConsoleCaller,
    extract::{Data, Path, Query, data},
    support::markdown,
};

pub const STALE_MESSAGE: &str =
    "This skill changed since you opened it. Review the latest content before saving.";
const MAX_DOCUMENT_BYTES: usize = 64 * 1024;
const RESERVED_NAMES: &[&str] = &["search"];
pub const VISIBILITIES: &[&str] = &["private", "shared"];

// --- rows ---------------------------------------------------------------

/// A skill with its owner.
#[derive(Clone, Debug, sqlx::FromRow)]
pub struct Skill {
    pub id: i64,
    pub name: String,
    pub description: String,
    pub content: String,
    pub visibility: String,
    pub lock_version: i32,
    pub user_id: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub owner_name: Option<String>,
    pub owner_email: String,
}

const SELECT: &str = "SELECT s.id, s.name, s.description, s.content, s.visibility, s.lock_version, \
    s.user_id, s.created_at AT TIME ZONE 'UTC' AS created_at, s.updated_at AT TIME ZONE 'UTC' AS updated_at, \
    u.name AS owner_name, u.email AS owner_email \
    FROM skills s JOIN users u ON u.id = s.user_id";

const IS_EDITOR: &str =
    "EXISTS (SELECT 1 FROM skill_editors e WHERE e.skill_id = s.id AND e.user_id = $1)";

/// Which skills a lookup may see. The user (if any) is bound as `$1`.
#[derive(Clone, Copy, Debug)]
pub enum Scope {
    /// Shared skills plus the user's own and edited ones.
    Visible(Option<i64>),
    /// Skills the user owns or edits.
    Editable(i64),
    /// Skills the user owns.
    Owned(i64),
    /// Shared skills, and the user's own.
    SharedOrOwned(i64),
    /// Shared skills only.
    Shared,
    /// Any skill that is not archived (to reload after a permitted write).
    Active,
}

impl Scope {
    fn user(self) -> Option<i64> {
        match self {
            Self::Visible(user) => user,
            Self::Editable(user) | Self::Owned(user) | Self::SharedOrOwned(user) => Some(user),
            Self::Shared | Self::Active => None,
        }
    }

    fn sql(self) -> String {
        match self {
            Self::Visible(_) => format!(
                "s.archived_at IS NULL AND (s.visibility = 'shared' OR s.user_id = $1 OR {IS_EDITOR})"
            ),
            Self::Editable(_) => {
                format!("s.archived_at IS NULL AND (s.user_id = $1 OR {IS_EDITOR})")
            }
            Self::Owned(_) => "s.archived_at IS NULL AND s.user_id = $1".to_owned(),
            Self::SharedOrOwned(_) => {
                "s.archived_at IS NULL AND (s.visibility = 'shared' OR s.user_id = $1)".to_owned()
            }
            Self::Shared => {
                "s.archived_at IS NULL AND s.visibility = 'shared' AND $1::bigint IS NULL"
                    .to_owned()
            }
            Self::Active => "s.archived_at IS NULL AND $1::bigint IS NULL".to_owned(),
        }
    }
}

pub async fn find(db: &PgPool, scope: Scope, id: &str) -> ApiResult<Skill> {
    let id = decode_oid("skl", id).ok_or_else(ApiError::not_found)?;
    sqlx::query_as(&format!("{SELECT} WHERE {} AND s.id = $2", scope.sql()))
        .bind(scope.user())
        .bind(id)
        .fetch_optional(db)
        .await?
        .ok_or_else(ApiError::not_found)
}

pub async fn find_by_name(db: &PgPool, scope: Scope, name: &str) -> ApiResult<Skill> {
    sqlx::query_as(&format!("{SELECT} WHERE {} AND s.name = $2", scope.sql()))
        .bind(scope.user())
        .bind(name)
        .fetch_optional(db)
        .await?
        .ok_or_else(ApiError::not_found)
}

pub struct List<'a> {
    pub scope: Scope,
    pub visibility: Option<&'a str>,
    /// Non-blank search text (any term may match).
    pub search: Option<&'a str>,
    pub limit: Option<i64>,
}

pub async fn list(db: &PgPool, list: &List<'_>) -> ApiResult<Vec<Skill>> {
    const QUERY: &str = "replace(plainto_tsquery('english', $3)::text, ' & ', ' | ')::tsquery";
    Ok(sqlx::query_as(&format!(
        "{SELECT} WHERE {scope} AND ($2::text IS NULL OR s.visibility = $2) \
         AND ($3::text IS NULL OR s.search_vector @@ {QUERY}) \
         ORDER BY CASE WHEN $3::text IS NOT NULL AND lower(s.name) = lower($3) THEN 0 ELSE 1 END, \
                  CASE WHEN $3::text IS NULL THEN 0 ELSE -ts_rank(s.search_vector, {QUERY}) END, \
                  s.updated_at DESC, s.id ASC LIMIT $4::bigint",
        scope = list.scope.sql()
    ))
    .bind(list.scope.user())
    .bind(list.visibility)
    .bind(list.search)
    .bind(list.limit)
    .fetch_all(db)
    .await?)
}

// --- the document ---------------------------------------------------------

#[derive(Serialize)]
struct Frontmatter<'a> {
    name: &'a str,
    description: &'a str,
}

impl Skill {
    /// The SKILL.md: YAML frontmatter (name, description) and the content.
    pub fn document(&self) -> String {
        document(&self.name, &self.description, &self.content)
    }

    pub fn checksum(&self) -> String {
        hex::encode(Sha256::digest(self.document().as_bytes()))
    }

    pub fn author(&self) -> String {
        author(self.owner_name.as_deref(), &self.owner_email)
    }

    pub fn is_shared(&self) -> bool {
        self.visibility == "shared"
    }
}

fn document(name: &str, description: &str, content: &str) -> String {
    let frontmatter = serde_yaml::to_string(&Frontmatter { name, description }).unwrap_or_default();
    format!("---\n{frontmatter}---\n\n{content}\n")
}

pub fn author(name: Option<&str>, email: &str) -> String {
    name.map(str::trim)
        .filter(|name| !name.is_empty())
        .unwrap_or(email)
        .to_owned()
}

// --- writes ---------------------------------------------------------------

/// The fields a create or update may set; `None` keeps the current value.
#[derive(Debug, Default, Deserialize)]
pub struct Input {
    pub name: Option<String>,
    pub description: Option<String>,
    pub instructions: Option<String>,
    pub visibility: Option<String>,
    pub lock_version: Option<i32>,
    pub editor_ids: Option<Vec<String>>,
}

fn clean_content(value: &str) -> String {
    value.replace("\r\n", "\n").trim().to_owned()
}

fn validate(
    name: &str,
    description: &str,
    content: &str,
    visibility: &str,
) -> Result<(), ApiError> {
    let mut errors = FieldErrors::new();
    if !VISIBILITIES.contains(&visibility) {
        errors.add("visibility", "is not included in the list");
    }
    if content.is_empty() {
        errors.add("instructions", "can't be blank");
    }
    if description.is_empty() {
        errors.add("description", "can't be blank");
    }
    if name.is_empty() {
        errors.add("name", "can't be blank");
    } else {
        if name.chars().count() > 64 {
            errors.add("name", "is too long (maximum is 64 characters)");
        }
        if !name_format_valid(name) {
            errors.add("name", "is invalid");
        }
        if RESERVED_NAMES.contains(&name) {
            errors.add("name", "is reserved");
        }
    }
    if document(name, description, content).len() > MAX_DOCUMENT_BYTES {
        errors.add("instructions", "makes the complete SKILL.md exceed 64 KiB");
    }
    errors.finish()
}

/// Lowercase words joined by single hyphens.
fn name_format_valid(name: &str) -> bool {
    !name.is_empty()
        && name.split('-').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        })
}

/// Creates a skill owned by `owner_id`; `visibility` defaults to shared.
pub async fn create(
    state: &AppState,
    owner_id: i64,
    input: &Input,
    editors: Option<&[i64]>,
) -> ApiResult<i64> {
    let name = input.name.as_deref().unwrap_or_default().trim();
    let description = input.description.as_deref().unwrap_or_default().trim();
    let content = clean_content(input.instructions.as_deref().unwrap_or_default());
    let visibility = input.visibility.as_deref().unwrap_or("shared");
    validate(name, description, &content, visibility)?;
    let now = state.now().naive_utc();
    let mut tx = state.db.begin().await?;
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO skills (name, description, content, visibility, shared_at, user_id, \
                             lock_version, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, CASE WHEN $4 = 'shared' THEN $5 END, $6, 0, $5, $5) RETURNING id",
    )
    .bind(name)
    .bind(description)
    .bind(&content)
    .bind(visibility)
    .bind(now)
    .bind(owner_id)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| ApiError::unique(e, "name", "is already used by another skill"))?;
    if let Some(editors) = editors {
        replace_editors(&mut tx, id, editors, now).await?;
    }
    tx.commit().await?;
    Ok(id)
}

/// Updates the submitted fields of a skill the caller may edit. Editors are
/// replaced only when `editors` is given.
pub async fn update(
    state: &AppState,
    id: i64,
    input: &Input,
    editors: Option<&[i64]>,
) -> ApiResult<()> {
    let now = state.now().naive_utc();
    let mut tx = state.db.begin().await?;
    let (name, description, content, visibility, lock_version): (
        String,
        String,
        String,
        String,
        i32,
    ) = sqlx::query_as(
        "SELECT name, description, content, visibility, lock_version FROM skills \
             WHERE id = $1 AND archived_at IS NULL FOR UPDATE",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(ApiError::not_found)?;
    if input
        .lock_version
        .is_some_and(|version| version != lock_version)
    {
        return Err(ApiError::Conflict(STALE_MESSAGE.into()));
    }
    let new_name = input.name.as_deref().map_or(name.as_str(), str::trim);
    let new_description = input
        .description
        .as_deref()
        .map_or(description.as_str(), str::trim);
    let new_content = input
        .instructions
        .as_deref()
        .map_or_else(|| content.clone(), clean_content);
    validate(new_name, new_description, &new_content, &visibility)?;
    let editors_changed = match editors {
        Some(wanted) => replace_editors(&mut tx, id, wanted, now).await?,
        None => false,
    };
    let changed = new_name != name || new_description != description || new_content != content;
    if changed || editors_changed {
        sqlx::query(
            "UPDATE skills SET name = $2, description = $3, content = $4, \
             updated_at = $5, lock_version = lock_version + 1 WHERE id = $1",
        )
        .bind(id)
        .bind(new_name)
        .bind(new_description)
        .bind(&new_content)
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(|e| ApiError::unique(e, "name", "is already used by another skill"))?;
    }
    tx.commit().await?;
    Ok(())
}

/// Sets the visibility of a skill (`shared` keeps an earlier `shared_at`).
pub async fn set_visibility(
    db: &PgPool,
    id: i64,
    shared: bool,
    now: DateTime<Utc>,
) -> ApiResult<()> {
    sqlx::query(
        "UPDATE skills SET visibility = $2, \
         shared_at = CASE WHEN $2 = 'shared' THEN COALESCE(shared_at, $3) END, \
         updated_at = $3, lock_version = lock_version + 1 WHERE id = $1",
    )
    .bind(id)
    .bind(if shared { "shared" } else { "private" })
    .bind(now.naive_utc())
    .execute(db)
    .await?;
    Ok(())
}

/// Archives a skill: private, and its name is free again.
pub async fn archive(db: &PgPool, id: i64, now: DateTime<Utc>) -> ApiResult<()> {
    sqlx::query(
        "UPDATE skills SET archived_at = $2, visibility = 'private', shared_at = NULL, \
         updated_at = $2, lock_version = lock_version + 1 WHERE id = $1",
    )
    .bind(id)
    .bind(now.naive_utc())
    .execute(db)
    .await?;
    Ok(())
}

/// Bumps `updated_at` and `lock_version` (an editor assignment changed).
pub async fn touch(conn: &mut PgConnection, id: i64, now: DateTime<Utc>) -> ApiResult<()> {
    sqlx::query("UPDATE skills SET updated_at = $2, lock_version = lock_version + 1 WHERE id = $1")
        .bind(id)
        .bind(now.naive_utc())
        .execute(conn)
        .await?;
    Ok(())
}

// --- editors --------------------------------------------------------------

#[derive(Clone, Debug, sqlx::FromRow)]
pub struct Person {
    pub id: i64,
    pub email: String,
    pub name: Option<String>,
    pub status: String,
}

impl Person {
    pub fn oid(&self) -> String {
        oid("usr", self.id)
    }
}

pub async fn editors(db: &PgPool, skill_id: i64, active_only: bool) -> ApiResult<Vec<Person>> {
    Ok(sqlx::query_as(
        "SELECT u.id, u.email, u.name, u.status FROM users u \
         JOIN skill_editors e ON e.user_id = u.id \
         WHERE e.skill_id = $1 AND (NOT $2 OR u.status = 'active') ORDER BY u.email ASC",
    )
    .bind(skill_id)
    .bind(active_only)
    .fetch_all(db)
    .await?)
}

/// Makes `wanted` the skill's editors; whether the set changed.
async fn replace_editors(
    tx: &mut PgConnection,
    skill_id: i64,
    wanted: &[i64],
    now: chrono::NaiveDateTime,
) -> ApiResult<bool> {
    let removed =
        sqlx::query("DELETE FROM skill_editors WHERE skill_id = $1 AND NOT (user_id = ANY($2))")
            .bind(skill_id)
            .bind(wanted)
            .execute(&mut *tx)
            .await?
            .rows_affected();
    let added = sqlx::query(
        "INSERT INTO skill_editors (skill_id, user_id, created_at, updated_at) \
         SELECT $1, id, $3, $3 FROM users WHERE id = ANY($2) ON CONFLICT DO NOTHING",
    )
    .bind(skill_id)
    .bind(wanted)
    .bind(now)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    Ok(removed + added > 0)
}

/// Resolves submitted editor oids to distinct active users other than the
/// owner; any other id is a 422 on `editor_ids`.
async fn resolve_editors(db: &PgPool, owner_id: i64, submitted: &[String]) -> ApiResult<Vec<i64>> {
    let mut ids: Vec<i64> = Vec::new();
    for oid in submitted
        .iter()
        .map(|oid| oid.trim())
        .filter(|oid| !oid.is_empty())
    {
        let id = decode_oid("usr", oid).ok_or_else(unavailable)?;
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    let active: Vec<i64> =
        sqlx::query_scalar("SELECT id FROM users WHERE status = 'active' AND id = ANY($1)")
            .bind(&ids)
            .fetch_all(db)
            .await?;
    if ids.iter().any(|id| *id == owner_id || !active.contains(id)) {
        return Err(unavailable());
    }
    Ok(ids)
}

fn unavailable() -> ApiError {
    ApiError::field("editor_ids", "include an unavailable user")
}

pub async fn is_editor(db: &PgPool, skill_id: i64, user_id: i64) -> ApiResult<bool> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM skill_editors WHERE skill_id = $1 AND user_id = $2)",
    )
    .bind(skill_id)
    .bind(user_id)
    .fetch_one(db)
    .await?)
}

// --- console ----------------------------------------------------------------

fn user_json(id: i64, name: Option<&str>, email: &str) -> Value {
    json!({
        "id": oid("usr", id),
        "name": name.map(str::trim).filter(|name| !name.is_empty()),
        "email": email,
    })
}

fn summary(skill: &Skill, caller_id: i64) -> Value {
    json!({
        "id": oid("skl", skill.id),
        "name": skill.name,
        "description": skill.description,
        "visibility": skill.visibility,
        "author": skill.author(),
        "owner": user_json(skill.user_id, skill.owner_name.as_deref(), &skill.owner_email),
        "updated_at": skill.updated_at,
        "viewer": { "owner": skill.user_id == caller_id },
    })
}

async fn detail(state: &AppState, skill: &Skill, caller_id: i64, admin: bool) -> ApiResult<Value> {
    let editors = editors(&state.db, skill.id, true).await?;
    let owner = skill.user_id == caller_id;
    let can_edit = owner || is_editor(&state.db, skill.id, caller_id).await?;
    let mut body = summary(skill, caller_id);
    body["viewer"] = json!({
        "owner": owner,
        "can_edit": can_edit,
        "can_manage": owner,
        "can_unshare": skill.is_shared() && (owner || admin),
    });
    body["instructions"] = json!(skill.content);
    body["content_html"] = json!(markdown::render(&skill.content));
    body["lock_version"] = json!(skill.lock_version);
    body["checksum"] = json!(skill.checksum());
    body["created_at"] = json!(skill.created_at);
    body["editors"] = editors
        .iter()
        .map(|editor| user_json(editor.id, editor.name.as_deref(), &editor.email))
        .collect();
    Ok(body)
}

#[derive(Deserialize)]
pub struct IndexQuery {
    tab: Option<String>,
    q: Option<String>,
}

pub async fn index(
    State(state): State<AppState>,
    ConsoleCaller(user): ConsoleCaller,
    Query(query): Query<IndexQuery>,
) -> ApiResult {
    let scope = if query.tab.as_deref() == Some("mine") {
        Scope::Editable(user.id)
    } else {
        Scope::Shared
    };
    let search = query.q.as_deref().map(str::trim).filter(|q| !q.is_empty());
    let skills = list(
        &state.db,
        &List {
            scope,
            visibility: None,
            search,
            limit: None,
        },
    )
    .await?;
    let body: Vec<Value> = skills.iter().map(|skill| summary(skill, user.id)).collect();
    Ok(data(body).into_response())
}

pub async fn show(
    State(state): State<AppState>,
    ConsoleCaller(user): ConsoleCaller,
    Path(id): Path<String>,
) -> ApiResult {
    let skill = find(&state.db, Scope::Visible(Some(user.id)), &id).await?;
    Ok(data(detail(&state, &skill, user.id, user.admin).await?).into_response())
}

pub async fn create_skill(
    State(state): State<AppState>,
    ConsoleCaller(user): ConsoleCaller,
    Data(input): Data<Input>,
) -> ApiResult {
    let editors = resolve_editors(
        &state.db,
        user.id,
        input.editor_ids.as_deref().unwrap_or(&[]),
    )
    .await?;
    let input = Input {
        visibility: None,
        ..input
    };
    let id = create(&state, user.id, &input, Some(&editors)).await?;
    let skill = find(&state.db, Scope::Owned(user.id), &oid("skl", id)).await?;
    let body = data(detail(&state, &skill, user.id, user.admin).await?);
    Ok((StatusCode::CREATED, body).into_response())
}

pub async fn update_skill(
    State(state): State<AppState>,
    ConsoleCaller(user): ConsoleCaller,
    Path(id): Path<String>,
    Data(input): Data<Input>,
) -> ApiResult {
    let skill = find(&state.db, Scope::Editable(user.id), &id).await?;
    // Only the owner changes the editors.
    let editors = match (&input.editor_ids, skill.user_id == user.id) {
        (Some(submitted), true) => Some(resolve_editors(&state.db, user.id, submitted).await?),
        _ => None,
    };
    let input = Input {
        visibility: None,
        ..input
    };
    update(&state, skill.id, &input, editors.as_deref()).await?;
    let skill = find(&state.db, Scope::Editable(user.id), &id).await?;
    Ok(data(detail(&state, &skill, user.id, user.admin).await?).into_response())
}

pub async fn destroy(
    State(state): State<AppState>,
    ConsoleCaller(user): ConsoleCaller,
    Path(id): Path<String>,
) -> ApiResult {
    let skill = find(&state.db, Scope::Owned(user.id), &id).await?;
    archive(&state.db, skill.id, state.now()).await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

async fn change_visibility(
    state: AppState,
    user: centaur_console_auth::AuthUser,
    id: String,
    share: bool,
) -> ApiResult {
    // An admin may also unshare other people's shared skills.
    let scope = if !share && user.admin {
        Scope::SharedOrOwned(user.id)
    } else {
        Scope::Owned(user.id)
    };
    let skill = find(&state.db, scope, &id).await?;
    set_visibility(&state.db, skill.id, share, state.now()).await?;
    let skill = find(&state.db, Scope::Active, &id).await?;
    Ok(data(detail(&state, &skill, user.id, user.admin).await?).into_response())
}

pub async fn share(
    State(state): State<AppState>,
    ConsoleCaller(user): ConsoleCaller,
    Path(id): Path<String>,
) -> ApiResult {
    change_visibility(state, user, id, true).await
}

pub async fn unshare(
    State(state): State<AppState>,
    ConsoleCaller(user): ConsoleCaller,
    Path(id): Path<String>,
) -> ApiResult {
    change_visibility(state, user, id, false).await
}

#[derive(Deserialize)]
pub struct CandidateQuery {
    q: Option<String>,
}

/// Active users the caller may add as editors.
pub async fn editor_candidates(
    State(state): State<AppState>,
    ConsoleCaller(user): ConsoleCaller,
    Query(query): Query<CandidateQuery>,
) -> ApiResult {
    let pattern = query
        .q
        .as_deref()
        .map(str::trim)
        .filter(|q| !q.is_empty())
        .map(|q| format!("%{}%", escape_like(&q.to_lowercase())));
    let users: Vec<Person> = sqlx::query_as(
        "SELECT id, email, name, status FROM users WHERE status = 'active' AND id <> $1 \
         AND ($2::text IS NULL OR lower(email) LIKE $2 OR lower(coalesce(name, '')) LIKE $2) \
         ORDER BY email ASC",
    )
    .bind(user.id)
    .bind(pattern)
    .fetch_all(&state.db)
    .await?;
    let body: Vec<Value> = users
        .iter()
        .map(|user| user_json(user.id, user.name.as_deref(), &user.email))
        .collect();
    Ok(data(body).into_response())
}

pub fn escape_like(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        if matches!(c, '\\' | '%' | '_') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_hyphenated_lowercase_words() {
        for ok in ["a", "release-notes", "v2-api"] {
            assert!(name_format_valid(ok), "{ok}");
        }
        for bad in ["", "-a", "a-", "a--b", "A", "a_b", "a b"] {
            assert!(!name_format_valid(bad), "{bad}");
        }
    }

    #[test]
    fn the_document_has_yaml_frontmatter() {
        let doc = document("deploy", "Ship: it # fast", "Steps");
        assert!(doc.starts_with("---\n"));
        assert!(doc.ends_with("---\n\nSteps\n"));
        let yaml = doc
            .trim_start_matches("---\n")
            .split("---\n")
            .next()
            .unwrap();
        let parsed: serde_yaml::Value = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(parsed["description"], "Ship: it # fast");
    }
}
