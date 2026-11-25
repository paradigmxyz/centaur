//! API clients: machine callers of the api-rs session API, and the principals
//! they may run sessions as.
//!
//! Console answers the whole feature as an unknown route while
//! `CENTAUR_CONSOLE_API_CLIENTS_ENABLED` is off, so nothing here is registered
//! in that case. The plaintext secret is returned by `create` and
//! `rotate_secret` only, with `Cache-Control: no-store`.

use axum::{
    extract::{Path, State},
    response::Response,
    routing::{delete, get, post},
};
use centaur_console_db::ConsoleUser;
use centaur_console_db::areas::admin::{
    ApiClientLink, ApiClientPrincipalRef, ApiClientWithLinks, api_client, api_clients,
    attach_api_client_principal, create_api_client, delete_api_client, detach_api_client_principal,
    find_api_client_link, find_api_client_principal, make_default_api_client_principal,
    rotate_api_client_secret, set_api_client_disabled, update_api_client,
};
use centaur_console_db::model::{
    ApiClient as ApiClientModel, ApiClientPrincipal as ApiClientPrincipalModel, ConsoleModel,
    Principal as PrincipalModel,
};
use centaur_console_db::validation::{ValidateFields, rules};
use serde::{Deserialize, Deserializer, Serialize};
use time::PrimitiveDateTime;

use super::common::{self, json_ok, no_store, nullable_field};
use crate::{
    AppState,
    console::guard::GuardedRoutes,
    console::{
        http::{ConsoleError, created_data, json_data, no_content},
        request::ConsoleBody,
        request::ConsoleData,
    },
};

const SCOPES: [&str; 2] = ["sessions:read", "sessions:write"];
const SECRET_PREFIX: &str = "acs_";
const SCOPES_EMPTY_MESSAGE: &str = "must include at least one scope";

/// A new API client: its name, its scopes, and optionally the principal to
/// attach as its default.
#[derive(Deserialize, garde::Validate)]
struct CreateApiClient {
    #[serde(default)]
    #[garde(custom(rules::present_not_blank))]
    name: Option<String>,
    #[serde(default)]
    #[garde(dive)]
    scopes: Scopes,
    /// A reference the handler resolves, not a rule of the record.
    #[serde(default)]
    #[garde(skip)]
    principal: Option<String>,
}

/// A PATCH keeps an absent `name` (leave it) apart from an explicit `null`
/// (presence failure); an absent or null `scopes` leaves the stored list alone.
#[derive(Deserialize, garde::Validate)]
struct UpdateApiClient {
    #[serde(default, deserialize_with = "nullable_field")]
    #[garde(custom(rules::patch_not_blank))]
    name: Option<Option<String>>,
    #[serde(default)]
    #[garde(dive)]
    scopes: Option<Scopes>,
}

/// The scopes list, which is validated as a
/// whole (`scopes_supported`) instead of per item, so the two messages are this
/// type's own [`garde::Validate`] implementation rather than field rules.
///
/// Deserializing drops blanks before validation, and a non-array (including a
/// scalar) is a typed request error, not a coercion.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Scopes(Vec<String>);

impl Scopes {
    fn as_slice(&self) -> &[String] {
        &self.0
    }
}

impl<'de> Deserialize<'de> for Scopes {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self(
            Vec::<String>::deserialize(deserializer)?
                .into_iter()
                .filter(|scope| !scope.trim().is_empty())
                .collect(),
        ))
    }
}

impl garde::Validate for Scopes {
    type Context = ();

    fn validate_into(
        &self,
        _context: &Self::Context,
        parent: &mut dyn FnMut() -> garde::Path,
        report: &mut garde::Report,
    ) {
        if self.0.is_empty() {
            report.append(parent(), garde::Error::new(SCOPES_EMPTY_MESSAGE));
        }
        // `scopes - SCOPES` keeps the list's order and removes duplicates.
        let mut unsupported: Vec<&str> = Vec::new();
        for scope in &self.0 {
            if !SCOPES.contains(&scope.as_str()) && !unsupported.contains(&scope.as_str()) {
                unsupported.push(scope);
            }
        }
        if !unsupported.is_empty() {
            report.append(
                parent(),
                garde::Error::new(format!(
                    "contains unsupported values: {}",
                    unsupported.join(", ")
                )),
            );
        }
    }
}

/// `POST /api_clients/:id/principals` reads `data.principal`, falling back to a
/// top-level `principal`, and does not require a `data` wrapper.
#[derive(Deserialize)]
struct AttachPrincipal {
    #[serde(default)]
    principal: Option<String>,
    #[serde(default)]
    data: Option<AttachPrincipalData>,
}

