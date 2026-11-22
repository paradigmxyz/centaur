//! Console area: principals, roles, grants, role assignments, and Slack
//! channel permissions. See `../README.md` for conventions.
//!
//! Responses are typed payloads from `centaur_console_db::areas::principals`,
//! lists go through [`crate::console::http::list_rows`]/[`list_response`], and
//! lookups and 404s use the shared model descriptor through the area's view
//! finders.

use std::collections::{BTreeMap, HashMap};

use axum::{
    Json, Router,
    extract::{Path, Query, RawQuery, State},
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{delete, get, post, put},
};
use centaur_console_db::{
    ConsoleUser,
    areas::principals::{
        AssignRole, ConsolePrincipalPayload, EffectiveGrantPayload, GRANT_COLUMNS, GrantPayload,
        GrantWrite, PRINCIPAL_COLUMNS, PermissionOwner, PermissionSetWrite, PermissionWrite,
        PrincipalPayload, PrincipalRow, PrincipalView, PrincipalWrite, ROLE_COLUMNS, RolePayload,
        RoleRow, RoleView, RoleWrite, SlackChannelOptionPayload, assign_role,
        console_grant_payload, console_principal_payload,
        console_show_grant as console_grant_by_oid, create_grant, create_principal, create_role,
        delete_principal, delete_role, destroy_grant, destroy_slack_channel_permission,
        effective_grants, find_principal_by_foreign_id, find_principal_by_oid,
        find_role_by_foreign_id, find_role_by_oid, grant_payload, like_pattern,
        load_principal_view, load_role_view, permission_channel_ids, principal_payload,
        principal_roles_payload, replace_slack_channel_permissions, role_payload, show_grant,
        slack_catalog_error, slack_channel_option, slack_channels, upsert_principal, upsert_role,
        upsert_slack_channel_permission,
    },
    effective_config,
    model::{self, ConsoleModel},
    validation::plain_message,
};
use garde::Validate;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::{
    AppState,
    console::{
        bearer::{BearerRoutes, BearerUser},
        http::{
            ConsoleError, DataPayload, ListParams, ListSource, Pagination, bind, created_data,
            json_data, labels_value, list_response, list_rows, no_content,
        },
        request::{ConsoleData, ConsoleQuery},
    },
    console_session::ConsoleRoutes,
};

const PRINCIPAL_LIST_FROM: &str = "FROM principals \
     WHERE ($1::jsonb IS NULL OR labels @> $1::jsonb) \
       AND ($2::text IS NULL OR principals.name ILIKE $2 OR principals.foreign_id ILIKE $2)";
const PRINCIPAL_LIST_ORDER_CREATED: &str = "ORDER BY principals.created_at ASC, principals.id ASC";
const PRINCIPAL_LIST_ORDER_NAME: &str =
    "ORDER BY principals.name ASC NULLS LAST, principals.created_at ASC, principals.id ASC";

const ROLE_LIST_FROM: &str = "FROM roles \
     WHERE ($1::bigint IS NULL OR roles.id NOT IN \
            (SELECT role_id FROM principal_roles WHERE principal_id = $1)) \
       AND ($2::jsonb IS NULL OR labels @> $2::jsonb) \
       AND ($3::text IS NULL OR roles.name ILIKE $3 OR roles.foreign_id ILIKE $3)";
const ROLE_LIST_ORDER: &str = "ORDER BY created_at ASC, id ASC";

const GRANT_LIST_ORDER: &str = "ORDER BY created_at ASC, id ASC";

/// The endpoint-specific scalar query parameters of the console principal list.
/// Pagination and `labels[key]` filters stay on `ListParams`.
#[derive(Debug, Default, Deserialize, garde::Validate)]
struct PrincipalListQuery {
    #[serde(default)]
    #[garde(custom(sort_order))]
    sort: Option<String>,
    /// A free-text search filter, not a rule of the record.
    #[serde(default)]
    #[garde(skip)]
    q: Option<String>,
}

/// `sort` is a closed set. Rails answers an unknown order with
/// `ActionController::BadRequest`, so this gate is a plain 400, not the 422 field
/// envelope.
fn sort_order(value: &Option<String>, _: &()) -> garde::Result {
    match value.as_deref().map(str::trim) {
        // A blank `sort` is no sort at all, as `params[:sort].present?` is.
        Some(sort) if !sort.is_empty() && sort != "name" => {
            Err(garde::Error::new("sort must be name"))
        }
        _ => Ok(()),
    }
}

/// The endpoint-specific scalar query parameters of the console role list.
#[derive(Debug, Default, Deserialize)]
struct RoleListQuery {
    #[serde(default)]
    exclude_principal: Option<String>,
    #[serde(default)]
    q: Option<String>,
}

/// The scalar query parameters of the Slack channel picker. `ids[]` is a Rack
/// array parameter, so it stays on the raw query string.
#[derive(Debug, Default, Deserialize, garde::Validate)]
struct SlackChannelsQuery {
    /// A free-text search filter, not a rule of the record.
    #[serde(default)]
    #[garde(skip)]
    q: Option<String>,
    #[serde(default)]
    #[garde(custom(owner_selector))]
    exclude_owner: Option<String>,
}

