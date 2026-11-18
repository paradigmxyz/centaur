//! Principals, roles, role assignments, grants, Slack channel permissions,
//! effective config and grants, console-user provisioning, credential
//! reconciliation, and the sandbox admin surface (including the Slack
//! Connect allowlist).
//!
//! - `principals`, `roles`, `assignments`, `grants`, `permissions`: the
//!   endpoints, shared by the bearer, console, and sandbox-admin namespaces
//!   (the namespace guard decides who may call; [`Actor`] says who did);
//! - `payload`: JSON for lists, built with one query per association per
//!   page;
//! - `effective`: `effective_config` and `effective_grants`;
//! - [`provision`]: the principal a console user's own work runs as;
//! - [`reconcile`]: grants OAuth-flow credentials to the user principals
//!   that appear to be the same human;
//! - [`effects`]: what follows a committed principal write;
//! - `sandbox_admin`: creating principals, sandbox access, and Slack Connect
//!   teams from an admin's Slack DM.

use axum::{
    Router,
    extract::FromRequestParts,
    routing::{delete, get, patch, post, put},
};
use centaur_console_auth::Denial;
use centaur_console_db::sync_config::{bump_sync_config, role_member_ids};
use serde::{Deserialize, Deserializer, Serialize};
use sqlx::PgConnection;

use super::{
    ApiError, ApiResult, AppState, FieldErrors, Routes,
    auth::{BearerCaller, ConsoleCaller, SandboxAdminCaller},
};

mod assignments;
pub mod effective;
pub mod effects;
mod grants;
mod payload;
mod permissions;
mod principal;
pub mod provision;
pub mod reconcile;
mod role;
mod sandbox_admin;

/// The console user behind a request, whichever namespace it came through.
pub struct Actor(pub i64);

impl FromRequestParts<AppState> for Actor {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut http::request::Parts,
        _: &AppState,
    ) -> Result<Self, ApiError> {
        let ext = &parts.extensions;
        if let Some(BearerCaller(auth)) = ext.get() {
            Ok(Self(auth.user.id))
        } else if let Some(ConsoleCaller(user)) = ext.get() {
            Ok(Self(user.id))
        } else if let Some(SandboxAdminCaller(auth)) = ext.get() {
            Ok(Self(auth.admin.id))
        } else {
            Err(Denial::NAMESPACE_MISMATCH.into())
        }
    }
}

/// Who a grant, a role assignment's counterpart, or a Slack channel
/// permission belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Grantee {
    Principal(i64),
    Role(i64),
}

impl Grantee {
    pub fn column(self) -> &'static str {
        match self {
            Self::Principal(_) => "principal_id",
            Self::Role(_) => "role_id",
        }
    }

    pub fn id(self) -> i64 {
        match self {
            Self::Principal(id) | Self::Role(id) => id,
        }
    }

    /// The principals whose config depends on this grantee.
    async fn affected(self, conn: &mut PgConnection) -> ApiResult<Vec<i64>> {
        match self {
            Self::Principal(id) => Ok(vec![id]),
            Self::Role(id) => Ok(role_member_ids(conn, id).await?),
        }
    }

    /// Invalidates the config of every affected principal; call it in the
    /// writing transaction, after the last write.
    async fn bump(self, conn: &mut PgConnection) -> ApiResult<()> {
        let ids = self.affected(conn).await?;
        Ok(bump_sync_config(conn, &ids).await?)
    }
}

/// A grantable kind: its slug in payloads, grant column, table, oid
/// prefix, and label. In display order.
#[derive(Clone, Copy, Debug)]
pub struct SecretKind {
    pub kind: &'static str,
    pub column: &'static str,
    pub table: &'static str,
    pub prefix: &'static str,
    pub label: &'static str,
}

const fn kind(
    kind: &'static str,
    column: &'static str,
    table: &'static str,
    prefix: &'static str,
    label: &'static str,
) -> SecretKind {
    SecretKind {
        kind,
        column,
        table,
        prefix,
        label,
    }
}