#[derive(Deserialize)]
struct AttachPrincipalData {
    #[serde(default)]
    principal: Option<String>,
}

/// An API client as Console renders it; `secret` is present only in the
/// create/rotate responses.
#[derive(Serialize)]
struct ApiClientPayload {
    id: String,
    name: String,
    scopes: Vec<String>,
    disabled: bool,
    #[serde(with = "centaur_console_db::timestamp::primitive_option")]
    disabled_at: Option<PrimitiveDateTime>,
    principals_count: usize,
    principals: Vec<LinkPayload>,
    #[serde(with = "centaur_console_db::timestamp::primitive")]
    created_at: PrimitiveDateTime,
    #[serde(with = "centaur_console_db::timestamp::primitive")]
    updated_at: PrimitiveDateTime,
    #[serde(skip_serializing_if = "Option::is_none")]
    secret: Option<String>,
}

#[derive(Serialize)]
struct LinkPayload {
    id: String,
    #[serde(rename = "default")]
    default: bool,
    principal: PrincipalRefPayload,
}

#[derive(Serialize)]
struct PrincipalRefPayload {
    id: String,
    foreign_id: Option<String>,
    name: Option<String>,
    kind: String,
}

/// The unpaginated list envelope: `{data, meta: {total}}`.
#[derive(Serialize)]
struct ApiClientsPayload {
    data: Vec<ApiClientPayload>,
    meta: ApiClientsMeta,
}

#[derive(Serialize)]
struct ApiClientsMeta {
    total: usize,
}

pub(super) fn console(routes: GuardedRoutes) -> GuardedRoutes {
    if !crate::console::api_clients_enabled() {
        return routes;
    }
    routes
        .console_admin("/api_clients", get(index).post(create))
        .console_admin("/api_clients/{id}", get(show).patch(update).delete(destroy))
        .console_admin("/api_clients/{id}/rotate_secret", post(rotate_secret))
        .console_admin("/api_clients/{id}/disable", post(disable))
        .console_admin("/api_clients/{id}/enable", post(enable))
        .console_admin("/api_clients/{id}/principals", post(attach_principal))
        .console_admin(
            "/api_clients/{id}/principals/{link_id}",
            delete(detach_principal),
        )
        .console_admin(
            "/api_clients/{id}/principals/{link_id}/default",
            post(make_default_principal),
        )
}

async fn index(
    State(state): State<AppState>,
    _user: ConsoleUser,
) -> Result<Response, ConsoleError> {
    let pool = common::pool(&state)?;
    let clients = api_clients(&pool).await?;
    Ok(json_ok(ApiClientsPayload {
        meta: ApiClientsMeta {
            total: clients.len(),
        },
        data: clients.iter().map(|client| payload(client, None)).collect(),
    }))
}

async fn show(
    State(state): State<AppState>,
    Path(id): Path<String>,
    _user: ConsoleUser,
) -> Result<Response, ConsoleError> {
    let pool = common::pool(&state)?;
    let client = load(&pool, &id).await?;
    Ok(json_data(payload(&client, None)))
}

async fn create(
    State(state): State<AppState>,
    user: ConsoleUser,
    ConsoleData(data): ConsoleData<CreateApiClient>,
) -> Result<Response, ConsoleError> {
    let pool = common::pool(&state)?;
    // Console checks the principal reference before the record, so a bad
    // reference is reported alone.
    let principal_ref = data
        .principal
        .as_deref()
        .unwrap_or_default()
        .trim()
        .to_owned();
    let principal = find_api_client_principal(&pool, &principal_ref).await?;
    if !principal_ref.is_empty() && principal.is_none() {
        return Err(common::field_error(
            centaur_console_db::FieldErrors::BASE,
            format!("No principal matches \"{principal_ref}\"."),
        ));
    }
    data.validate_fields()?;
    let secret = common::issue_token(SECRET_PREFIX);
    let id = create_api_client(
        &pool,
        data.name.as_deref().unwrap_or_default(),
        data.scopes.as_slice(),
        user.id,
        &common::token_digest(&secret),
        principal.map(|principal| principal.id),
    )
    .await?;
    let client = reload(&pool, id).await?;
    Ok(no_store(created_data(payload(&client, Some(&secret)))))
}

