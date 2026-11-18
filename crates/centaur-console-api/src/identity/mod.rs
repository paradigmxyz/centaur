//! Identity: sign-in (password and SSO), sign-out, the pending-approval
//! page, the PWA files and launch handler, the console session (`/me`), and
//! user administration.
//!
//! - `pages`: `/login`, `/logout`, `/pending`, `/launch`;
//! - `sso`: `/auth/{provider}/start|callback`;
//! - `console`: `/api/v1/console/{me,session,users}`;
//! - `pwa`: `/manifest`, `/service-worker`;
//! - `settings`: SSO provider and password-login settings from the
//!   environment;
//! - `provision`: linking an SSO identity to a user.

use axum::{
    Router,
    routing::{delete, get, post},
};

use super::{routes::Routes, state::AppState};

pub mod console;
pub mod pages;
pub mod provision;
pub mod pwa;
pub mod settings;
pub mod sso;

/// Where a signed-in user starts.
pub fn landing_path(admin: bool) -> &'static str {
    if admin {
        "/console/principals"
    } else {
        "/console/integrations"
    }
}

/// Sign-in lives outside `/api`, so its handlers authenticate themselves.
pub fn routes() -> Routes {
    Routes {
        public: Router::<AppState>::new()
            .route("/login", get(pages::login_form).post(pages::login))
            .route("/logout", delete(pages::logout).post(pages::logout))
            .route("/pending", get(pages::pending))
            .route("/launch", get(pages::launch))
            .route("/manifest", get(pwa::manifest))
            .route("/service-worker", get(pwa::service_worker))
            .route("/auth/{provider}/start", get(sso::start))
            .route("/auth/{provider}/callback", get(sso::callback)),
        console_pending: Router::new()
            .route("/me", get(console::me))
            .route("/session", delete(console::sign_out)),
        console: Router::new()
            .route("/users", get(console::list_users))
            .route("/users/{id}/approve", post(console::approve))
            .route("/users/{id}/disable", post(console::disable))
            .route("/users/{id}/promote", post(console::promote))
            .route("/users/{id}/demote", post(console::demote)),
        ..Routes::default()
    }
}