pub const SECRET_KINDS: [SecretKind; 8] = [
    kind(
        "static",
        "static_secret_id",
        "static_secrets",
        "ssr",
        "Static",
    ),
    kind(
        "gcp_auth",
        "gcp_auth_secret_id",
        "gcp_auth_secrets",
        "gas",
        "GCP Auth",
    ),
    kind(
        "gcp_id_token",
        "gcp_id_token_secret_id",
        "gcp_id_token_secrets",
        "gid",
        "GCP ID Token",
    ),
    kind(
        "aws_auth",
        "aws_auth_secret_id",
        "aws_auth_secrets",
        "aas",
        "AWS Auth",
    ),
    kind(
        "oauth_token",
        "oauth_token_secret_id",
        "oauth_token_secrets",
        "ots",
        "OAuth Token",
    ),
    kind(
        "pg_dsn",
        "pg_dsn_secret_id",
        "pg_dsn_secrets",
        "pgs",
        "Postgres DSN",
    ),
    kind("hmac", "hmac_secret_id", "hmac_secrets", "hms", "HMAC"),
    kind(
        "ssh_key",
        "ssh_key_secret_id",
        "ssh_key_secrets",
        "sks",
        "SSH key",
    ),
];

/// Distinguishes a JSON key that is absent from one that is `null`:
/// `#[serde(default, deserialize_with = "maybe")] name: Option<Option<String>>`.
pub(crate) fn maybe<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

/// `[A-Za-z0-9-._~]+`, the characters a `foreign_id` may use in a URL.
fn url_safe(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~'))
}

/// The `foreign_id` rules shared by principals and roles: URL-safe, and
/// not shaped like an oid (`prefix_...`), which would shadow it in paths.
fn check_foreign_id(errors: &mut FieldErrors, foreign_id: Option<&str>, oid_prefix: &str) {
    let Some(foreign_id) = foreign_id else { return };
    if foreign_id.starts_with(&format!("{oid_prefix}_")) {
        errors.add(
            "foreign_id",
            format!("must not start with \"{oid_prefix}_\", which is reserved for opaque ids"),
        );
    }
    if !url_safe(foreign_id) {
        errors.add(
            "foreign_id",
            "must contain only URL-safe characters (A-Z, a-z, 0-9, -, ., _, ~)",
        );
    }
}

/// `labels[key]=value` filters from the query string (JSONB containment).
/// Values are strings; the SPA, iron-control, and the e2e tests send them.
#[derive(Default)]
pub(crate) struct LabelFilter(serde_json::Map<String, serde_json::Value>);

impl LabelFilter {
    pub fn from_pairs(pairs: Vec<(String, String)>) -> Self {
        Self(
            pairs
                .into_iter()
                .filter_map(|(key, value)| {
                    let label = key.strip_prefix("labels[")?.strip_suffix(']')?;
                    Some((label.to_owned(), serde_json::Value::String(value)))
                })
                .collect(),
        )
    }

    fn as_json(&self) -> Option<serde_json::Value> {
        (!self.0.is_empty()).then(|| serde_json::Value::Object(self.0.clone()))
    }
}

/// `%`-wrapped, LIKE-escaped search pattern for `q`.
fn like_pattern(query: &str) -> Option<String> {
    let query = query.trim();
    if query.is_empty() {
        return None;
    }
    let mut pattern = String::from("%");
    for c in query.chars() {
        if matches!(c, '\\' | '%' | '_') {
            pattern.push('\\');
        }
        pattern.push(c);
    }
    pattern.push('%');
    Some(pattern)
}

#[derive(Serialize)]
struct Ref {
    id: String,
    foreign_id: Option<String>,
    name: Option<String>,
}

