//! The sandbox's skill catalog (`/api/v1/sandbox/skills`): any sandbox can
//! read shared skills; one linked to an active console user also sees and
//! authors that user's skills.

use axum::{
    extract::State,
    response::{IntoResponse, Response},
};
use centaur_console_auth::sandbox::SandboxAuth;
use centaur_console_db::{decode_oid, oid};
use http::{HeaderValue, StatusCode, header};
use serde::Deserialize;
use serde_json::{Value, json};

use super::skills::{self, Input, List, Person, Scope, Skill, editors, find, find_by_name};
use crate::{
    ApiError, ApiResult, AppState, FieldErrors,
    auth::SandboxCaller,
    extract::{Data, Path, Query, data},
};

const MAX_LIMIT: i64 = 20;

fn no_store(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

/// The sandbox principal's console user, when active.
async fn linked_user(state: &AppState, auth: &SandboxAuth) -> ApiResult<Option<i64>> {
    let Some(user_id) = auth.console_user_id else {
        return Ok(None);
    };
    Ok(
        sqlx::query_scalar("SELECT id FROM users WHERE id = $1 AND status = 'active'")
            .bind(user_id)
            .fetch_optional(&state.db)
            .await?,
    )
}

/// Authoring needs a linked, active console user.
async fn authoring_user(state: &AppState, auth: &SandboxAuth) -> ApiResult<i64> {
    linked_user(state, auth).await?.ok_or_else(|| {
        ApiError::Forbidden("sandbox principal is not linked to an active Console user".into())
    })
}

fn catalog_json(skill: &Skill, include_document: bool, include_lock_version: bool) -> Value {
    let mut body = json!({
        "id": oid("skl", skill.id),
        "name": skill.name,
        "description": skill.description,
        "visibility": skill.visibility,
        "author": skill.author(),
        "updated_at": skill.updated_at,
        "checksum": skill.checksum(),
    });
    if include_document {
        body["document"] = json!(skill.document());
    }
    if include_lock_version {
        body["lock_version"] = json!(skill.lock_version);
    }
    body
}

#[derive(Deserialize)]
pub struct IndexQuery {
    scope: Option<String>,
    limit: Option<String>,
}

fn limit(raw: Option<&str>) -> ApiResult<i64> {
    match raw {
        None => Ok(10),
        Some(raw) => raw
            .trim()
            .parse::<i64>()
            .map(|value| value.clamp(1, MAX_LIMIT))
            .map_err(|_| ApiError::BadRequest("limit must be an integer".into())),
    }
}

pub async fn index(
    State(state): State<AppState>,
    SandboxCaller(auth): SandboxCaller,
    Query(query): Query<IndexQuery>,
) -> ApiResult {
    let user = linked_user(&state, &auth).await?;
    let limit = limit(query.limit.as_deref())?;
    let visibility = query
        .scope
        .as_deref()
        .filter(|scope| skills::VISIBILITIES.contains(scope));
    let found = skills::list(
        &state.db,
        &List {
            scope: Scope::Visible(user),
            visibility,
            search: None,
            limit: Some(limit),
        },
    )
    .await?;
    let body: Vec<Value> = found
        .iter()
        .map(|s| catalog_json(s, false, false))
        .collect();
    Ok(data(body).into_response())
}

#[derive(Deserialize)]
pub struct SearchQuery {
    q: Option<String>,
    limit: Option<String>,
}

pub async fn search(
    State(state): State<AppState>,
    SandboxCaller(auth): SandboxCaller,
    Query(query): Query<SearchQuery>,
) -> ApiResult {
    let text = query.q.as_deref().map(str::trim).unwrap_or_default();
    if text.is_empty() {
        return Err(ApiError::BadRequest("q must not be empty".into()));
    }
    let user = linked_user(&state, &auth).await?;
    let limit = limit(query.limit.as_deref())?;
    let found = skills::list(
        &state.db,
        &List {
            scope: Scope::Visible(user),
            visibility: None,
            search: Some(text),
            limit: Some(limit),
        },
    )
    .await?;
    let body: Vec<Value> = found
        .iter()
        .map(|s| catalog_json(s, false, false))
        .collect();
    Ok(data(body).into_response())
}

/// By oid when the id looks like one, otherwise by name.
async fn visible(state: &AppState, user: Option<i64>, id: &str) -> ApiResult<Skill> {
    let scope = Scope::Visible(user);
    if id.starts_with("skl_") {
        find(&state.db, scope, id).await
    } else {
        find_by_name(&state.db, scope, id).await
    }
}

pub async fn show(
    State(state): State<AppState>,
    SandboxCaller(auth): SandboxCaller,
    Path(id): Path<String>,
) -> ApiResult {
    let user = linked_user(&state, &auth).await?;
    let skill = visible(&state, user, &id).await?;
    Ok(no_store(
        data(catalog_json(&skill, true, false)).into_response(),
    ))
}

pub async fn create(
    State(state): State<AppState>,
    SandboxCaller(auth): SandboxCaller,
    Data(input): Data<Input>,
) -> ApiResult {
    let user = authoring_user(&state, &auth).await?;
    let id = skills::create(&state, user, &input, None).await?;
    let skill = find(&state.db, Scope::Owned(user), &oid("skl", id)).await?;
    Ok((StatusCode::CREATED, data(catalog_json(&skill, true, true))).into_response())
}

pub async fn update(
    State(state): State<AppState>,
    SandboxCaller(auth): SandboxCaller,
    Path(id): Path<String>,
    Data(input): Data<Input>,
) -> ApiResult {
    let user = authoring_user(&state, &auth).await?;
    let skill = find(&state.db, Scope::Editable(user), &id).await?;
    // Visibility changes use share/unshare; editors have their own routes.
    let input = Input {
        visibility: None,
        editor_ids: None,
        ..input
    };
    skills::update(&state, skill.id, &input, None).await?;
    let skill = find(&state.db, Scope::Editable(user), &id).await?;
    Ok(data(catalog_json(&skill, true, true)).into_response())
}

pub async fn destroy(
    State(state): State<AppState>,
    SandboxCaller(auth): SandboxCaller,
    Path(id): Path<String>,
) -> ApiResult {
    let user = authoring_user(&state, &auth).await?;
    let skill = find(&state.db, Scope::Owned(user), &id).await?;
    skills::archive(&state.db, skill.id, state.now()).await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

async fn change_visibility(
    state: AppState,
    auth: SandboxAuth,
    id: String,
    share: bool,
) -> ApiResult {
    let user = authoring_user(&state, &auth).await?;
    let skill = find(&state.db, Scope::Owned(user), &id).await?;
    skills::set_visibility(&state.db, skill.id, share, state.now()).await?;
    let skill = find(&state.db, Scope::Owned(user), &id).await?;
    Ok(data(catalog_json(&skill, true, true)).into_response())
}

pub async fn share(
    State(state): State<AppState>,
    SandboxCaller(auth): SandboxCaller,
    Path(id): Path<String>,
) -> ApiResult {
    change_visibility(state, auth, id, true).await
}

pub async fn unshare(
    State(state): State<AppState>,
    SandboxCaller(auth): SandboxCaller,
    Path(id): Path<String>,
) -> ApiResult {
    change_visibility(state, auth, id, false).await
}

// --- editors ----------------------------------------------------------------

async fn editors_json(state: &AppState, skill: &Skill) -> ApiResult<Response> {
    let list = editors(&state.db, skill.id, false).await?;
    let editors: Vec<Value> = list
        .iter()
        .map(|editor: &Person| {
            json!({
                "id": editor.oid(),
                "email": editor.email,
                "name": editor.name,
                "status": editor.status,
            })
        })
        .collect();
    Ok(data(json!({
        "id": oid("skl", skill.id),
        "editors": editors,
        "lock_version": skill.lock_version,
    }))
    .into_response())
}

pub async fn list_editors(
    State(state): State<AppState>,
    SandboxCaller(auth): SandboxCaller,
    Path(id): Path<String>,
) -> ApiResult {
    let user = linked_user(&state, &auth).await?;
    let skill = visible(&state, user, &id).await?;
    editors_json(&state, &skill).await
}

#[derive(Deserialize)]
pub struct EditorRef {
    user: String,
}

/// A user by oid (`usr_...`) or email, among `candidates` (SQL joined to
/// `users u`).
async fn resolve(state: &AppState, reference: &str, skill_id: Option<i64>) -> ApiResult<Person> {
    let reference = reference.trim();
    let by_id = reference
        .starts_with("usr_")
        .then(|| decode_oid("usr", reference).ok_or_else(ApiError::not_found))
        .transpose()?;
    let sql = match skill_id {
        None => {
            "SELECT u.id, u.email, u.name, u.status FROM users u WHERE u.status = 'active' \
             AND ($1::bigint IS NULL OR u.id = $1) AND ($1::bigint IS NOT NULL OR u.email = $2)"
        }
        Some(_) => {
            "SELECT u.id, u.email, u.name, u.status FROM users u \
             JOIN skill_editors e ON e.user_id = u.id AND e.skill_id = $3 \
             WHERE ($1::bigint IS NULL OR u.id = $1) AND ($1::bigint IS NOT NULL OR u.email = $2)"
        }
    };
    sqlx::query_as(sql)
        .bind(by_id)
        .bind(reference.to_lowercase())
        .bind(skill_id)
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(ApiError::not_found)
}

pub async fn add_editor(
    State(state): State<AppState>,
    SandboxCaller(auth): SandboxCaller,
    Path(id): Path<String>,
    Data(editor): Data<EditorRef>,
) -> ApiResult {
    let user = authoring_user(&state, &auth).await?;
    let skill = find(&state.db, Scope::Owned(user), &id).await?;
    let person = resolve(&state, &editor.user, None).await?;
    if person.id == skill.user_id {
        let mut errors = FieldErrors::new();
        errors.add("user", "is already the skill owner");
        return Err(ApiError::Invalid(errors));
    }
    let now = state.now();
    let mut tx = state.db.begin().await?;
    let added = sqlx::query(
        "INSERT INTO skill_editors (skill_id, user_id, created_at, updated_at) \
         VALUES ($1, $2, $3, $3) ON CONFLICT DO NOTHING",
    )
    .bind(skill.id)
    .bind(person.id)
    .bind(now.naive_utc())
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if added > 0 {
        skills::touch(&mut tx, skill.id, now).await?;
    }
    tx.commit().await?;
    let skill = find(&state.db, Scope::Owned(user), &id).await?;
    editors_json(&state, &skill).await
}

pub async fn remove_editor(
    State(state): State<AppState>,
    SandboxCaller(auth): SandboxCaller,
    Path(id): Path<String>,
    Data(editor): Data<EditorRef>,
) -> ApiResult {
    let user = authoring_user(&state, &auth).await?;
    let skill = find(&state.db, Scope::Owned(user), &id).await?;
    let person = resolve(&state, &editor.user, Some(skill.id)).await?;
    let now = state.now();
    let mut tx = state.db.begin().await?;
    sqlx::query("DELETE FROM skill_editors WHERE skill_id = $1 AND user_id = $2")
        .bind(skill.id)
        .bind(person.id)
        .execute(&mut *tx)
        .await?;
    skills::touch(&mut tx, skill.id, now).await?;
    tx.commit().await?;
    let skill = find(&state.db, Scope::Owned(user), &id).await?;
    editors_json(&state, &skill).await
}
