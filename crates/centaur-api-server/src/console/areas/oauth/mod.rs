#![allow(clippy::result_large_err)] // handlers propagate `Response` as their error

//! Console area: OAuth apps, broker credentials, integrations, providers, and
//! the server-rendered consent flow.
//!
//! The record routes use the shared abstractions: typed payload structs, the
//! model descriptors' lookups and 404s, `console::http::list`, `FieldErrors`,
//! and `ConsoleError` with `?`.

mod apps;
mod credentials;
mod enrich;
mod flow;
mod http;
mod providers;
#[cfg(test)]
mod tests_db;

/// The sorted attribute keys of a validation result, for tests.
#[cfg(test)]
pub(super) fn error_keys(errors: centaur_console_db::FieldErrors) -> Vec<String> {
    let mut keys: Vec<String> = errors
        .entries()
        .map(|(attribute, _)| attribute.to_owned())
        .collect();
    keys.sort();
    keys
}

/// Tests replace Slack's API origin with a loopback mock.
#[cfg(test)]
pub(super) static SLACK_API_BASE: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Slack's API endpoint for `path`, overridable only by the crate's tests.
pub(super) fn slack_api_endpoint(path: &str) -> String {
    #[cfg(test)]
    if let Some(base) = SLACK_API_BASE.get() {
        return format!("{}/{}", base.trim_end_matches('/'), path);
    }
    format!("https://slack.com/api/{path}")
}

use axum::{
    Json, Router,
    http::StatusCode,
    routing::{delete, get, post},
};
use centaur_console_db::{
    areas::oauth::{BrokerCredential, OauthApp},
    model::{
        self, BrokerCredential as BrokerCredentialModel, ConsoleModel, OauthApp as OauthAppModel,
    },
};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use sqlx::PgPool;

use crate::{
    AppState,
    console::{
        guard::GuardedRoutes,
        http::{ConsoleError, DataPayload, ListParams, labels_value},
    },
    console_session::ConsoleSessionConfig,
};

/// Routes under `/api/v1/console` (paths relative to that prefix).
pub(crate) fn console(routes: GuardedRoutes) -> GuardedRoutes {
    routes
        .console_admin(
            "/oauth_apps",
            get(apps::console_index).post(apps::console_create),
        )
        .console_admin(
            "/oauth_apps/{id}",
            get(apps::console_show)
                .patch(apps::console_update)
                .delete(apps::destroy),
        )
        .console_admin(
            "/oauth_apps/{id}/credentials",
            get(apps::console_credentials),
        )
        .console_admin(
            "/oauth_apps/{id}/provider_details",
            get(apps::console_provider_details),
        )
        .console_admin("/oauth_apps/presets/{provider}", post(apps::console_preset))
        .console_admin("/oauth_providers", get(apps::console_providers))
        .console_user("/integrations", get(apps::console_integrations))
        .console_user(
            "/integrations/connections/{id}",
            delete(apps::console_disconnect),
        )
        .console_admin(
            "/broker_credentials",
            get(credentials::console_index).post(credentials::console_create),
        )
        .console_admin(
            "/broker_credentials/{id}",
            get(credentials::console_show)
                .patch(credentials::console_update)
                .delete(credentials::destroy),
        )
}

/// Routes under `/api/v1` (bearer API; paths relative to that prefix).
pub(crate) fn bearer(routes: GuardedRoutes) -> GuardedRoutes {
    routes
        .operator(
            "/oauth_apps",
            get(apps::bearer_index).post(apps::bearer_create),
        )
        .operator("/oauth_apps/lookup/{slug}", get(apps::bearer_lookup))
        .operator(
            "/oauth_apps/{id}",
            get(apps::bearer_show)
                .put(apps::bearer_update)
                .delete(apps::destroy),
        )
        .operator(
            "/broker_credentials",
            get(credentials::bearer_index).post(credentials::bearer_create),
        )
        .operator(
            "/broker_credentials/lookup/{foreign_id}",
            get(credentials::bearer_lookup),
        )
        .operator(
            "/broker_credentials/{id}",
            get(credentials::bearer_show)
                .put(credentials::bearer_update)
                .delete(credentials::destroy),
        )
}

/// Routes under `/api/v1/sandbox` (paths relative to that prefix).
pub(crate) fn sandbox(routes: GuardedRoutes) -> GuardedRoutes {
    // The single sandbox token now performs the same live proxy binding check
    // on every sandbox route, including this one.
    routes.sandbox(
        "/oauth_apps",
        get(apps::sandbox_index),
        crate::auth::Capability::SandboxOauthApps,
    )
}

/// Server-rendered pages and other top-level routes (absolute paths).
pub(crate) fn pages(router: Router<AppState>) -> Router<AppState> {
    router
        .route("/oauth/{slug}/start", get(flow::start))
        .route("/oauth/{slug}/callback", get(flow::callback))
}

