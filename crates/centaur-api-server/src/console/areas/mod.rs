//! One module per Console area. Each area registers its own routes through
//! the guarded builders; this file only fans out to them.

pub(crate) mod admin;
pub(crate) mod auth_pages;
pub(crate) mod automation;
pub(crate) mod oauth;
pub(crate) mod principals;
pub(crate) mod secrets;

use axum::Router;

use crate::{AppState, console::guard::GuardedRoutes};

pub(crate) fn console(routes: GuardedRoutes) -> GuardedRoutes {
    let routes = principals::console(routes);
    let routes = secrets::console(routes);
    let routes = oauth::console(routes);
    let routes = admin::console(routes);
    let routes = automation::console(routes);
    auth_pages::console(routes)
}

pub(crate) fn bearer(routes: GuardedRoutes) -> GuardedRoutes {
    let routes = principals::bearer(routes);
    let routes = secrets::bearer(routes);
    let routes = oauth::bearer(routes);
    let routes = admin::bearer(routes);
    let routes = automation::bearer(routes);
    auth_pages::bearer(routes)
}

pub(crate) fn sandbox(routes: GuardedRoutes) -> GuardedRoutes {
    let routes = oauth::sandbox(routes);
    let routes = automation::sandbox(routes);
    auth_pages::sandbox(routes)
}

pub(crate) fn pages(router: Router<AppState>) -> Router<AppState> {
    auth_pages::pages(oauth::pages(router))
}
