//! Console area: admin. See `../README.md` for conventions.
//!
//! Console users, API clients with their principal links, the singleton system
//! settings and its default roles, the Slack Connect allowlist, and the bearer
//! `api_keys` and `proxies` resources.

mod api_clients;
mod api_keys;
mod common;
mod proxies;
mod system_settings;
mod users;

use crate::console::guard::GuardedRoutes;

/// Routes under `/api/v1/console` (paths relative to that prefix).
pub(crate) fn console(routes: GuardedRoutes) -> GuardedRoutes {
    let routes = users::console(routes);
    let routes = api_keys::console(routes);
    let routes = api_clients::console(routes);
    system_settings::console(routes)
}

/// Routes under `/api/v1` (bearer API; paths relative to that prefix).
pub(crate) fn bearer(routes: GuardedRoutes) -> GuardedRoutes {
    let routes = api_keys::bearer(routes);
    proxies::bearer(routes)
}
