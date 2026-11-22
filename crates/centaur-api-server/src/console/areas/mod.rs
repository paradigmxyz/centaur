//! One module per Console area. Each area registers its own routes through
//! the guarded builders; this file only fans out to them.

pub(crate) mod admin;
pub(crate) mod auth_pages;
pub(crate) mod automation;
pub(crate) mod oauth;
pub(crate) mod principals;
pub(crate) mod secrets;

use axum::Router;

use crate::{
    AppState,
    console::{bearer::BearerRoutes, sandbox::SandboxRoutes},
    console_session::ConsoleRoutes,
};

pub(crate) fn console(routes: ConsoleRoutes) -> ConsoleRoutes {
    let routes = principals::console(routes);
    let routes = secrets::console(routes);
    let routes = oauth::console(routes);
    let routes = admin::console(routes);
    let routes = automation::console(routes);
    auth_pages::console(routes)
}

pub(crate) fn bearer(routes: BearerRoutes) -> BearerRoutes {
    let routes = principals::bearer(routes);
    let routes = secrets::bearer(routes);
    let routes = oauth::bearer(routes);
    let routes = admin::bearer(routes);
    let routes = automation::bearer(routes);
    auth_pages::bearer(routes)
}

pub(crate) fn sandbox(routes: SandboxRoutes) -> SandboxRoutes {
    let routes = oauth::sandbox(routes);
    let routes = automation::sandbox(routes);
    auth_pages::sandbox(routes)
}

pub(crate) fn pages(router: Router<AppState>) -> Router<AppState> {
    let router = principals::pages(router);
    let router = secrets::pages(router);
    let router = oauth::pages(router);
    let router = admin::pages(router);
    let router = automation::pages(router);
    auth_pages::pages(router)
}
