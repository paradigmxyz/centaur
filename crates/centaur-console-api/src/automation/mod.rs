//! Automation: scheduled tasks (console, sandbox, bearer, and the runner's
//! claim/result endpoints), the workflow dashboard, API clients, and system
//! settings.
//!
//! - `tasks`: the stored task, validation, and targeted writes;
//! - `scheduled_tasks`: the console, sandbox, and bearer endpoints;
//! - `runner`: `/internal/v1/scheduled_tasks/{id}/claim|result` for api-rs;
//! - `delivery`: where a user's task output may go;
//! - `workflows`, `workflow_runs`: the dashboard over api-rs's read-only view;
//! - `api_clients`, `system_settings`: admin pages;
//! - `centaur_api`: the console's calls to api-rs.

use axum::{
    Router,
    http::{HeaderValue, header::CACHE_CONTROL},
    routing::{get, post},
};
use tower_http::set_header::SetResponseHeaderLayer;

use super::{routes::Routes, state::AppState};

pub mod api_clients;
pub mod centaur_api;
pub mod delivery;
pub mod runner;
pub mod scheduled_tasks;
pub mod system_settings;
pub mod tasks;
pub mod workflow_runs;
pub mod workflows;

/// Task, workflow, and settings responses are never cached.
fn no_store(router: Router<AppState>) -> Router<AppState> {
    router.layer(SetResponseHeaderLayer::overriding(
        CACHE_CONTROL,
        HeaderValue::from_static("no-store"),
    ))
}

/// `api_clients` mounts the API client admin routes (they exist only while
/// `CENTAUR_CONSOLE_API_CLIENTS_ENABLED` is on).
pub fn routes(api_clients_enabled: bool) -> Routes {
    use scheduled_tasks as st;
    let mut console = Router::new()
        .route(
            "/system_settings",
            get(system_settings::show)
                .patch(system_settings::update)
                .put(system_settings::update),
        )
        .route(
            "/system_settings/slack_external_teams",
            get(system_settings::slack_external_teams)
                .put(system_settings::replace_slack_external_teams),
        )
        .route("/workflows", get(workflows::index))
        .route("/workflows/{id}", get(workflows::show))
        .route(
            "/workflows/{id}/runs",
            get(workflows::list_runs).post(workflows::create_run),
        );
    if api_clients_enabled {
        console = console
            .route(
                "/api_clients",
                get(api_clients::index).post(api_clients::create),
            )
            .route(
                "/api_clients/{id}",
                get(api_clients::show)
                    .patch(api_clients::update)
                    .put(api_clients::update)
                    .delete(api_clients::destroy),
            )
            .route(
                "/api_clients/{id}/rotate_secret",
                post(api_clients::rotate_secret),
            )
            .route("/api_clients/{id}/disable", post(api_clients::disable))
            .route("/api_clients/{id}/enable", post(api_clients::enable))
            .route(
                "/api_clients/{id}/principals",
                post(api_clients::attach_principal),
            )
            .route(
                "/api_clients/{id}/principals/{link_id}",
                axum::routing::delete(api_clients::detach_principal),
            )
            .route(
                "/api_clients/{id}/principals/{link_id}/default",
                post(api_clients::make_default_principal),
            );
    }
    Routes {
        console: no_store(console),
        console_active: no_store(
            Router::new()
                .route(
                    "/scheduled_tasks",
                    get(st::console_index).post(st::console_create),
                )
                .route("/scheduled_tasks/new", get(st::console_new))
                .route(
                    "/scheduled_tasks/slack_channel_options",
                    get(st::console_channel_options),
                )
                .route("/scheduled_tasks/{id}/edit", get(st::console_edit_form))
                .route("/scheduled_tasks/{id}/run", post(st::console_run))
                .route(
                    "/scheduled_tasks/{id}",
                    axum::routing::patch(st::console_update)
                        .put(st::console_update)
                        .delete(st::console_destroy),
                ),
        ),
        bearer: no_store(Router::new().route("/scheduled_tasks/{id}", get(st::bearer_show))),
        sandbox: no_store(
            Router::new()
                .route(
                    "/scheduled_tasks",
                    get(st::sandbox_index).post(st::sandbox_create),
                )
                .route("/scheduled_tasks/{id}/run", post(st::sandbox_run))
                .route(
                    "/scheduled_tasks/{id}",
                    get(st::sandbox_show)
                        .patch(st::sandbox_update)
                        .put(st::sandbox_update)
                        .delete(st::sandbox_destroy),
                ),
        ),
        internal: Router::new()
            .route("/v1/scheduled_tasks/{id}/claim", post(runner::claim))
            .route("/v1/scheduled_tasks/{id}/result", post(runner::result)),
        ..Routes::default()
    }
}