/// `exclude_owner`'s shape: `principal:<oid>` or `role:<oid>`. Rails raises
/// `ActionController::BadRequest` for anything else, so this gate is a plain 400;
/// whether the oid resolves is a database question the handler still answers.
fn owner_selector(value: &Option<String>, _: &()) -> garde::Result {
    let Some(selector) = value.as_deref().map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(());
    };
    match selector.split_once(':') {
        Some(("principal" | "role", _)) => Ok(()),
        _ => Err(garde::Error::new(
            "exclude_owner must be principal:<oid> or role:<oid>",
        )),
    }
}

/// The Slack channel picker's body, whose `meta` carries the catalog state.
#[derive(Serialize)]
struct SlackChannelsPayload {
    data: Vec<SlackChannelOptionPayload>,
    meta: SlackCatalogMeta,
}

#[derive(Serialize)]
struct SlackCatalogMeta {
    catalog_error: Option<&'static str>,
}

#[derive(Serialize)]
struct EffectiveConfigPayload {
    id: String,
    secrets: Vec<Value>,
    transforms: Vec<Value>,
    postgres: Vec<Value>,
}

#[derive(Serialize)]
struct EffectiveGrantsPayload {
    data: Vec<EffectiveGrantPayload>,
    meta: EffectiveGrantsMeta,
}

/// The effective-grants page meta: `page` is the clamped page the rows came
/// from, and an empty table still reports one page.
#[derive(Serialize)]
struct EffectiveGrantsMeta {
    page: i64,
    limit: i64,
    total: i64,
    total_pages: i64,
}

#[derive(Clone, Copy)]
enum SubjectKind {
    Principal,
    Role,
}

#[derive(Clone, Copy)]
enum OwnerKind {
    Principal,
    Role,
}

/// Routes under `/api/v1/console` (paths relative to that prefix).
pub(crate) fn console(routes: ConsoleRoutes) -> ConsoleRoutes {
    routes
        .admin(
            "/principals",
            get(console_list_principals).post(console_create_principal),
        )
        .admin(
            "/principals/{id}",
            get(console_show_principal)
                .patch(console_update_principal)
                .delete(console_delete_principal),
        )
        .admin(
            "/principals/{id}/roles",
            get(console_principal_roles).post(console_assign_role),
        )
        .admin(
            "/principals/{principal_id}/roles/{id}",
            delete(console_remove_role),
        )
        .admin("/principals/{id}/grants", get(console_principal_grants))
        .admin(
            "/principals/{id}/effective_grants",
            get(console_effective_grants),
        )
        .admin(
            "/principals/{id}/slack_channel_permissions",
            put(console_replace_principal_permissions),
        )
        .admin(
            "/principals/{id}/slack_channel_permissions/{channel_id}",
            delete(console_destroy_principal_permission),
        )
        .admin("/roles", get(console_list_roles).post(console_create_role))
        .admin(
            "/roles/{id}",
            get(console_show_role)
                .patch(console_update_role)
                .delete(console_delete_role),
        )
        .admin("/roles/{role_id}/grants", get(console_role_grants))
        .admin(
            "/roles/{id}/slack_channel_permissions",
            put(console_replace_role_permissions),
        )
        .admin(
            "/roles/{id}/slack_channel_permissions/{channel_id}",
            delete(console_destroy_role_permission),
        )
        .admin("/grants", post(console_create_grant))
        .admin(
            "/grants/{id}",
            get(console_show_grant).delete(console_destroy_grant),
        )
        .admin("/slack_channels", get(console_slack_channels))
}

/// Routes under `/api/v1` (bearer API; paths relative to that prefix).
pub(crate) fn bearer(routes: BearerRoutes) -> BearerRoutes {
    routes
        .admin(
            "/principals",
            get(bearer_list_principals).post(bearer_create_principal),
        )
        .admin(
            "/principals/lookup/{foreign_id}",
            get(bearer_lookup_principal),
        )
        .admin(
            "/principals/lookup/{foreign_id}/effective_config",
            get(bearer_lookup_effective_config),
        )
        .admin(
            "/principals/{id}",
            get(bearer_show_principal).put(bearer_update_principal),
        )
        .admin(
            "/principals/{id}/effective_config",
            get(bearer_effective_config),
        )
        .admin(
            "/principals/{id}/slack_channel_permissions",
            post(bearer_upsert_principal_permission),
        )
        .admin(
            "/principals/{principal_id}/roles",
            get(bearer_principal_roles).post(bearer_assign_role),
        )
        .admin(
            "/principals/{principal_id}/roles/{id}",
            delete(bearer_remove_role),
        )
        .admin(
            "/principals/{principal_id}/grants",
            get(bearer_principal_grants),
        )
        .admin("/roles", get(bearer_list_roles).post(bearer_create_role))
        .admin("/roles/lookup/{foreign_id}", get(bearer_lookup_role))
        .admin(
            "/roles/{id}",
            get(bearer_show_role)
                .put(bearer_update_role)
                .delete(bearer_delete_role),
        )
        .admin(
            "/roles/{id}/slack_channel_permissions",
            post(bearer_upsert_role_permission),
        )
        .admin("/roles/{role_id}/grants", get(bearer_role_grants))
        .admin("/grants", post(bearer_create_grant))
        .admin(
            "/grants/{id}",
            get(bearer_show_grant).delete(bearer_destroy_grant),
        )
}

