//! Console area: secrets.
//!
//! All eight grantable secret kinds are one area. Each kind is a marker type in
//! this module implementing [`SecretRoutes`], which names the kind's table
//! descriptor (`SecretKind`), its bearer write document, and its record payload;
//! the marker traits below say which namespaces mount it:
//!
//! - [`EditableKind`]: the four kinds the Console SPA edits (static, pg_dsn,
//!   gcp_auth, gcp_id_token). `/api/v1/console/*` through the console session
//!   guard.
//! - every kind is a [`SecretRoutes`]: `/api/v1/*` through the bearer API-key
//!   guard, including the canonical `/lookup/:foreign_id` lookup.
//!
//! Routes are registered from one generic handler set per namespace, so a kind
//! declares its support once instead of repeating the handler boilerplate.
//!
//! Persistence, document parsing, and validation all live in
//! `centaur_console_db::areas::secrets`; this module only turns HTTP into those
//! calls and renders the Console payloads.

mod payloads;

use std::collections::HashMap;

use axum::{
    Extension, Json,
    extract::{Path, Query, State},
    http::{Method, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use centaur_console_db::{
    ConsoleUser,
    areas::secrets::{
        AwsAuthRequest, Grantee, HmacRequest, OauthTokenRequest, SecretDocumentPayload,
        SecretFilter, SecretKind, SecretRow, SshKeyRequest, WriteDocument, WriteOptions,
        WriteTarget, create_role_grant, delete_role_grant, delete_secret, find_role,
        find_role_grant, find_secret, find_secret_by_foreign_id, list_kind_secrets, list_secrets,
        role_grants, write_secret,
    },
    find_principal,
    model::{ConsoleModel, Principal},
};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;

use crate::{
    AppState,
    console::{
        guard::{BearerUser, GuardedRoutes, SandboxDelegated},
        http::{ConsoleError, ListParams, created_data, json_data, list_response, no_content},
        request::{ConsoleData, ConsoleQuery},
    },
};

/// One kind's HTTP surface.
trait SecretRoutes: 'static {
    /// The kind this marker registers.
    const KIND: SecretKind;
    /// The typed `data` object of a bearer write.
    type Write: DeserializeOwned + Into<WriteDocument> + Send + 'static;
    /// The bearer/console record payload.
    type Record: Serialize;

    fn record(row: &SecretRow) -> Self::Record;
}

/// The four kinds the Console SPA edits; their write body is the shared
/// document and the console detail page renders them all.
trait EditableKind: SecretRoutes<Write = SecretDocumentPayload> {}

macro_rules! secret_kind {
    (
        $(#[$doc:meta])*
        $name:ident => $kind:ident, write: $write:ty, record: $record:ident $(,)?
    ) => {
        $(#[$doc])*
        struct $name;

        impl SecretRoutes for $name {
            const KIND: SecretKind = SecretKind::$kind;

            type Write = $write;
            type Record = payloads::$record;

            fn record(row: &SecretRow) -> Self::Record {
                payloads::$record::from(row)
            }
        }
    };
}

secret_kind! {
    /// `static_secrets`
    Static => Static, write: SecretDocumentPayload, record: StaticRecord
}
secret_kind! {
    /// `pg_dsn_secrets`
    PgDsn => PgDsn, write: SecretDocumentPayload, record: PgDsnRecord
}
secret_kind! {
    /// `gcp_auth_secrets`
    GcpAuth => GcpAuth, write: SecretDocumentPayload, record: GcpAuthRecord
}
secret_kind! {
    /// `gcp_id_token_secrets`
    GcpIdToken => GcpIdToken, write: SecretDocumentPayload, record: GcpIdTokenRecord
}
secret_kind! {
    /// `aws_auth_secrets`
    AwsAuth => AwsAuth, write: AwsAuthRequest, record: AwsAuthRecord
}
secret_kind! {
    /// `oauth_token_secrets`
    OauthToken => OauthToken, write: OauthTokenRequest, record: OauthTokenRecord
}
secret_kind! {
    /// `hmac_secrets`
    Hmac => Hmac, write: HmacRequest, record: HmacRecord
}
secret_kind! {
    /// `ssh_key_secrets`
    SshKey => SshKey, write: SshKeyRequest, record: SshKeyRecord,
}

impl EditableKind for Static {}
impl EditableKind for PgDsn {}
impl EditableKind for GcpAuth {}
impl EditableKind for GcpIdToken {}

/// Registers one namespace's generic route builder for every listed marker.
macro_rules! register_kinds {
    ($routes:expr, $register:ident, [$($kind:ty),* $(,)?]) => {{
        let routes = $routes;
        $(let routes = $register::<$kind>(routes);)*
        routes
    }};
}

/// `GET /api/v1/console/secrets` filters. Pagination and `labels[key]` come
/// from [`ListParams`], so the integer parsing and clamping are unchanged.
#[derive(Debug, Deserialize)]
struct ConsoleIndexQuery {
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    q: Option<String>,
    #[serde(default)]
    exclude_grantee: Option<String>,
}

/// `POST .../role_grants` reads `data.require(:role_id)`. The typed field makes
/// a missing or non-string role id a validation error instead of a 400.
#[derive(Debug, Deserialize)]
struct CreateRoleGrant {
    role_id: String,
}

/// Routes under `/api/v1/console` (paths relative to that prefix).
pub(crate) fn console(routes: GuardedRoutes) -> GuardedRoutes {
    let routes = routes
        .console_admin("/secrets", get(console_index))
        .console_admin("/secrets/{kind}/{id}", get(console_show))
        .console_admin(
            "/secrets/{kind}/{secret_id}/role_grants",
            get(console_role_grants).post(console_create_role_grant),
        )
        .console_admin(
            "/secrets/{kind}/{secret_id}/role_grants/{id}",
            axum::routing::delete(console_delete_role_grant),
        );
    register_kinds!(routes, console_kind, [Static, PgDsn, GcpAuth, GcpIdToken])
}

/// Routes under `/api/v1` (bearer API; paths relative to that prefix).
pub(crate) fn bearer(routes: GuardedRoutes) -> GuardedRoutes {
    register_kinds!(
        routes,
        bearer_kind,
        [
            Static, GcpAuth, GcpIdToken, AwsAuth, OauthToken, PgDsn, Hmac, SshKey
        ]
    )
}

// ---------------------------------------------------------------------------
// Console
// ---------------------------------------------------------------------------

async fn console_index(
    State(state): State<AppState>,
    _user: ConsoleUser,
    Query(raw): Query<HashMap<String, String>>,
    ConsoleQuery(query): ConsoleQuery<ConsoleIndexQuery>,
) -> Result<Response, ConsoleError> {
    let pool = pool(&state)?;
    let params = ListParams::from_query(&raw)?;
    let pagination = params.pagination;
    let kind = match query.kind.as_deref().filter(|value| !value.is_empty()) {
        Some(kind) => match SecretKind::parse(kind) {
            Some(kind) => Some(kind),
            None => {
                return Err(ConsoleError::bad_request(format!(
                    "kind must be one of {}",
                    SecretKind::ALL
                        .iter()
                        .map(|kind| kind.slug())
                        .collect::<Vec<_>>()
                        .join(", ")
                )));
            }
        },
        None => None,
    };
    let exclude = exclude_grantee(&pool, query.exclude_grantee.as_deref()).await?;
    let filter = SecretFilter {
        kind,
        query: query.q.unwrap_or_default().trim().to_owned(),
        exclude,
        page: pagination.page,
        limit: pagination.limit,
    };
    let page = list_secrets(&pool, &filter).await?;
    let rows: Vec<_> = page.rows.iter().map(payloads::summary).collect();
    Ok(Json(list_response(pagination, &rows, page.total)).into_response())
}

async fn console_show(
    State(state): State<AppState>,
    _user: ConsoleUser,
    Path((kind, id)): Path<(String, String)>,
) -> Result<Response, ConsoleError> {
    let Some(kind) = SecretKind::parse(&kind) else {
        return Err(ConsoleError::not_found("secret not found"));
    };
    let pool = pool(&state)?;
    let row = find_secret(&pool, kind, &id)
        .await?
        .ok_or_else(|| not_found_secret(kind))?;
    Ok(json_data(payloads::detail(&row)))
}

async fn console_role_grants(
    State(state): State<AppState>,
    _user: ConsoleUser,
    Path((kind, secret_id)): Path<(String, String)>,
) -> Result<Response, ConsoleError> {
    let (pool, kind, secret) = console_secret(&state, &kind, &secret_id).await?;
    let (grants, assignable) = role_grants(&pool, kind, secret.id).await?;
    Ok(Json(payloads::RoleGrantsResponse {
        data: grants
            .iter()
            .map(|grant| payloads::grant(grant, &secret))
            .collect(),
        assignable_roles: assignable.iter().map(payloads::RolePayload::from).collect(),
    })
    .into_response())
}

async fn console_create_role_grant(
    State(state): State<AppState>,
    user: ConsoleUser,
    Path((kind, secret_id)): Path<(String, String)>,
    ConsoleData(data): ConsoleData<CreateRoleGrant>,
) -> Result<Response, ConsoleError> {
    let (pool, kind, secret) = console_secret(&state, &kind, &secret_id).await?;
    let role = find_role(&pool, &data.role_id)
        .await?
        .ok_or_else(|| ConsoleError::not_found("Role not found"))?;
    let (grant, created) =
        create_role_grant(&pool, kind, secret.id, role.id, Some(user.id)).await?;
    Ok(document_response(payloads::grant(&grant, &secret), created))
}

async fn console_delete_role_grant(
    State(state): State<AppState>,
    _user: ConsoleUser,
    Path((kind, secret_id, id)): Path<(String, String, String)>,
) -> Result<Response, ConsoleError> {
    let (pool, kind, secret) = console_secret(&state, &kind, &secret_id).await?;
    let grant = find_role_grant(&pool, kind, secret.id, &id)
        .await?
        .ok_or_else(|| ConsoleError::not_found("Grant not found"))?;
    delete_role_grant(&pool, kind, secret.id, grant.id).await?;
    Ok(no_content())
}

#[allow(clippy::result_large_err)] // The error is the HTTP response this handler returns.
async fn console_secret(
    state: &AppState,
    kind: &str,
    oid: &str,
) -> Result<(PgPool, SecretKind, SecretRow), ConsoleError> {
    let Some(kind) = SecretKind::parse(kind) else {
        return Err(ConsoleError::not_found("secret not found"));
    };
    let pool = pool(state)?;
    let secret = find_secret(&pool, kind, oid)
        .await?
        .ok_or_else(|| not_found_secret(kind))?;
    Ok((pool, kind, secret))
}

fn console_kind<K: EditableKind>(routes: GuardedRoutes) -> GuardedRoutes {
    let base = format!("/{}_secrets", K::KIND.slug());
    let item = format!("{base}/{{id}}");
    routes
        .console_admin(&base, post(console_create::<K>).get(missing_route))
        .console_admin(
            &item,
            get(console_kind_show::<K>)
                .patch(console_update::<K>)
                .delete(console_destroy::<K>),
        )
}

async fn console_create<K: EditableKind>(
    State(state): State<AppState>,
    user: ConsoleUser,
    delegated: Option<Extension<SandboxDelegated>>,
    ConsoleData(data): ConsoleData<SecretDocumentPayload>,
) -> Result<Response, ConsoleError> {
    if delegated.is_some() {
        refuse_sandbox_source(K::KIND, &data)?;
    }
    let pool = pool(&state)?;
    write_kind::<K>(
        &pool,
        WriteTarget::New,
        WriteDocument::Console(data),
        Some(user.id),
        console_options(false, false),
    )
    .await
}

async fn console_kind_show<K: EditableKind>(
    State(state): State<AppState>,
    _user: ConsoleUser,
    Path(id): Path<String>,
) -> Result<Response, ConsoleError> {
    let pool = pool(&state)?;
    let row = find_secret(&pool, K::KIND, &id)
        .await?
        .ok_or_else(|| not_found_secret(K::KIND))?;
    Ok(json_data(K::record(&row)))
}

async fn console_update<K: EditableKind>(
    State(state): State<AppState>,
    user: ConsoleUser,
    delegated: Option<Extension<SandboxDelegated>>,
    Path(id): Path<String>,
    method: Method,
    ConsoleData(data): ConsoleData<SecretDocumentPayload>,
) -> Result<Response, ConsoleError> {
    let pool = pool(&state)?;
    let target = target_for(K::KIND, &id);
    if delegated.is_some() {
        refuse_sandbox_update(&pool, K::KIND, &target, &data).await?;
    }
    let clear_foreign_id = matches!(target, WriteTarget::Oid(_));
    write_kind::<K>(
        &pool,
        target,
        WriteDocument::Console(data),
        Some(user.id),
        console_options(method == Method::PATCH, clear_foreign_id),
    )
    .await
}

async fn console_destroy<K: EditableKind>(
    State(state): State<AppState>,
    _user: ConsoleUser,
    delegated: Option<Extension<SandboxDelegated>>,
    Path(id): Path<String>,
) -> Result<Response, ConsoleError> {
    if delegated.is_some() {
        return Err(ConsoleError::new(
            StatusCode::FORBIDDEN,
            "secrets cannot be deleted from a sandbox",
        ));
    }
    let pool = pool(&state)?;
    delete_kind(&pool, K::KIND, &id).await
}

// ---------------------------------------------------------------------------
// Sandbox-delegated writes
// ---------------------------------------------------------------------------

/// Source types a sandbox may never set: an inline value would pass through the
/// agent, and an OAuth credential is managed by the consent flow.
const SANDBOX_REFUSED_SOURCE_TYPES: [&str; 2] = ["control_plane", "token_broker"];

/// A Slack DM sandbox acting for an admin may
/// only point a secret at an external store, never supply an inline
/// (`control_plane`) value or an OAuth (`token_broker`) credential.
#[allow(clippy::result_large_err)] // The error is the HTTP response this handler returns.
fn refuse_sandbox_source(
    kind: SecretKind,
    data: &SecretDocumentPayload,
) -> Result<(), ConsoleError> {
    let source_type = data
        .source_for(kind)
        .and_then(|source| source.source_type.as_deref());
    match source_type {
        Some(source_type) if SANDBOX_REFUSED_SOURCE_TYPES.contains(&source_type) => {
            Err(ConsoleError::new(
                StatusCode::FORBIDDEN,
                format!(
                    "{source_type} sources cannot be set from a sandbox; reference an external store"
                ),
            ))
        }
        _ => Ok(()),
    }
}

/// The update half of sandbox admin secret writes: updates address an existing
/// secret by oid, and a secret backed by an OAuth credential stays read-only.
async fn refuse_sandbox_update(
    pool: &PgPool,
    kind: SecretKind,
    target: &WriteTarget,
    data: &SecretDocumentPayload,
) -> Result<(), ConsoleError> {
    refuse_sandbox_source(kind, data)?;
    let WriteTarget::Oid(oid) = target else {
        return Err(not_found_secret(kind));
    };
    let row = find_secret(pool, kind, oid)
        .await?
        .ok_or_else(|| not_found_secret(kind))?;
    if row
        .source()
        .is_some_and(|source| source.source_type == "token_broker")
    {
        return Err(ConsoleError::new(
            StatusCode::FORBIDDEN,
            "OAuth credential secrets cannot be edited from a sandbox",
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Bearer
// ---------------------------------------------------------------------------

fn bearer_kind<K: SecretRoutes>(routes: GuardedRoutes) -> GuardedRoutes {
    let base = format!("/{}_secrets", K::KIND.slug());
    let item = format!("{base}/{{id}}");
    let routes = routes
        .operator(&base, get(bearer_index::<K>).post(bearer_create::<K>))
        .operator(
            &item,
            get(bearer_show::<K>)
                .put(bearer_update::<K>)
                .delete(bearer_destroy::<K>),
        );
    let lookup = format!("{base}/lookup/{{foreign_id}}");
    routes.operator(&lookup, get(bearer_lookup::<K>))
}

async fn bearer_index<K: SecretRoutes>(
    State(state): State<AppState>,
    _user: BearerUser,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Response, ConsoleError> {
    let pool = pool(&state)?;
    let params = ListParams::from_query(&query)?;
    let page = list_kind_secrets(
        &pool,
        K::KIND,
        &params.labels,
        params.pagination.page,
        params.pagination.limit,
    )
    .await?;
    let rows: Vec<_> = page.rows.iter().map(K::record).collect();
    Ok(Json(list_response(params.pagination, &rows, page.total)).into_response())
}

async fn bearer_show<K: SecretRoutes>(
    State(state): State<AppState>,
    _user: BearerUser,
    Path(id): Path<String>,
) -> Result<Response, ConsoleError> {
    let pool = pool(&state)?;
    let row = find_secret(&pool, K::KIND, &id)
        .await?
        .ok_or_else(|| not_found_secret(K::KIND))?;
    Ok(json_data(K::record(&row)))
}

async fn bearer_create<K: SecretRoutes>(
    State(state): State<AppState>,
    user: BearerUser,
    ConsoleData(data): ConsoleData<K::Write>,
) -> Result<Response, ConsoleError> {
    let pool = pool(&state)?;
    write_kind::<K>(
        &pool,
        WriteTarget::New,
        data.into(),
        Some(user.0.id),
        WriteOptions::default(),
    )
    .await
}

async fn bearer_update<K: SecretRoutes>(
    State(state): State<AppState>,
    user: BearerUser,
    Path(id): Path<String>,
    ConsoleData(data): ConsoleData<K::Write>,
) -> Result<Response, ConsoleError> {
    let pool = pool(&state)?;
    refuse_personal_key(&pool, K::KIND, &id).await?;
    write_kind::<K>(
        &pool,
        target_for(K::KIND, &id),
        data.into(),
        Some(user.0.id),
        WriteOptions::default(),
    )
    .await
}

async fn bearer_destroy<K: SecretRoutes>(
    State(state): State<AppState>,
    _user: BearerUser,
    Path(id): Path<String>,
) -> Result<Response, ConsoleError> {
    let pool = pool(&state)?;
    delete_kind(&pool, K::KIND, &id).await
}

async fn bearer_lookup<K: SecretRoutes>(
    State(state): State<AppState>,
    _user: BearerUser,
    Path(foreign_id): Path<String>,
) -> Result<Response, ConsoleError> {
    let pool = pool(&state)?;
    let row = find_secret_by_foreign_id(&pool, K::KIND, &foreign_id)
        .await?
        .ok_or_else(|| not_found_secret(K::KIND))?;
    Ok(json_data(K::record(&row)))
}

/// A personal SSH key may only be edited by its owner. A missing record is left
/// to the write, so its 404 comes from there.
async fn refuse_personal_key(
    pool: &PgPool,
    kind: SecretKind,
    id: &str,
) -> Result<(), ConsoleError> {
    if kind != SecretKind::SshKey {
        return Ok(());
    }
    let row = match target_for(kind, id) {
        WriteTarget::Oid(oid) => find_secret(pool, kind, &oid).await.ok().flatten(),
        WriteTarget::ForeignId(foreign_id) => find_secret_by_foreign_id(pool, kind, &foreign_id)
            .await
            .ok()
            .flatten(),
        WriteTarget::New => None,
    };
    if row.is_some_and(|row| row.owner_id.is_some()) {
        return Err(ConsoleError::new(
            StatusCode::FORBIDDEN,
            "personal SSH keys are managed by their owner",
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Shared handlers
// ---------------------------------------------------------------------------

async fn write_kind<K: SecretRoutes>(
    pool: &PgPool,
    target: WriteTarget,
    document: WriteDocument,
    operator_id: Option<i64>,
    options: WriteOptions,
) -> Result<Response, ConsoleError> {
    let result = write_secret(pool, K::KIND, target, document, operator_id, options).await?;
    Ok(document_response(K::record(&result.row), result.created))
}

async fn delete_kind(pool: &PgPool, kind: SecretKind, oid: &str) -> Result<Response, ConsoleError> {
    let Some(id) = kind.decode_oid(oid) else {
        return Err(not_found_secret(kind));
    };
    delete_secret(pool, kind, id).await?;
    Ok(no_content())
}

fn not_found_secret(kind: SecretKind) -> ConsoleError {
    kind.not_found().into()
}

fn console_options(retain_inline_secret: bool, clear_foreign_id: bool) -> WriteOptions {
    WriteOptions {
        retain_inline_secret,
        explicit_foreign_id_clear: clear_foreign_id,
    }
}

/// An `:id` with the model's oid prefix is an oid that must exist, and anything
/// else is a foreign id upsert (`resolve_for_upsert`).
fn target_for(kind: SecretKind, id: &str) -> WriteTarget {
    if id.starts_with(&format!("{}_", kind.oid_prefix())) {
        WriteTarget::Oid(id.to_owned())
    } else {
        WriteTarget::ForeignId(id.to_owned())
    }
}

/// A collection path that only accepts writes answers a stray read with not
/// found, rather than axum's method-not-allowed.
async fn missing_route() -> Response {
    ConsoleError::not_found("not found").into_response()
}

/// `201 Created` for a new record and `200 OK` for an existing one.
fn document_response<T: Serialize>(payload: T, created: bool) -> Response {
    if created {
        created_data(payload)
    } else {
        json_data(payload)
    }
}

/// The writable Console pool every handler in this area reads and writes.
fn pool(state: &AppState) -> Result<PgPool, ConsoleError> {
    state
        .console_session_db()
        .map_err(|error| ConsoleError::new(StatusCode::SERVICE_UNAVAILABLE, error.to_string()))
}

async fn exclude_grantee(
    pool: &PgPool,
    raw: Option<&str>,
) -> Result<Option<Grantee>, ConsoleError> {
    let Some(raw) = raw.filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    let (kind, oid) = raw.split_once(':').unwrap_or((raw, ""));
    match kind {
        "principal" => {
            let Some(id) = Principal::decode_oid(oid) else {
                return Err(ConsoleError::not_found("Principal not found"));
            };
            match find_principal(pool, id).await {
                Ok(Some(_)) => Ok(Some(Grantee::Principal(id))),
                Ok(None) => Err(ConsoleError::not_found("Principal not found")),
                Err(error) => Err(ConsoleError::internal(error)),
            }
        }
        "role" => match find_role(pool, oid).await {
            Ok(Some(role)) => Ok(Some(Grantee::Role(role.id))),
            Ok(None) => Err(ConsoleError::not_found("Role not found")),
            Err(error) => Err(ConsoleError::internal(error)),
        },
        _ => Err(ConsoleError::bad_request(
            "exclude_grantee must be principal:<oid> or role:<oid>",
        )),
    }
}
