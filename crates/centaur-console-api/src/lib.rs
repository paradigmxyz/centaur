//! The console's HTTP application (RFC 0006, Decisions 4 and 5).
//! Read `CONVENTIONS.md` in the crate root first.
//!
//! - `routes::Routes`: what an area contributes, one axum `Router` per
//!   credential namespace;
//! - `app::router`: mounts them under their prefixes and guards;
//! - `auth`: the session layer, the per-namespace guards, and the caller
//!   extractors;
//! - [`extract`], [`error`]: typed extractors and the one error type;
//! - `pages`: server-rendered page helpers;
//! - `assets`: static files, the SPA shell, and the JSON 404;
//! - `http`: the outbound `reqwest` client;
//! - `support`: CIDR, JSON Schema, and Markdown helpers;
//! - `automation`: scheduled tasks, workflows, API clients, system settings;
//! - `misc`: skills, personal SSH key, Slack catalog, API keys, proxies,
//!   sandbox permissions;
//! - `oauth`: the MCP authorization server, broker consent flows, OAuth apps
//!   and broker credentials;
//! - `identity`: sign-in, SSO, the session, `/me`, and users (the
//!   reference area);
//! - `principals`: principals, roles, grants, effective config, Slack
//!   channel permissions, and the sandbox admin surface.

// Guards short-circuit with a full `Response` (the axum idiom); boxing them
// would only add noise to every handler.
#![allow(clippy::result_large_err)]

pub mod app;
pub mod assets;
pub mod auth;
pub mod automation;
pub mod env;
pub mod error;
pub mod extract;
pub mod hosts;
pub mod http;
pub mod identity;
pub mod misc;
pub mod oauth;
pub mod pages;
pub mod pagination;
pub mod principals;
pub mod routes;
pub mod secrets;
pub mod state;
pub mod support;
#[cfg(feature = "testing")]
pub mod testing;

pub use error::{ApiError, ApiResult, FieldErrors};
pub use routes::Routes;
pub use state::{AppState, Config};