/// An empty or whitespace-only string.
pub(super) fn blank(value: &str) -> bool {
    value.trim().is_empty()
}

/// The label containment value a list query binds, or `None` for no filter.
pub(super) fn label_filter(params: &ListParams) -> Option<sqlx::types::Json<Value>> {
    if params.labels.is_empty() {
        return None;
    }
    Some(sqlx::types::Json(Value::Object(labels_value(
        &params.labels,
    ))))
}

/// garde rule for a required `String` field: rejects an empty value and one of
/// only whitespace.
pub(super) fn present(value: &str, _: &()) -> garde::Result {
    if blank(value) {
        Err(garde::Error::new("can't be blank"))
    } else {
        Ok(())
    }
}

/// One body field, distinguishing an absent key from an explicit JSON null.
///
/// `null` clears a nullable scalar and is ignored for a hash/array filter, the
/// same two cases; a value of the wrong type
/// is a validation error from the typed extractor instead of a silent cast.
#[derive(Clone, Debug, Default)]
pub(super) enum Patch<T> {
    #[default]
    Missing,
    Null,
    Value(T),
}

impl<'de, T> Deserialize<'de> for Patch<T>
where
    T: Deserialize<'de>,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Ok(match Option::<T>::deserialize(deserializer)? {
            None => Self::Null,
            Some(value) => Self::Value(value),
        })
    }
}

impl<T> Patch<T> {
    pub(super) fn into_option(self) -> Option<T> {
        match self {
            Self::Value(value) => Some(value),
            Self::Missing | Self::Null => None,
        }
    }
}

/// Whether a path segment is `model`'s opaque-id spelling (`oap_...`).
///
/// The upsert routes treat that spelling as a lookup that must resolve, not as a
/// `foreign_id`/slug to create, so they branch on it before falling back.
pub(super) fn is_oid<M: ConsoleModel>(identifier: &str) -> bool {
    identifier
        .strip_prefix(M::OID_PREFIX)
        .is_some_and(|rest| rest.starts_with('_'))
}

/// The writable Console pool. The console layer is mounted only with a Console
/// database, so a failure here is a deployment fault; it keeps its 503.
pub(super) fn pool(state: &AppState) -> Result<PgPool, ConsoleError> {
    state
        .console_session_db()
        .map_err(|error| ConsoleError::new(StatusCode::SERVICE_UNAVAILABLE, error.to_string()))
}

/// Console's document envelope around one record payload.
pub(super) fn document<T: Serialize>(payload: T) -> Json<DataPayload<T>> {
    Json(DataPayload { data: payload })
}

/// Console's 201 for a document that a write created.
pub(super) fn created<T: Serialize>(payload: T) -> (StatusCode, Json<DataPayload<T>>) {
    (StatusCode::CREATED, document(payload))
}

/// The app whose row id is `id`, or the descriptor's 404.
pub(super) async fn app_by_id(pool: &PgPool, id: i64) -> Result<OauthApp, ConsoleError> {
    Ok(model::find_by_id::<OauthAppModel, OauthApp, _>(pool, id)
        .await?
        .ok_or_else(OauthAppModel::not_found)?)
}

/// The app a path segment's opaque id names, or the descriptor's 404.
pub(super) async fn app_by_oid(pool: &PgPool, oid: &str) -> Result<OauthApp, ConsoleError> {
    Ok(model::find_by_oid::<OauthAppModel, OauthApp, _>(pool, oid)
        .await?
        .ok_or_else(OauthAppModel::not_found)?)
}

/// The credential a path segment's opaque id names, or the descriptor's 404.
pub(super) async fn credential_by_oid(
    pool: &PgPool,
    oid: &str,
) -> Result<BrokerCredential, ConsoleError> {
    Ok(
        model::find_by_oid::<BrokerCredentialModel, BrokerCredential, _>(pool, oid)
            .await?
            .ok_or_else(BrokerCredentialModel::not_found)?,
    )
}

pub(super) fn oauth_callback_redirect_uri(config: &ConsoleSessionConfig, slug: &str) -> String {
    format!(
        "{}/oauth/{slug}/callback",
        super::auth_pages::public_base_url(config)
    )
}

pub(super) fn oauth_start_url(config: &ConsoleSessionConfig, slug: &str) -> String {
    format!(
        "{}/oauth/{slug}/start",
        super::auth_pages::public_base_url(config)
    )
}

/// The deployment is missing Console's encryption keys; every write of an
/// encrypted attribute fails rather than storing plaintext.
pub(super) fn config_failure() -> ConsoleError {
    ConsoleError::internal("console encryption keys are not configured")
}