/// Server-rendered pages and other top-level routes (absolute paths).
pub(crate) fn pages(router: Router<AppState>) -> Router<AppState> {
    router
}

// --- shared plumbing -------------------------------------------------------

fn pool(state: &AppState) -> Result<sqlx::PgPool, ConsoleError> {
    state.console_session_db().map_err(|_| {
        ConsoleError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "the console database is not configured",
        )
    })
}

async fn principal_view(pool: &sqlx::PgPool, id: &str) -> Result<PrincipalView, ConsoleError> {
    Ok(find_principal_by_oid(pool, id).await?)
}

async fn role_view(pool: &sqlx::PgPool, id: &str) -> Result<RoleView, ConsoleError> {
    Ok(find_role_by_oid(pool, id).await?)
}

/// The `labels @> $1` binding, absent when the query carried no label filter.
fn label_binding(params: &ListParams) -> Option<String> {
    (!params.labels.is_empty()).then(|| Value::Object(labels_value(&params.labels)).to_string())
}

fn principal_list_source(order_by_name: bool) -> ListSource {
    ListSource::owned(
        format!("SELECT {PRINCIPAL_COLUMNS}"),
        PRINCIPAL_LIST_FROM,
        if order_by_name {
            PRINCIPAL_LIST_ORDER_NAME
        } else {
            PRINCIPAL_LIST_ORDER_CREATED
        },
        2,
    )
}

fn role_list_source() -> ListSource {
    ListSource::owned(
        format!("SELECT {ROLE_COLUMNS}"),
        ROLE_LIST_FROM,
        ROLE_LIST_ORDER,
        3,
    )
}

fn grant_list_source(grantee: SubjectKind) -> ListSource {
    let from_where = match grantee {
        SubjectKind::Principal => "FROM grants WHERE principal_id = $1",
        SubjectKind::Role => "FROM grants WHERE role_id = $1",
    };
    ListSource::owned(
        format!("SELECT {GRANT_COLUMNS}"),
        from_where,
        GRANT_LIST_ORDER,
        1,
    )
}

async fn principal_views(
    pool: &sqlx::PgPool,
    rows: &[PrincipalRow],
) -> Result<Vec<PrincipalView>, ConsoleError> {
    let mut views = Vec::with_capacity(rows.len());
    for row in rows {
        if let Some(view) = load_principal_view(pool, row.id).await? {
            views.push(view);
        }
    }
    Ok(views)
}

async fn role_views(pool: &sqlx::PgPool, rows: &[RoleRow]) -> Result<Vec<RoleView>, ConsoleError> {
    let mut views = Vec::with_capacity(rows.len());
    for row in rows {
        if let Some(view) = load_role_view(pool, row.id).await? {
            views.push(view);
        }
    }
    Ok(views)
}

// --- console principals ----------------------------------------------------

async fn console_list_principals(
    State(state): State<AppState>,
    Query(raw): Query<HashMap<String, String>>,
    ConsoleQuery(query): ConsoleQuery<PrincipalListQuery>,
) -> Result<Response, ConsoleError> {
    let pool = pool(&state)?;
    let params = ListParams::from_query(&raw)?;
    if let Err(report) = query.validate() {
        let message = plain_message(&report).expect("a failed sort gate carries a message");
        return Err(ConsoleError::bad_request(message));
    }
    let order_by_name = query.sort.as_deref().map(str::trim) == Some("name");
    let labels = label_binding(&params);
    let q = query
        .q
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(like_pattern);
    let (rows, total) = list_rows::<PrincipalRow, _>(
        &pool,
        &params,
        principal_list_source(order_by_name),
        |arguments| {
            bind(arguments, labels.clone())?;
            bind(arguments, q.clone())
        },
    )
    .await?;
    let views = principal_views(&pool, &rows).await?;
    let data: Vec<ConsolePrincipalPayload> = views.iter().map(console_principal_payload).collect();
    Ok(Json(list_response(params.pagination, &data, total)).into_response())
}

