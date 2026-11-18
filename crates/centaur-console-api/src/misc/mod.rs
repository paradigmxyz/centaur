//! Skills, the caller's personal SSH key, the Slack channel catalog and
//! picker, bearer API keys and proxies, and what a sandbox may do.
//!
//! - `skills`, `sandbox_skills`: skill authoring (console) and the sandbox
//!   skill catalog;
//! - `personal_key`: `/api/v1/console/me/ssh_key`;
//! - `slack`, `slack_channels`: the catalog jobs and the console picker;
//! - `credentials`: `/api/v1/api_keys` and `/api/v1/proxies`;
//! - `sandbox`: `/api/v1/sandbox/permissions` and `/oauth_apps`.

use axum::{
    Router,
    routing::{get, post},
};

use super::{routes::Routes, state::AppState};

pub mod credentials;
pub mod personal_key;
pub mod sandbox;
pub mod sandbox_skills;
pub mod skills;
pub mod slack;
pub mod slack_channels;

pub fn routes() -> Routes {
    Routes {
        bearer: Router::<AppState>::new()
            .route(
                "/api_keys",
                get(credentials::api_keys_index).post(credentials::api_keys_create),
            )
            .route(
                "/api_keys/{id}",
                get(credentials::api_keys_show).delete(credentials::api_keys_destroy),
            )
            .route(
                "/proxies",
                get(credentials::proxies_index).post(credentials::proxies_create),
            )
            .route(
                "/proxies/{id}",
                get(credentials::proxies_show)
                    .patch(credentials::proxies_update)
                    .put(credentials::proxies_update)
                    .delete(credentials::proxies_destroy),
            ),
        console: Router::new().route("/slack_channels", get(slack_channels::index)),
        console_active: Router::new()
            .route("/skills/editor_candidates", get(skills::editor_candidates))
            .route("/skills/{id}/share", post(skills::share))
            .route("/skills/{id}/unshare", post(skills::unshare))
            .route("/skills", get(skills::index).post(skills::create_skill))
            .route(
                "/skills/{id}",
                get(skills::show)
                    .patch(skills::update_skill)
                    .put(skills::update_skill)
                    .delete(skills::destroy),
            )
            .route("/me/ssh_key/validate", post(personal_key::validate))
            .route(
                "/me/ssh_key",
                get(personal_key::show)
                    .patch(personal_key::save)
                    .put(personal_key::save)
                    .delete(personal_key::destroy),
            ),
        sandbox: Router::new()
            .route("/permissions", get(sandbox::permissions))
            .route("/skills/search", get(sandbox_skills::search))
            .route("/skills/{id}/share", post(sandbox_skills::share))
            .route("/skills/{id}/unshare", post(sandbox_skills::unshare))
            .route(
                "/skills/{id}/editors",
                get(sandbox_skills::list_editors)
                    .post(sandbox_skills::add_editor)
                    .delete(sandbox_skills::remove_editor),
            )
            .route(
                "/skills",
                get(sandbox_skills::index).post(sandbox_skills::create),
            )
            .route(
                "/skills/{id}",
                get(sandbox_skills::show)
                    .patch(sandbox_skills::update)
                    .put(sandbox_skills::update)
                    .delete(sandbox_skills::destroy),
            ),
        sandbox_claims: Router::new().route("/oauth_apps", get(sandbox::oauth_apps)),
        ..Routes::default()
    }
}
