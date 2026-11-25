//! Bearer `/api/v1/proxies`: the per-sandbox credentials agents receive.
//!
//! A proxy may boot unassigned and be assigned, swapped, or cleared later, so
//! `principal_id` is mutable and `principal_id: null` unassigns. The plaintext
//! token is returned once, by `create`.

use std::collections::HashMap;

use axum::{
    extract::{Path, Query, State},
    response::Response,
    routing::get,
};
use centaur_console_db::areas::admin::{
    PROXY_LIST_FROM, PROXY_LIST_ORDER, PROXY_SELECT, ProxyChange, ProxyRow, admin_proxy,
    create_proxy, delete_proxy, update_proxy,
};
use centaur_console_db::find_principal;
use centaur_console_db::model::{ConsoleModel, Principal as PrincipalModel, Proxy as ProxyModel};
use centaur_console_db::validation::{ValidateFields, rules};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::types::Json;
use time::PrimitiveDateTime;

use super::common::{self, json_ok, nullable_field};
use crate::{
    AppState,
    console::{
        guard::{BearerUser, GuardedRoutes},
        http::{
            ConsoleError, ListParams, ListSource, Pagination, bind, created_data, json_data,
            label_filters, labels_value, list, no_content,
        },
        request::ConsoleData,
    },
};

const TOKEN_PREFIX: &str = "iprx_";
const LABEL_VALUE_MESSAGE: &str = "values must be strings";
const LABEL_MAP_MESSAGE: &str = "must be a hash";

/// The index filter: `principal_id` and `labels @> $2`.
const PROXY_LIST: ListSource = ListSource::new(PROXY_SELECT, PROXY_LIST_FROM, PROXY_LIST_ORDER, 2);

/// A new proxy: its name, optional principal assignment, and labels.
#[derive(Deserialize, garde::Validate)]
struct CreateProxy {
    #[serde(default)]
    #[garde(custom(rules::present_not_blank))]
    name: Option<String>,
    #[serde(default)]
    #[garde(dive)]
    labels: Option<Labels>,
    /// References the handler resolves, not rules of the record.
    #[serde(default)]
    #[garde(skip)]
    principal_id: Option<String>,
    #[serde(default)]
    #[garde(skip)]
    requester_principal_id: Option<String>,
}

/// A PATCH: an absent key leaves its column alone, while an explicit `null`
/// clears a principal, clears the labels, or fails the name's presence
/// validation.
#[derive(Deserialize, garde::Validate)]
struct UpdateProxy {
    #[serde(default, deserialize_with = "nullable_field")]
    #[garde(custom(rules::patch_not_blank))]
    name: Option<Option<String>>,
    #[serde(default, deserialize_with = "nullable_field")]
    #[garde(dive)]
    labels: Option<Option<Labels>>,
    #[serde(default, deserialize_with = "nullable_field")]
    #[garde(skip)]
    principal_id: Option<Option<String>>,
    #[serde(default, deserialize_with = "nullable_field")]
    #[garde(skip)]
    requester_principal_id: Option<Option<String>>,
}

/// `labels` as the model's `labels_are_string_map` sees it: a JSON object whose
/// values are all strings, one message per value that is not. A field rule
/// cannot report several errors for one field, so the check is this type's own
/// [`garde::Validate`] implementation. A JSON object's keys are always strings,
/// so the model's `keys must be strings` branch is unreachable here.
#[derive(Clone, Debug, Deserialize)]
#[serde(transparent)]
struct Labels(Value);

impl Labels {
    /// The stored column: an explicit `null` is the empty map.
    fn into_value(self) -> Value {
        match self.0 {
            Value::Null => json!({}),
            labels => labels,
        }
    }
}

impl garde::Validate for Labels {
    type Context = ();

    fn validate_into(
        &self,
        _context: &Self::Context,
        parent: &mut dyn FnMut() -> garde::Path,
        report: &mut garde::Report,
    ) {
        match &self.0 {
            Value::Object(entries) => {
                for _ in entries.values().filter(|value| !value.is_string()) {
                    report.append(parent(), garde::Error::new(LABEL_VALUE_MESSAGE));
                }
            }
            _ => report.append(parent(), garde::Error::new(LABEL_MAP_MESSAGE)),
        }
    }
}

