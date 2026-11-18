//! What an area contributes: one [`Routes`] value with a plain axum
//! `Router` per credential namespace. Paths are relative to the
//! namespace's prefix, which [`super::app`] adds together with the
//! namespace's guard, so an area cannot mount a handler under the wrong
//! credential or forget the check.

use axum::Router;

use super::state::AppState;

type Part = Router<AppState>;

/// Routes by namespace. Start from `Routes::default()` and fill the
/// namespaces the area has: `Routes { bearer: Router::new().route(..),
/// ..Routes::default() }`.
#[derive(Default)]
pub struct Routes {
    /// `/api/v1/*`: bearer API keys of active admins.
    pub bearer: Part,
    /// `/api/v1/console/*`: the console session, active admins only.
    pub console: Part,
    /// `/api/v1/console/*`: any active account (scope data to the caller).
    pub console_active: Part,
    /// `/api/v1/console/*`: pending accounts too.
    pub console_pending: Part,
    /// `/api/v1/sandbox/*`: the sandbox entitlement token, bound to its
    /// proxy and principal in the database.
    pub sandbox: Part,
    /// `/api/v1/sandbox/*`: the entitlement token's signature only.
    pub sandbox_claims: Part,
    /// `/api/v1/sandbox/admin/*`: a sandbox acting for an admin's Slack DM.
    pub sandbox_admin: Part,
    /// `/internal/*`: api-rs service tokens.
    pub internal: Part,
    /// Pages and protocol endpoints outside `/api` (login, OAuth, MCP,
    /// PWA). Handlers authenticate themselves from the session.
    pub public: Part,
}

impl Routes {
    /// Combines two areas' routes (a path may only be registered once).
    pub fn merge(self, other: Routes) -> Routes {
        Routes {
            bearer: self.bearer.merge(other.bearer),
            console: self.console.merge(other.console),
            console_active: self.console_active.merge(other.console_active),
            console_pending: self.console_pending.merge(other.console_pending),
            sandbox: self.sandbox.merge(other.sandbox),
            sandbox_claims: self.sandbox_claims.merge(other.sandbox_claims),
            sandbox_admin: self.sandbox_admin.merge(other.sandbox_admin),
            internal: self.internal.merge(other.internal),
            public: self.public.merge(other.public),
        }
    }
}
