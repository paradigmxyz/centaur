//! Secrets: the eight kinds of grantable credential across the bearer API
//! (`/api/v1/<kind>_secrets`), the console (the cross-kind list and detail,
//! role grants, and the four kinds it edits), and the sandbox admin API
//! (find, create, and edit secrets that reference external stores).
//!
//! - [`kind`]: what distinguishes the kinds;
//! - [`draft`]: a request document, parsed and validated;
//! - [`parts`]: sources and request rules;
//! - [`write`]: create, replace, delete (one transaction, row lock first);
//! - [`store`]: reads and the per-kind resource JSON;
//! - [`console`], [`view`]: the console's list, detail, and role grants.

pub mod console;
pub mod draft;
pub mod dsn;
pub mod kind;
pub mod parts;
pub mod store;
pub mod view;
pub mod write;

use axum::{
    Router,
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use self::{
    kind::{ALL, Kind},
    write::{Flavor, Target},
};
use crate::{
    ApiResult, AppState, Routes,
    auth::{BearerCaller, ConsoleCaller, SandboxAdminCaller},
    extract::{Data, Path, Query, data},
    pagination::PageParams,
};

/// A static secret that wraps a broker credential changed: grant it to the
/// user principals the credential belongs to.
pub(crate) async fn reconcile_wrapped_credential(state: &AppState, broker_credential_id: i64) {
    super::principals::effects::credential_changed(state, broker_credential_id).await;
}

/// `labels[key]=value` query parameters (the filter the list endpoints
/// take); every other parameter is ignored.
fn label_filter(params: Vec<(String, String)>) -> Map<String, Value> {
    params
        .into_iter()
        .filter_map(|(name, value)| {
            let key = name.strip_prefix("labels[")?.strip_suffix(']')?;
            Some((key.to_owned(), Value::String(value)))
        })
        .collect()
}

async fn render(state: &AppState, kind: Kind, id: i64) -> ApiResult<Value> {
    let mut conn = state.db.acquire().await?;
    let loaded = store::load_oid(&mut conn, kind, &kind.oid(id)).await?;
    Ok(loaded.payload())
}

async fn written(state: &AppState, kind: Kind, saved: write::Saved) -> ApiResult {
    let payload = render(state, kind, saved.id).await?;
    Ok((saved.status(), data(payload)).into_response())
}

// --- bearer ----------------------------------------------------------------

async fn list(
    state: &AppState,
    kind: Kind,
    labels: Vec<(String, String)>,
    page: PageParams,
) -> ApiResult {
    let page = page.resolve();
    let mut conn = state.db.acquire().await?;
    let (rows, total) = store::list(&mut conn, kind, &label_filter(labels), page).await?;
    let rows: Vec<Value> = rows.iter().map(store::Loaded::payload).collect();
    Ok(axum::Json(json!({"data": rows, "meta": page.meta(total)})).into_response())
}

async fn show(state: &AppState, kind: Kind, oid: &str) -> ApiResult {
    let mut conn = state.db.acquire().await?;
    let loaded = store::load_oid(&mut conn, kind, oid).await?;
    Ok(data(loaded.payload()).into_response())
}

async fn lookup(state: &AppState, kind: Kind, foreign_id: &str) -> ApiResult {
    let mut conn = state.db.acquire().await?;
    let loaded = store::load_foreign_id(&mut conn, kind, foreign_id).await?;
    Ok(data(loaded.payload()).into_response())
}

async fn destroy(state: &AppState, kind: Kind, oid: &str) -> ApiResult {
    write::destroy(state, kind, oid).await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// `/api/v1/<kind>_secrets`: list, create, lookup, show, upsert (by oid or
/// foreign id), delete.
fn bearer_resource(kind: Kind) -> Router<AppState> {
    let base = format!("/{}", kind.table());
    let upsert = move |State(state): State<AppState>,
                       BearerCaller(auth): BearerCaller,
                       Path(id): Path<String>,
                       Data(body): Data<Value>| async move {
        let saved = write::save(
            &state,
            kind,
            Flavor::Bearer,
            auth.user.id,
            Target::Upsert(&id),
            body,
        )
        .await?;
        written(&state, kind, saved).await
    };
    let by_foreign_id = move |State(state): State<AppState>,
                              _caller: BearerCaller,
                              Path(foreign_id): Path<String>| async move {
        lookup(&state, kind, &foreign_id).await
    };
    let mut router = Router::new()
        .route(
            &base,
            get(
                move |State(state): State<AppState>,
                      _caller: BearerCaller,
                      Query(labels): Query<Vec<(String, String)>>,
                      Query(page): Query<PageParams>| async move {
                    list(&state, kind, labels, page).await
                },
            )
            .post(
                move |State(state): State<AppState>,
                      BearerCaller(auth): BearerCaller,
                      Data(body): Data<Value>| async move {
                    let saved = write::save(
                        &state,
                        kind,
                        Flavor::Bearer,
                        auth.user.id,
                        Target::Create,
                        body,
                    )
                    .await?;
                    written(&state, kind, saved).await
                },
            ),
        )
        .route(&format!("{base}/lookup/{{foreign_id}}"), get(by_foreign_id))
        .route(
            &format!("{base}/{{id}}"),
            get(
                move |State(state): State<AppState>,
                      _caller: BearerCaller,
                      Path(id): Path<String>| async move {
                    show(&state, kind, &id).await
                },
            )
            .put(upsert)
            .patch(upsert)
            .delete(
                move |State(state): State<AppState>,
                      _caller: BearerCaller,
                      Path(id): Path<String>| async move {
                    destroy(&state, kind, &id).await
                },
            ),
        );
    if kind != Kind::SshKey {
        router = router.route(
            &format!("{base}/lookup/default/{{foreign_id}}"),
            get(by_foreign_id),
        );
    }
    router
}

// --- console ---------------------------------------------------------------

/// `/api/v1/console/<kind>_secrets`: the kinds the console edits.
fn console_resource(kind: Kind) -> Router<AppState> {
    let base = format!("/{}", kind.table());
    let update = move |State(state): State<AppState>,
                       ConsoleCaller(user): ConsoleCaller,
                       Path(id): Path<String>,
                       Data(body): Data<Value>| async move {
        let saved = write::save(
            &state,
            kind,
            Flavor::Console,
            user.id,
            Target::Oid(&id),
            body,
        )
        .await?;
        written(&state, kind, saved).await
    };
    Router::new()
        .route(
            &base,
            post(
                move |State(state): State<AppState>,
                      ConsoleCaller(user): ConsoleCaller,
                      Data(body): Data<Value>| async move {
                    let saved =
                        write::save(&state, kind, Flavor::Console, user.id, Target::Create, body)
                            .await?;
                    written(&state, kind, saved).await
                },
            ),
        )
        .route(
            &format!("{base}/{{id}}"),
            get(
                move |State(state): State<AppState>,
                      _caller: ConsoleCaller,
                      Path(id): Path<String>| async move {
                    show(&state, kind, &id).await
                },
            )
            .put(update)
            .patch(update)
            .delete(
                move |State(state): State<AppState>,
                      _caller: ConsoleCaller,
                      Path(id): Path<String>| async move {
                    destroy(&state, kind, &id).await
                },
            ),
        )
}

// --- sandbox admin ---------------------------------------------------------

#[derive(sqlx::FromRow)]
struct Summary {
    id: i64,
    foreign_id: Option<String>,
    name: Option<String>,
    description: Option<String>,
    labels: Value,
}

#[derive(Deserialize, Default)]
struct SearchQuery {
    q: Option<String>,
}

/// `GET /api/v1/sandbox/admin/secrets`: grantable secrets of every kind,
/// identifying metadata only.
async fn sandbox_index(
    State(state): State<AppState>,
    _caller: SandboxAdminCaller,
    Query(search): Query<SearchQuery>,
) -> ApiResult {
    let pattern = search
        .q
        .as_deref()
        .map(str::trim)
        .filter(|query| !query.is_empty())
        .map(console::like_pattern);
    let mut rows = Vec::new();
    for kind in ALL {
        let found: Vec<Summary> = sqlx::query_as(&format!(
            "SELECT id, foreign_id, name, description, labels FROM {} \
             WHERE ($1::text IS NULL OR name ILIKE $1 OR foreign_id ILIKE $1) ORDER BY id",
            kind.table()
        ))
        .bind(&pattern)
        .fetch_all(&state.db)
        .await?;
        for Summary {
            id,
            foreign_id,
            name,
            description,
            labels,
        } in found
        {
            rows.push(json!({
                "id": kind.oid(id),
                "grant_key": kind.owner_column(),
                "foreign_id": foreign_id,
                "name": name,
                "description": description,
                "labels": labels,
            }));
        }
    }
    Ok(data(rows).into_response())
}

/// `/api/v1/sandbox/admin/<kind>_secrets`: create, show, and edit (the
/// fields sent change; the rest stays).
fn sandbox_resource(kind: Kind) -> Router<AppState> {
    let base = format!("/{}", kind.table());
    let update = move |State(state): State<AppState>,
                       _caller: SandboxAdminCaller,
                       Path(id): Path<String>,
                       Data(body): Data<Value>| async move {
        let saved = write::save(
            &state,
            kind,
            Flavor::SandboxAdmin,
            0,
            Target::Oid(&id),
            body,
        )
        .await?;
        written(&state, kind, saved).await
    };
    Router::new()
        .route(
            &base,
            post(
                move |State(state): State<AppState>,
                      SandboxAdminCaller(auth): SandboxAdminCaller,
                      Data(body): Data<Value>| async move {
                    let saved = write::save(
                        &state,
                        kind,
                        Flavor::SandboxAdmin,
                        auth.admin.id,
                        Target::Create,
                        body,
                    )
                    .await?;
                    written(&state, kind, saved).await
                },
            ),
        )
        .route(
            &format!("{base}/{{id}}"),
            get(
                move |State(state): State<AppState>,
                      _caller: SandboxAdminCaller,
                      Path(id): Path<String>| async move {
                    show(&state, kind, &id).await
                },
            )
            .put(update)
            .patch(update),
        )
}

pub fn routes() -> Routes {
    let merge = |kinds: &[Kind], resource: fn(Kind) -> Router<AppState>| {
        kinds
            .iter()
            .fold(Router::new(), |router, kind| router.merge(resource(*kind)))
    };
    let console_kinds = [Kind::Static, Kind::PgDsn, Kind::GcpAuth, Kind::GcpIdToken];
    let sandbox_kinds = [Kind::Static, Kind::PgDsn, Kind::GcpIdToken];
    Routes {
        bearer: merge(&ALL, bearer_resource),
        console: merge(&console_kinds, console_resource)
            .route("/secrets", get(console::index))
            .route("/secrets/{kind}/{id}", get(console::show))
            .route(
                "/secrets/{kind}/{id}/role_grants",
                get(console::role_grants).post(console::create_role_grant),
            )
            .route(
                "/secrets/{kind}/{id}/role_grants/{grant}",
                axum::routing::delete(console::delete_role_grant),
            ),
        sandbox_admin: merge(&sandbox_kinds, sandbox_resource)
            .route("/secrets", get(sandbox_index)),
        ..Routes::default()
    }
}