async fn update(
    State(state): State<AppState>,
    Path(id): Path<String>,
    _user: ConsoleUser,
    ConsoleData(data): ConsoleData<UpdateApiClient>,
) -> Result<Response, ConsoleError> {
    let pool = common::pool(&state)?;
    let client = load(&pool, &id).await?;
    data.validate_fields()?;
    if data.name.is_some() || data.scopes.is_some() {
        // An absent attribute keeps the stored value, as `assign_attributes`
        // does before the record is saved.
        let name = data
            .name
            .as_ref()
            .and_then(|name| name.as_deref())
            .unwrap_or(&client.client.name);
        let scopes = data
            .scopes
            .as_ref()
            .map(Scopes::as_slice)
            .unwrap_or(&client.client.scopes);
        update_api_client(&pool, client.client.id, name, scopes).await?;
    }
    let client = reload(&pool, client.client.id).await?;
    Ok(json_data(payload(&client, None)))
}

async fn destroy(
    State(state): State<AppState>,
    Path(id): Path<String>,
    _user: ConsoleUser,
) -> Result<Response, ConsoleError> {
    let pool = common::pool(&state)?;
    let client = load(&pool, &id).await?;
    delete_api_client(&pool, client.client.id).await?;
    Ok(no_content())
}

async fn rotate_secret(
    State(state): State<AppState>,
    Path(id): Path<String>,
    _user: ConsoleUser,
) -> Result<Response, ConsoleError> {
    let pool = common::pool(&state)?;
    let client = load(&pool, &id).await?;
    let secret = common::issue_token(SECRET_PREFIX);
    rotate_api_client_secret(&pool, client.client.id, &common::token_digest(&secret)).await?;
    let client = reload(&pool, client.client.id).await?;
    Ok(no_store(json_data(payload(&client, Some(&secret)))))
}

async fn disable(
    State(state): State<AppState>,
    Path(id): Path<String>,
    _user: ConsoleUser,
) -> Result<Response, ConsoleError> {
    let pool = common::pool(&state)?;
    let client = load(&pool, &id).await?;
    if !client.client.disabled() {
        set_api_client_disabled(&pool, client.client.id, true).await?;
    }
    render(&pool, client.client.id).await
}

async fn enable(
    State(state): State<AppState>,
    Path(id): Path<String>,
    _user: ConsoleUser,
) -> Result<Response, ConsoleError> {
    let pool = common::pool(&state)?;
    let client = load(&pool, &id).await?;
    set_api_client_disabled(&pool, client.client.id, false).await?;
    render(&pool, client.client.id).await
}

async fn attach_principal(
    State(state): State<AppState>,
    Path(id): Path<String>,
    _user: ConsoleUser,
    ConsoleBody(body): ConsoleBody<AttachPrincipal>,
) -> Result<Response, ConsoleError> {
    let pool = common::pool(&state)?;
    let client = load(&pool, &id).await?;
    let principal_ref = body
        .data
        .and_then(|data| data.principal)
        .or(body.principal)
        .unwrap_or_default()
        .trim()
        .to_owned();
    let Some(principal) = find_api_client_principal(&pool, &principal_ref).await? else {
        return Err(common::field_error(
            centaur_console_db::FieldErrors::BASE,
            "No principal matches that OID or foreign ID.",
        ));
    };
    let link = attach_api_client_principal(&pool, client.client.id, principal.id).await?;
    Ok(created_data(link_payload(&link)))
}

async fn detach_principal(
    State(state): State<AppState>,
    Path((id, link_id)): Path<(String, String)>,
    _user: ConsoleUser,
) -> Result<Response, ConsoleError> {
    let pool = common::pool(&state)?;
    let client = load(&pool, &id).await?;
    let link = load_link(&pool, client.client.id, &link_id).await?;
    detach_api_client_principal(&pool, client.client.id, link.id).await?;
    Ok(no_content())
}

async fn make_default_principal(
    State(state): State<AppState>,
    Path((id, link_id)): Path<(String, String)>,
    _user: ConsoleUser,
) -> Result<Response, ConsoleError> {
    let pool = common::pool(&state)?;
    let client = load(&pool, &id).await?;
    let link = load_link(&pool, client.client.id, &link_id).await?;
    let link = make_default_api_client_principal(&pool, client.client.id, link.id)
        .await?
        .ok_or_else(|| ConsoleError::internal("api client link vanished after its update"))?;
    Ok(json_data(link_payload(&link)))
}

async fn load(pool: &sqlx::PgPool, oid: &str) -> Result<ApiClientWithLinks, ConsoleError> {
    Ok(api_client(pool, common::oid::<ApiClientModel>(oid)?)
        .await?
        .ok_or_else(ApiClientModel::not_found)?)
}