async fn console_show_principal(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Response, ConsoleError> {
    let pool = pool(&state)?;
    let view = principal_view(&pool, &id).await?;
    Ok(json_data(console_principal_payload(&view)))
}

async fn console_create_principal(
    State(state): State<AppState>,
    user: ConsoleUser,
    ConsoleData(write): ConsoleData<PrincipalWrite>,
) -> Result<Response, ConsoleError> {
    let pool = pool(&state)?;
    let view = create_principal(&pool, &write, user.id).await?;
    Ok(created_data(principal_payload(&view)))
}

async fn console_update_principal(
    State(state): State<AppState>,
    Path(id): Path<String>,
    user: ConsoleUser,
    ConsoleData(write): ConsoleData<PrincipalWrite>,
) -> Result<Response, ConsoleError> {
    let pool = pool(&state)?;
    let (view, was_new) = upsert_principal(&pool, &id, &write, user.id).await?;
    let payload = principal_payload(&view);
    Ok(if was_new {
        created_data(payload)
    } else {
        json_data(payload)
    })
}

async fn console_delete_principal(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Response, ConsoleError> {
    let pool = pool(&state)?;
    let view = principal_view(&pool, &id).await?;
    delete_principal(&pool, &view).await?;
    Ok(no_content())
}

async fn console_principal_roles(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Response, ConsoleError> {
    let pool = pool(&state)?;
    let view = principal_view(&pool, &id).await?;
    let payload = principal_roles_payload(&pool, view.row.id).await?;
    Ok(json_data(payload))
}

async fn console_assign_role(
    State(state): State<AppState>,
    Path(principal_id): Path<String>,
    ConsoleData(write): ConsoleData<AssignRole>,
) -> Result<Response, ConsoleError> {
    let pool = pool(&state)?;
    let principal = principal_view(&pool, &principal_id).await?;
    let (role, inserted) = assign_role(&pool, principal.row.id, &write.role_id).await?;
    let payload = role_payload(&role);
    Ok(if inserted {
        created_data(payload)
    } else {
        json_data(payload)
    })
}

async fn console_remove_role(
    State(state): State<AppState>,
    Path((principal_id, id)): Path<(String, String)>,
) -> Result<Response, ConsoleError> {
    remove_role_response(&state, &principal_id, &id).await
}

async fn remove_role_response(
    state: &AppState,
    principal_id: &str,
    role_oid: &str,
) -> Result<Response, ConsoleError> {
    let pool = pool(state)?;
    let principal = principal_view(&pool, principal_id).await?;
    // Resolve the role first so an unknown role is a 404, like Rails.
    role_view(&pool, role_oid).await?;
    centaur_console_db::areas::principals::remove_role(&pool, principal.row.id, role_oid).await?;
    Ok(no_content())
}

async fn console_principal_grants(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(raw): Query<HashMap<String, String>>,
) -> Result<Response, ConsoleError> {
    grantee_grants(&state, SubjectKind::Principal, &id, &raw, true).await
}

async fn console_effective_grants(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(raw): Query<HashMap<String, String>>,
) -> Result<Response, ConsoleError> {
    effective_grants_response(&state, &id, &raw).await
}

async fn console_replace_principal_permissions(
    State(state): State<AppState>,
    Path(id): Path<String>,
    ConsoleData(write): ConsoleData<PermissionSetWrite>,
) -> Result<Response, ConsoleError> {
    replace_permissions_response(&state, OwnerKind::Principal, &id, &write).await
}

async fn console_destroy_principal_permission(
    State(state): State<AppState>,
    Path((id, channel_id)): Path<(String, String)>,
) -> Result<Response, ConsoleError> {
    destroy_permission_response(&state, OwnerKind::Principal, &id, &channel_id).await
}

// --- console roles ---------------------------------------------------------

async fn console_list_roles(
    State(state): State<AppState>,
    Query(raw): Query<HashMap<String, String>>,
    ConsoleQuery(query): ConsoleQuery<RoleListQuery>,
) -> Result<Response, ConsoleError> {
    role_list_response(&state, &raw, &query).await
}

async fn console_show_role(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Response, ConsoleError> {
    role_show_response(&state, &id).await
}

async fn console_create_role(
    State(state): State<AppState>,
    user: ConsoleUser,
    ConsoleData(write): ConsoleData<RoleWrite>,
) -> Result<Response, ConsoleError> {
    let pool = pool(&state)?;
    let view = create_role(&pool, &write, user.id).await?;
    Ok(created_data(role_payload(&view)))
}

async fn console_update_role(
    State(state): State<AppState>,
    Path(id): Path<String>,
    user: ConsoleUser,
    ConsoleData(write): ConsoleData<RoleWrite>,
) -> Result<Response, ConsoleError> {
    let pool = pool(&state)?;
    let (view, was_new) = upsert_role(&pool, &id, &write, user.id).await?;
    let payload = role_payload(&view);
    Ok(if was_new {
        created_data(payload)
    } else {
        json_data(payload)
    })
}

async fn console_delete_role(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Response, ConsoleError> {
    let pool = pool(&state)?;
    let view = role_view(&pool, &id).await?;
    delete_role(&pool, &view).await?;
    Ok(no_content())
}

async fn console_role_grants(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(raw): Query<HashMap<String, String>>,
) -> Result<Response, ConsoleError> {
    grantee_grants(&state, SubjectKind::Role, &id, &raw, true).await
}

async fn console_replace_role_permissions(
    State(state): State<AppState>,
    Path(id): Path<String>,
    ConsoleData(write): ConsoleData<PermissionSetWrite>,
) -> Result<Response, ConsoleError> {
    replace_permissions_response(&state, OwnerKind::Role, &id, &write).await
}

async fn console_destroy_role_permission(
    State(state): State<AppState>,
    Path((id, channel_id)): Path<(String, String)>,
) -> Result<Response, ConsoleError> {
    destroy_permission_response(&state, OwnerKind::Role, &id, &channel_id).await
}

// --- console grants and Slack catalog -------------------------------------

async fn console_create_grant(
    State(state): State<AppState>,
    user: ConsoleUser,
    ConsoleData(write): ConsoleData<GrantWrite>,
) -> Result<Response, ConsoleError> {
    let pool = pool(&state)?;
    let (row, inserted) = create_grant(&pool, &write, user.id).await?;
    let payload = console_grant_payload(&pool, &row).await?;
    Ok(if inserted {
        created_data(payload)
    } else {
        json_data(payload)
    })
}

async fn console_show_grant(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Response, ConsoleError> {
    let pool = pool(&state)?;
    let payload = console_grant_by_oid(&pool, &id).await?;
    Ok(json_data(payload))
}

async fn console_destroy_grant(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Response, ConsoleError> {
    let pool = pool(&state)?;
    destroy_grant(&pool, &id).await?;
    Ok(no_content())
}

async fn console_slack_channels(
    State(state): State<AppState>,
    ConsoleQuery(query): ConsoleQuery<SlackChannelsQuery>,
    RawQuery(raw): RawQuery,
) -> Result<Response, ConsoleError> {
    let pool = pool(&state)?;
    if let Err(report) = query.validate() {
        let message = plain_message(&report).expect("a failed owner gate carries a message");
        return Err(ConsoleError::bad_request(message));
    }
    // `ids[]` is Rack's array form, which a flat typed struct cannot express.
    let ids: Vec<String> =
        url::form_urlencoded::parse(raw.as_deref().unwrap_or_default().as_bytes())
            .filter(|(key, _)| key == "ids[]")
            .map(|(_, value)| value.into_owned())
            .collect();
    let owner = match query
        .exclude_owner
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        None => None,
        Some(value) => Some(parse_permission_owner(&pool, value).await?),
    };
    let mut exclude = Vec::new();
    if let Some(owner) = owner {
        exclude = match owner {
            PermissionOwner::Principal(id) => permission_channel_ids(&pool, Some(id), None).await?,
            PermissionOwner::Role(id) => permission_channel_ids(&pool, None, Some(id)).await?,
        };
    }
    let rows = slack_channels(&pool, &ids, query.q.as_deref(), &exclude).await?;
    let catalog_error = slack_catalog_error(&pool).await?;
    let payload = SlackChannelsPayload {
        data: rows.iter().map(slack_channel_option).collect(),
        meta: SlackCatalogMeta { catalog_error },
    };
    let mut response = Json(payload).into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

/// The selector's shape is a query gate ([`owner_selector`]), so this only
/// resolves the reference it names.
async fn parse_permission_owner(
    pool: &sqlx::PgPool,
    raw: &str,
) -> Result<PermissionOwner, ConsoleError> {
    let (kind, oid) = raw
        .split_once(':')
        .expect("a validated exclude_owner carries a kind and an oid");
    if kind == "role" {
        let view = find_role_by_oid(pool, oid).await?;
        return Ok(PermissionOwner::Role(view.row.id));
    }
    let view = find_principal_by_oid(pool, oid).await?;
    Ok(PermissionOwner::Principal(view.row.id))
}

// --- bearer principals and roles ------------------------------------------

async fn bearer_list_principals(
    State(state): State<AppState>,
    Query(raw): Query<HashMap<String, String>>,
) -> Result<Response, ConsoleError> {
    principal_list_response(&state, &raw, false).await
}

async fn bearer_create_principal(
    State(state): State<AppState>,
    user: BearerUser,
    ConsoleData(write): ConsoleData<PrincipalWrite>,
) -> Result<Response, ConsoleError> {
    let pool = pool(&state)?;
    let view = create_principal(&pool, &write, user.0.id).await?;
    Ok(created_data(principal_payload(&view)))
}

async fn bearer_show_principal(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Response, ConsoleError> {
    principal_show_response(&state, &id).await
}

async fn bearer_lookup_principal(
    State(state): State<AppState>,
    Path(foreign_id): Path<String>,
) -> Result<Response, ConsoleError> {
    let pool = pool(&state)?;
    let view = find_principal_by_foreign_id(&pool, &foreign_id).await?;
    Ok(json_data(principal_payload(&view)))
}

async fn bearer_update_principal(
    State(state): State<AppState>,
    Path(id): Path<String>,
    user: BearerUser,
    ConsoleData(write): ConsoleData<PrincipalWrite>,
) -> Result<Response, ConsoleError> {
    let pool = pool(&state)?;
    let (view, was_new) = upsert_principal(&pool, &id, &write, user.0.id).await?;
    let payload = principal_payload(&view);
    Ok(if was_new {
        created_data(payload)
    } else {
        json_data(payload)
    })
}

async fn bearer_effective_config(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Response, ConsoleError> {
    effective_config_response(&state, EffectiveConfigRef::Oid(&id)).await
}

async fn bearer_lookup_effective_config(
    State(state): State<AppState>,
    Path(foreign_id): Path<String>,
) -> Result<Response, ConsoleError> {
    effective_config_response(&state, EffectiveConfigRef::ForeignId(&foreign_id)).await
}

enum EffectiveConfigRef<'a> {
    Oid(&'a str),
    ForeignId(&'a str),
}

async fn effective_config_response(
    state: &AppState,
    reference: EffectiveConfigRef<'_>,
) -> Result<Response, ConsoleError> {
    let pool = pool(state)?;
    let view = match reference {
        EffectiveConfigRef::Oid(id) => principal_view(&pool, id).await?,
        EffectiveConfigRef::ForeignId(foreign_id) => {
            find_principal_by_foreign_id(&pool, foreign_id).await?
        }
    };
    let config = effective_config(&pool, view.row.id)
        .await?
        .ok_or_else(|| ConsoleError::not_found("principal not found"))?;
    let payload = DataPayload {
        data: EffectiveConfigPayload {
            id: model::Principal::oid(view.row.id),
            secrets: config.secrets,
            transforms: config.transforms,
            postgres: config.postgres,
        },
    };
    let body = serde_json::to_string(&payload).map_err(ConsoleError::internal)?;
    let etag = format!("\"{}\"", hex::encode(Sha256::digest(body.as_bytes())));
    let mut response = body.into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json; charset=utf-8"),
    );
    response.headers_mut().insert(
        header::ETAG,
        HeaderValue::from_str(&etag).map_err(ConsoleError::internal)?,
    );
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

async fn bearer_upsert_principal_permission(
    State(state): State<AppState>,
    Path(id): Path<String>,
    ConsoleData(write): ConsoleData<PermissionWrite>,
) -> Result<Response, ConsoleError> {
    upsert_permission_response(&state, OwnerKind::Principal, &id, &write).await
}

async fn bearer_principal_roles(
    State(state): State<AppState>,
    Path(principal_id): Path<String>,
) -> Result<Response, ConsoleError> {
    principal_roles_response(&state, &principal_id).await
}

async fn bearer_assign_role(
    State(state): State<AppState>,
    Path(principal_id): Path<String>,
    ConsoleData(write): ConsoleData<AssignRole>,
) -> Result<Response, ConsoleError> {
    assign_role_response(&state, &principal_id, &write).await
}

async fn bearer_remove_role(
    State(state): State<AppState>,
    Path((principal_id, id)): Path<(String, String)>,
) -> Result<Response, ConsoleError> {
    remove_role_response(&state, &principal_id, &id).await
}

async fn bearer_principal_grants(
    State(state): State<AppState>,
    Path(principal_id): Path<String>,
    Query(raw): Query<HashMap<String, String>>,
) -> Result<Response, ConsoleError> {
    grantee_grants(&state, SubjectKind::Principal, &principal_id, &raw, false).await
}

async fn bearer_list_roles(
    State(state): State<AppState>,
    Query(raw): Query<HashMap<String, String>>,
) -> Result<Response, ConsoleError> {
    bearer_role_list_response(&state, &raw).await
}

async fn bearer_create_role(
    State(state): State<AppState>,
    user: BearerUser,
    ConsoleData(write): ConsoleData<RoleWrite>,
) -> Result<Response, ConsoleError> {
    let pool = pool(&state)?;
    let view = create_role(&pool, &write, user.0.id).await?;
    Ok(created_data(role_payload(&view)))
}

async fn bearer_show_role(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Response, ConsoleError> {
    role_show_response(&state, &id).await
}

async fn bearer_lookup_role(
    State(state): State<AppState>,
    Path(foreign_id): Path<String>,
) -> Result<Response, ConsoleError> {
    let pool = pool(&state)?;
    let view = find_role_by_foreign_id(&pool, &foreign_id).await?;
    Ok(json_data(role_payload(&view)))
}

async fn bearer_update_role(
    State(state): State<AppState>,
    Path(id): Path<String>,
    user: BearerUser,
    ConsoleData(write): ConsoleData<RoleWrite>,
) -> Result<Response, ConsoleError> {
    let pool = pool(&state)?;
    let (view, was_new) = upsert_role(&pool, &id, &write, user.0.id).await?;
    let payload = role_payload(&view);
    Ok(if was_new {
        created_data(payload)
    } else {
        json_data(payload)
    })
}

async fn bearer_delete_role(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Response, ConsoleError> {
    role_delete_response(&state, &id).await
}

async fn bearer_upsert_role_permission(
    State(state): State<AppState>,
    Path(id): Path<String>,
    ConsoleData(write): ConsoleData<PermissionWrite>,
) -> Result<Response, ConsoleError> {
    upsert_permission_response(&state, OwnerKind::Role, &id, &write).await
}

async fn bearer_role_grants(
    State(state): State<AppState>,
    Path(role_id): Path<String>,
    Query(raw): Query<HashMap<String, String>>,
) -> Result<Response, ConsoleError> {
    grantee_grants(&state, SubjectKind::Role, &role_id, &raw, false).await
}

async fn bearer_create_grant(
    State(state): State<AppState>,
    user: BearerUser,
    ConsoleData(write): ConsoleData<GrantWrite>,
) -> Result<Response, ConsoleError> {
    create_grant_response(&state, user.0.id, &write).await
}

async fn bearer_show_grant(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Response, ConsoleError> {
    show_grant_response(&state, &id).await
}

async fn bearer_destroy_grant(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Response, ConsoleError> {
    destroy_grant_response(&state, &id).await
}

// --- responses shared by several namespaces -------------------------------

async fn principal_list_response(
    state: &AppState,
    raw: &HashMap<String, String>,
    console: bool,
) -> Result<Response, ConsoleError> {
    let pool = pool(state)?;
    let params = ListParams::from_query(raw)?;
    let labels = label_binding(&params);
    let (rows, total) =
        list_rows::<PrincipalRow, _>(&pool, &params, principal_list_source(false), |arguments| {
            bind(arguments, labels.clone())?;
            bind(arguments, Option::<String>::None)
        })
        .await?;
    let views = principal_views(&pool, &rows).await?;
    if console {
        let data: Vec<ConsolePrincipalPayload> =
            views.iter().map(console_principal_payload).collect();
        Ok(Json(list_response(params.pagination, &data, total)).into_response())
    } else {
        let data: Vec<PrincipalPayload> = views.iter().map(principal_payload).collect();
        Ok(Json(list_response(params.pagination, &data, total)).into_response())
    }
}

async fn principal_show_response(state: &AppState, id: &str) -> Result<Response, ConsoleError> {
    let pool = pool(state)?;
    let view = principal_view(&pool, id).await?;
    Ok(json_data(principal_payload(&view)))
}

async fn principal_roles_response(
    state: &AppState,
    principal_id: &str,
) -> Result<Response, ConsoleError> {
    let pool = pool(state)?;
    let view = principal_view(&pool, principal_id).await?;
    let payload = principal_roles_payload(&pool, view.row.id).await?;
    Ok(json_data(payload))
}

async fn assign_role_response(
    state: &AppState,
    principal_id: &str,
    write: &AssignRole,
) -> Result<Response, ConsoleError> {
    let pool = pool(state)?;
    let principal = principal_view(&pool, principal_id).await?;
    let (role, inserted) = assign_role(&pool, principal.row.id, &write.role_id).await?;
    let payload = role_payload(&role);
    Ok(if inserted {
        created_data(payload)
    } else {
        json_data(payload)
    })
}

async fn role_list_response(
    state: &AppState,
    raw: &HashMap<String, String>,
    query: &RoleListQuery,
) -> Result<Response, ConsoleError> {
    let pool = pool(state)?;
    let params = ListParams::from_query(raw)?;
    let exclude = match query
        .exclude_principal
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some(value) => Some(find_principal_by_oid(&pool, value).await?.row.id),
        None => None,
    };
    let labels = label_binding(&params);
    let q = query
        .q
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(like_pattern);
    let (rows, total) = list_rows::<RoleRow, _>(&pool, &params, role_list_source(), |arguments| {
        bind(arguments, exclude)?;
        bind(arguments, labels.clone())?;
        bind(arguments, q.clone())
    })
    .await?;
    let views = role_views(&pool, &rows).await?;
    let data: Vec<RolePayload> = views.iter().map(role_payload).collect();
    Ok(Json(list_response(params.pagination, &data, total)).into_response())
}

async fn bearer_role_list_response(
    state: &AppState,
    raw: &HashMap<String, String>,
) -> Result<Response, ConsoleError> {
    let pool = pool(state)?;
    let params = ListParams::from_query(raw)?;
    let labels = label_binding(&params);
    let (rows, total) = list_rows::<RoleRow, _>(&pool, &params, role_list_source(), |arguments| {
        bind(arguments, Option::<i64>::None)?;
        bind(arguments, labels.clone())?;
        bind(arguments, Option::<String>::None)
    })
    .await?;
    let views = role_views(&pool, &rows).await?;
    let data: Vec<RolePayload> = views.iter().map(role_payload).collect();
    Ok(Json(list_response(params.pagination, &data, total)).into_response())
}

async fn role_show_response(state: &AppState, id: &str) -> Result<Response, ConsoleError> {
    let pool = pool(state)?;
    let view = role_view(&pool, id).await?;
    Ok(json_data(role_payload(&view)))
}

async fn role_delete_response(state: &AppState, id: &str) -> Result<Response, ConsoleError> {
    let pool = pool(state)?;
    let view = role_view(&pool, id).await?;
    delete_role(&pool, &view).await?;
    Ok(no_content())
}

async fn grantee_grants(
    state: &AppState,
    grantee: SubjectKind,
    id: &str,
    raw: &HashMap<String, String>,
    console: bool,
) -> Result<Response, ConsoleError> {
    let pool = pool(state)?;
    let pagination = Pagination::from_query(raw)?;
    let params = ListParams {
        pagination,
        labels: BTreeMap::new(),
    };
    let (grantee_id, source) = match grantee {
        SubjectKind::Principal => {
            let view = find_principal_by_oid(&pool, id).await?;
            (view.row.id, grant_list_source(SubjectKind::Principal))
        }
        SubjectKind::Role => {
            let view = find_role_by_oid(&pool, id).await?;
            (view.row.id, grant_list_source(SubjectKind::Role))
        }
    };
    let (rows, total) = list_rows::<centaur_console_db::areas::principals::GrantRow, _>(
        &pool,
        &params,
        source,
        |arguments| bind(arguments, grantee_id),
    )
    .await?;
    let mut data: Vec<GrantPayload> = Vec::with_capacity(rows.len());
    for row in &rows {
        data.push(if console {
            console_grant_payload(&pool, row).await?
        } else {
            grant_payload(row)?
        });
    }
    Ok(Json(list_response(pagination, &data, total)).into_response())
}

async fn effective_grants_response(
    state: &AppState,
    id: &str,
    raw: &HashMap<String, String>,
) -> Result<Response, ConsoleError> {
    let pool = pool(state)?;
    let view = find_principal_by_oid(&pool, id).await?;
    let pagination = Pagination::from_query(raw)?;
    let (data, page, total) =
        effective_grants(&pool, view.row.id, pagination.page, pagination.limit).await?;
    // Rails renders `total_pages = 0` for an empty page, unlike the page
    // clamp, which always allows page 1.
    let total_pages = if total == 0 {
        0
    } else {
        (((total as f64) / (pagination.limit as f64)).ceil() as i64).max(1)
    };
    Ok(Json(EffectiveGrantsPayload {
        data,
        meta: EffectiveGrantsMeta {
            page,
            limit: pagination.limit,
            total,
            total_pages,
        },
    })
    .into_response())
}

async fn permission_owner(
    pool: &sqlx::PgPool,
    owner: OwnerKind,
    id: &str,
) -> Result<PermissionOwner, ConsoleError> {
    Ok(match owner {
        OwnerKind::Principal => {
            PermissionOwner::Principal(find_principal_by_oid(pool, id).await?.row.id)
        }
        OwnerKind::Role => PermissionOwner::Role(find_role_by_oid(pool, id).await?.row.id),
    })
}

async fn replace_permissions_response(
    state: &AppState,
    owner: OwnerKind,
    id: &str,
    write: &PermissionSetWrite,
) -> Result<Response, ConsoleError> {
    let pool = pool(state)?;
    let owner = permission_owner(&pool, owner, id).await?;
    let payload = replace_slack_channel_permissions(&pool, owner, write.rows()).await?;
    Ok(json_data(payload))
}

async fn upsert_permission_response(
    state: &AppState,
    owner: OwnerKind,
    id: &str,
    write: &PermissionWrite,
) -> Result<Response, ConsoleError> {
    let pool = pool(state)?;
    let owner = permission_owner(&pool, owner, id).await?;
    let (payload, inserted) = upsert_slack_channel_permission(&pool, owner, write).await?;
    Ok(if inserted {
        created_data(payload)
    } else {
        json_data(payload)
    })
}

async fn destroy_permission_response(
    state: &AppState,
    owner: OwnerKind,
    id: &str,
    channel_id: &str,
) -> Result<Response, ConsoleError> {
    let pool = pool(state)?;
    let owner = permission_owner(&pool, owner, id).await?;
    destroy_slack_channel_permission(&pool, owner, channel_id).await?;
    Ok(no_content())
}

async fn create_grant_response(
    state: &AppState,
    created_by: i64,
    write: &GrantWrite,
) -> Result<Response, ConsoleError> {
    let pool = pool(state)?;
    let (row, inserted) = create_grant(&pool, write, created_by).await?;
    let payload = grant_payload(&row)?;
    Ok(if inserted {
        created_data(payload)
    } else {
        json_data(payload)
    })
}

async fn show_grant_response(state: &AppState, id: &str) -> Result<Response, ConsoleError> {
    let pool = pool(state)?;
    let payload = show_grant(&pool, id).await?;
    Ok(json_data(payload))
}

async fn destroy_grant_response(state: &AppState, id: &str) -> Result<Response, ConsoleError> {
    let pool = pool(state)?;
    destroy_grant(&pool, id).await?;
    Ok(no_content())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sort_gate_accepts_only_the_name_order() {
        let sort = |body: &str| {
            let query: PrincipalListQuery = serde_json::from_str(body).expect("a typed query");
            query
                .validate()
                .ok()
                .map(|()| query.sort.as_deref().map(str::trim) == Some("name"))
        };
        assert_eq!(sort(r#"{}"#), Some(false));
        assert_eq!(sort(r#"{"sort": ""}"#), Some(false));
        assert_eq!(sort(r#"{"sort": "  "}"#), Some(false));
        assert_eq!(sort(r#"{"sort": "name"}"#), Some(true));
        // Rails strips the parameter before matching it.
        assert_eq!(sort(r#"{"sort": " name "}"#), Some(true));
        assert_eq!(sort(r#"{"sort": "bogus"}"#), None);
    }

    #[test]
    fn the_exclude_owner_gate_accepts_only_the_two_selectors() {
        let gate = |body: &str| {
            let query: SlackChannelsQuery = serde_json::from_str(body).expect("a typed query");
            query.validate().is_ok()
        };
        assert!(gate(r#"{}"#));
        assert!(gate(r#"{"exclude_owner": ""}"#));
        assert!(gate(r#"{"exclude_owner": "  "}"#));
        assert!(gate(r#"{"exclude_owner": "principal:prn_abc"}"#));
        assert!(gate(r#"{"exclude_owner": " role:role_abc "}"#));
        // A known kind with an empty oid is a reference the database answers.
        assert!(gate(r#"{"exclude_owner": "principal:"}"#));
        assert!(!gate(r#"{"exclude_owner": "bogus"}"#));
        assert!(!gate(r#"{"exclude_owner": "user:prn_abc"}"#));
    }
}