pub fn routes() -> Routes {
    use {assignments as asg, grants as grt, permissions as perm, principal as prn, role as rol};
    let bearer = Router::new()
        .route("/roles", get(rol::index).post(rol::create))
        // `lookup/default` is a compatibility alias of `lookup`.
        .route("/roles/lookup/default/{foreign_id}", get(rol::lookup))
        .route("/roles/lookup/{foreign_id}", get(rol::lookup))
        .route(
            "/roles/{id}",
            get(rol::show)
                .patch(rol::upsert)
                .put(rol::upsert)
                .delete(rol::destroy),
        )
        .route(
            "/roles/{id}/slack_channel_permissions",
            post(perm::role_upsert),
        )
        .route("/roles/{id}/grants", get(grt::role_index))
        .route("/principals", get(prn::index).post(prn::create))
        .route(
            "/principals/lookup/default/{foreign_id}/effective_config",
            get(effective::lookup_config),
        )
        .route(
            "/principals/lookup/{foreign_id}/effective_config",
            get(effective::lookup_config),
        )
        .route("/principals/lookup/default/{foreign_id}", get(prn::lookup))
        .route("/principals/lookup/{foreign_id}", get(prn::lookup))
        .route(
            "/principals/{id}",
            get(prn::show).patch(prn::upsert).put(prn::upsert),
        )
        .route("/principals/{id}/effective_config", get(effective::config))
        .route(
            "/principals/{id}/slack_channel_permissions",
            post(perm::principal_upsert),
        )
        .route("/principals/{id}/roles", get(asg::index).post(asg::create))
        .route("/principals/{id}/roles/{role_id}", delete(asg::destroy))
        .route("/principals/{id}/grants", get(grt::principal_index))
        .route("/grants", post(grt::create))
        .route("/grants/{id}", get(grt::show).delete(grt::destroy));

    let console = Router::new()
        .route("/grants", post(grt::create))
        .route("/grants/{id}", get(grt::show).delete(grt::destroy))
        .route("/principals", get(prn::index).post(prn::create))
        .route(
            "/principals/{id}",
            get(prn::show)
                .patch(prn::upsert)
                .put(prn::upsert)
                .delete(prn::destroy),
        )
        .route("/principals/{id}/effective_grants", get(effective::grants))
        .route(
            "/principals/{id}/slack_channel_permissions",
            put(perm::principal_replace),
        )
        .route(
            "/principals/{id}/slack_channel_permissions/{channel_id}",
            delete(perm::principal_destroy),
        )
        .route("/principals/{id}/roles", get(asg::index).post(asg::create))
        .route("/principals/{id}/roles/{role_id}", delete(asg::destroy))
        .route("/principals/{id}/grants", get(grt::principal_index))
        .route("/roles", get(rol::index).post(rol::create))
        .route(
            "/roles/{id}",
            get(rol::show)
                .patch(rol::upsert)
                .put(rol::upsert)
                .delete(rol::destroy),
        )
        .route(
            "/roles/{id}/slack_channel_permissions",
            put(perm::role_replace),
        )
        .route(
            "/roles/{id}/slack_channel_permissions/{channel_id}",
            delete(perm::role_destroy),
        )
        .route("/roles/{id}/grants", get(grt::role_index));

    let sandbox_admin = Router::new()
        .route(
            "/principals",
            get(sandbox_admin::principals).post(sandbox_admin::create_principal),
        )
        .route("/principals/lookup/{foreign_id}", get(prn::lookup))
        .route("/principals/{id}", get(prn::show))
        .route("/principals/{id}/effective_grants", get(effective::grants))
        .route(
            "/principals/{id}/sandbox_access",
            patch(sandbox_admin::sandbox_access),
        )
        .route(
            "/principals/{id}/slack_channel_permissions",
            post(perm::principal_upsert),
        )
        .route(
            "/principals/{id}/slack_channel_permissions/{channel_id}",
            delete(perm::principal_destroy),
        )
        .route("/principals/{id}/roles", get(asg::index).post(asg::create))
        .route("/principals/{id}/roles/{role_id}", delete(asg::destroy))
        .route("/principals/{id}/grants", get(grt::principal_index))
        .route("/grants", post(grt::create))
        .route("/grants/{id}", delete(grt::destroy))
        .route(
            "/slack_external_teams",
            get(sandbox_admin::teams).post(sandbox_admin::add_team),
        )
        .route(
            "/slack_external_teams/{team_id}",
            delete(sandbox_admin::remove_team),
        )
        .route("/roles", get(rol::index))
        .route("/roles/lookup/{foreign_id}", get(rol::lookup))
        .route("/roles/{id}", get(rol::show))
        .route("/roles/{id}/grants", get(grt::role_index));

    Routes {
        bearer,
        console,
        sandbox_admin,
        ..Routes::default()
    }
}