async fn load_link(
    pool: &sqlx::PgPool,
    api_client_id: i64,
    oid: &str,
) -> Result<ApiClientLink, ConsoleError> {
    Ok(find_api_client_link(
        pool,
        api_client_id,
        common::oid::<ApiClientPrincipalModel>(oid)?,
    )
    .await?
    .ok_or_else(ApiClientPrincipalModel::not_found)?)
}

/// The reloaded client after a write.
async fn reload(pool: &sqlx::PgPool, id: i64) -> Result<ApiClientWithLinks, ConsoleError> {
    api_client(pool, id)
        .await?
        .ok_or_else(|| ConsoleError::internal(format!("api client {id} vanished after its update")))
}

async fn render(pool: &sqlx::PgPool, id: i64) -> Result<Response, ConsoleError> {
    Ok(json_data(payload(&reload(pool, id).await?, None)))
}

fn payload(client: &ApiClientWithLinks, secret: Option<&str>) -> ApiClientPayload {
    ApiClientPayload {
        id: ApiClientModel::oid(client.client.id),
        name: client.client.name.clone(),
        scopes: client.client.scopes.clone(),
        disabled: client.client.disabled(),
        disabled_at: client.client.disabled_at,
        principals_count: client.links.len(),
        principals: client.links.iter().map(link_payload).collect(),
        created_at: client.client.created_at,
        updated_at: client.client.updated_at,
        secret: secret.map(str::to_owned),
    }
}

fn link_payload(link: &ApiClientLink) -> LinkPayload {
    LinkPayload {
        id: ApiClientPrincipalModel::oid(link.id),
        default: link.default,
        principal: principal_ref_payload(&link.principal),
    }
}

fn principal_ref_payload(principal: &ApiClientPrincipalRef) -> PrincipalRefPayload {
    PrincipalRefPayload {
        id: PrincipalModel::oid(principal.id),
        foreign_id: principal.foreign_id.clone(),
        name: principal.name.clone(),
        kind: principal.kind.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn issued_secrets_use_the_api_client_prefix() {
        assert_eq!(SECRET_PREFIX, "acs_");
        assert!(common::issue_token(SECRET_PREFIX).starts_with(SECRET_PREFIX));
    }

    /// The `details` keys a create request is rejected under, or `None` when it
    /// passes.
    fn rejected(body: serde_json::Value) -> Option<Vec<String>> {
        common::detail_keys::<CreateApiClient>(body)
    }

    #[test]
    fn scopes_are_compacted_then_checked_as_a_whole() {
        assert_eq!(
            rejected(json!({ "name": "client" })),
            Some(vec!["scopes".to_owned()])
        );
        assert_eq!(
            rejected(json!({ "name": "client", "scopes": [" ", ""] })),
            Some(vec!["scopes".to_owned()])
        );
        assert_eq!(
            rejected(json!({ "name": "client", "scopes": ["sessions:read", "bogus:scope"] })),
            Some(vec!["scopes".to_owned()])
        );
        assert_eq!(
            rejected(json!({ "name": "", "scopes": ["bogus"] })),
            Some(vec!["name".to_owned(), "scopes".to_owned()])
        );
        assert_eq!(
            rejected(json!({ "name": "client", "scopes": ["sessions:read", "sessions:write"] })),
            None
        );
        // A blank entry compacts away, so the stored list keeps the real scope.
        assert_eq!(
            rejected(json!({ "name": "client", "scopes": ["sessions:read", " "] })),
            None
        );
    }

    #[test]
    fn a_non_array_scope_list_is_a_typed_request_error() {
        assert!(
            serde_json::from_value::<CreateApiClient>(json!({ "scopes": "sessions:write" }))
                .is_err()
        );
    }

    #[test]
    fn a_patch_leaves_an_absent_name_alone_and_rejects_null() {
        let patch = |body: serde_json::Value| common::detail_keys::<UpdateApiClient>(body);
        assert_eq!(patch(json!({})), None);
        assert_eq!(
            patch(json!({ "name": null })),
            Some(vec!["name".to_owned()])
        );
        assert_eq!(
            patch(json!({ "name": "   " })),
            Some(vec!["name".to_owned()])
        );
        assert_eq!(patch(json!({ "name": "renamed" })), None);
        assert_eq!(
            patch(json!({ "scopes": [] })),
            Some(vec!["scopes".to_owned()])
        );
        assert_eq!(patch(json!({ "scopes": null })), None);
    }
}