/// A proxy as Console renders it; `token` is present only in the create
/// response.
#[derive(Serialize)]
struct ProxyPayload {
    id: String,
    name: String,
    principal_id: Option<String>,
    requester_principal_id: Option<String>,
    status: &'static str,
    labels: Value,
    #[serde(with = "centaur_console_db::timestamp::primitive_option")]
    principal_assigned_at: Option<PrimitiveDateTime>,
    #[serde(with = "centaur_console_db::timestamp::primitive_option")]
    requester_principal_assigned_at: Option<PrimitiveDateTime>,
    #[serde(with = "centaur_console_db::timestamp::primitive")]
    created_at: PrimitiveDateTime,
    #[serde(with = "centaur_console_db::timestamp::primitive")]
    updated_at: PrimitiveDateTime,
    #[serde(skip_serializing_if = "Option::is_none")]
    token: Option<String>,
}

pub(super) fn bearer(routes: GuardedRoutes) -> GuardedRoutes {
    routes
        .operator("/proxies", get(index).post(create))
        .operator("/proxies/{id}", get(show).patch(update).delete(destroy))
}

async fn index(
    State(state): State<AppState>,
    _user: BearerUser,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Response, ConsoleError> {
    // Console reads `principal_id`, then `labels`, then pagination, so each
    // error wins in that order.
    let pool = common::pool(&state)?;
    let principal_id = match query.get("principal_id").map(String::as_str) {
        Some(reference) if !reference.trim().is_empty() => {
            Some(resolve_principal(&pool, reference).await?)
        }
        _ => None,
    };
    let labels = label_filters(&query)?;
    let params = ListParams {
        pagination: Pagination::from_query(&query)?,
        labels,
    };
    let labels_json =
        (!params.labels.is_empty()).then(|| Json(Value::Object(labels_value(&params.labels))));
    let body = list::<ProxyRow, ProxyPayload, _, _>(
        &pool,
        &params,
        PROXY_LIST,
        |arguments| {
            bind(arguments, principal_id)?;
            bind(arguments, labels_json.clone())
        },
        payload,
    )
    .await?;
    Ok(json_ok(body))
}

async fn show(
    State(state): State<AppState>,
    Path(id): Path<String>,
    _user: BearerUser,
) -> Result<Response, ConsoleError> {
    let pool = common::pool(&state)?;
    let proxy = load(&pool, &id).await?;
    Ok(json_data(payload(&proxy)))
}

async fn create(
    State(state): State<AppState>,
    _user: BearerUser,
    ConsoleData(data): ConsoleData<CreateProxy>,
) -> Result<Response, ConsoleError> {
    let pool = common::pool(&state)?;
    let principal_id = match principal_reference(data.principal_id.as_deref()) {
        Some(reference) => Some(resolve_principal(&pool, reference).await?),
        None => None,
    };
    let requester_principal_id = match principal_reference(data.requester_principal_id.as_deref()) {
        Some(reference) => Some(resolve_principal(&pool, reference).await?),
        None => None,
    };
    data.validate_fields()?;
    let labels = data
        .labels
        .map(Labels::into_value)
        .unwrap_or_else(|| json!({}));
    let token = common::issue_token(TOKEN_PREFIX);
    let proxy = create_proxy(
        &pool,
        data.name.as_deref().unwrap_or_default(),
        &labels,
        &common::token_digest(&token),
        principal_id,
        requester_principal_id,
    )
    .await?;
    // The freshly minted token is the only field the listing never renders.
    let mut data = payload(&proxy);
    data.token = Some(token);
    Ok(created_data(data))
}

async fn update(
    State(state): State<AppState>,
    Path(id): Path<String>,
    _user: BearerUser,
    ConsoleData(data): ConsoleData<UpdateProxy>,
) -> Result<Response, ConsoleError> {
    let pool = common::pool(&state)?;
    let proxy = load(&pool, &id).await?;
    let mut change = ProxyChange::default();
    if let Some(reference) = &data.principal_id {
        change.principal_id = Some(match principal_reference(reference.as_deref()) {
            Some(reference) => Some(resolve_principal(&pool, reference).await?),
            None => None,
        });
    }
    if let Some(reference) = &data.requester_principal_id {
        change.requester_principal_id = Some(match principal_reference(reference.as_deref()) {
            Some(reference) => Some(resolve_principal(&pool, reference).await?),
            None => None,
        });
    }
    data.validate_fields()?;
    if let Some(labels) = &data.labels {
        // An explicit null clears the labels.
        change.labels = Some(
            labels
                .clone()
                .map(Labels::into_value)
                .unwrap_or_else(|| json!({})),
        );
    }
    change.name = data.name.flatten();
    let proxy = update_proxy(&pool, proxy.id, &change)
        .await?
        .ok_or_else(|| ConsoleError::internal("proxy vanished after its update"))?;
    Ok(json_data(payload(&proxy)))
}

async fn destroy(
    State(state): State<AppState>,
    Path(id): Path<String>,
    _user: BearerUser,
) -> Result<Response, ConsoleError> {
    let pool = common::pool(&state)?;
    let proxy = load(&pool, &id).await?;
    delete_proxy(&pool, proxy.id).await?;
    Ok(no_content())
}

async fn load(pool: &sqlx::PgPool, oid: &str) -> Result<ProxyRow, ConsoleError> {
    Ok(admin_proxy(pool, common::oid::<ProxyModel>(oid)?)
        .await?
        .ok_or_else(ProxyModel::not_found)?)
}

/// Resolve a principal reference: an unknown or malformed one is a 404.
async fn resolve_principal(pool: &sqlx::PgPool, reference: &str) -> Result<i64, ConsoleError> {
    Ok(
        find_principal(pool, common::oid::<PrincipalModel>(reference)?)
            .await?
            .map(|principal| principal.id)
            .ok_or_else(PrincipalModel::not_found)?,
    )
}

/// Only a non-blank reference names a principal. The reference itself is
/// passed through unmasked.
fn principal_reference(reference: Option<&str>) -> Option<&str> {
    reference.filter(|reference| !reference.trim().is_empty())
}

fn payload(proxy: &ProxyRow) -> ProxyPayload {
    ProxyPayload {
        id: ProxyModel::oid(proxy.id),
        name: proxy.name.clone(),
        principal_id: proxy.principal_id.map(PrincipalModel::oid),
        requester_principal_id: proxy.requester_principal_id.map(PrincipalModel::oid),
        status: if proxy.principal_id.is_some() {
            "assigned"
        } else {
            "unassigned"
        },
        labels: proxy.labels.clone(),
        principal_assigned_at: proxy.principal_assigned_at,
        requester_principal_assigned_at: proxy.requester_principal_assigned_at,
        created_at: proxy.created_at,
        updated_at: proxy.updated_at,
        token: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issued_tokens_use_the_proxy_prefix() {
        assert_eq!(TOKEN_PREFIX, "iprx_");
        assert!(common::issue_token(TOKEN_PREFIX).starts_with(TOKEN_PREFIX));
    }

    /// The `details` keys a create request is rejected under, or `None` when it
    /// passes.
    fn create_rejections(body: Value) -> Option<Vec<String>> {
        common::detail_keys::<CreateProxy>(body)
    }

    fn update_rejections(body: Value) -> Option<Vec<String>> {
        common::detail_keys::<UpdateProxy>(body)
    }

    #[test]
    fn labels_are_a_string_map() {
        for labels in [json!("team=diff"), json!({ "team": 5 })] {
            assert_eq!(
                create_rejections(json!({ "name": "proxy", "labels": labels })),
                Some(vec!["labels".to_owned()]),
                "{labels}"
            );
        }
        assert_eq!(
            create_rejections(json!({ "name": "proxy", "labels": { "team": "diff" } })),
            None
        );
        // A missing or explicit-null map is the empty one.
        assert_eq!(create_rejections(json!({ "name": "proxy" })), None);
        assert_eq!(
            create_rejections(json!({ "name": "proxy", "labels": null })),
            None
        );
    }

    #[test]
    fn a_patch_leaves_absent_keys_alone_and_clears_an_explicit_null() {
        assert_eq!(update_rejections(json!({})), None);
        assert_eq!(
            update_rejections(json!({ "name": null })),
            Some(vec!["name".to_owned()])
        );
        assert_eq!(
            update_rejections(json!({ "name": "  " })),
            Some(vec!["name".to_owned()])
        );
        assert_eq!(
            update_rejections(json!({ "labels": null })),
            None,
            "an explicit null clears the labels"
        );
        assert_eq!(
            update_rejections(json!({ "labels": { "team": 5 } })),
            Some(vec!["labels".to_owned()])
        );
        assert_eq!(
            update_rejections(json!({ "name": "", "labels": { "team": 5 }, "principal_id": null })),
            Some(vec!["labels".to_owned(), "name".to_owned()])
        );
    }

    /// The labels column a PATCH asks for; absent leaves the stored map alone.
    fn patched_labels(body: Value) -> Option<Value> {
        let update: UpdateProxy = serde_json::from_value(body).expect("a typed patch");
        update.labels.as_ref().map(|labels| {
            labels
                .clone()
                .map(Labels::into_value)
                .unwrap_or_else(|| json!({}))
        })
    }

    #[test]
    fn a_patch_clears_labels_on_an_explicit_null() {
        assert_eq!(patched_labels(json!({})), None);
        assert_eq!(patched_labels(json!({ "labels": null })), Some(json!({})));
        assert_eq!(
            patched_labels(json!({ "labels": { "team": "diff" } })),
            Some(json!({ "team": "diff" }))
        );
    }
}
