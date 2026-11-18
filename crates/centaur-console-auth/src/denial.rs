//! Authentication and authorization refusals, with the exact status,
//! message, and `details.reason` Rails renders through `render_error`.

/// A refused request: render `{"error":{"message":..,"details":{"reason":..}}}`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Denial {
    pub status: u16,
    pub message: &'static str,
    pub reason: Option<&'static str>,
}

impl Denial {
    const fn new(status: u16, message: &'static str, reason: Option<&'static str>) -> Self {
        Self {
            status,
            message,
            reason,
        }
    }

    // Bearer API (`Api::BaseController#authenticate_api_key!`).
    pub const INVALID_API_KEY: Self = Self::new(401, "invalid or missing API key", None);
    pub const API_KEY_NOT_ADMIN: Self = Self::new(403, "API key owner is not an admin", None);

    // Console API (`ConsoleApiAuthentication`).
    pub const BEARER_ON_CONSOLE: Self = Self::new(
        403,
        "the console API requires a console session; API keys are not accepted \
         (the bearer API is /api/v1 outside /api/v1/console)",
        Some("session_required"),
    );
    pub const SESSION_REQUIRED: Self =
        Self::new(401, "console session required", Some("session_required"));
    pub const ACCOUNT_DISABLED: Self = Self::new(401, "account disabled", Some("account_disabled"));
    pub const ACCOUNT_PENDING: Self =
        Self::new(403, "account pending approval", Some("account_pending"));
    pub const CSRF: Self = Self::new(403, "invalid or missing CSRF token", Some("csrf"));
    pub const ADMIN_REQUIRED: Self =
        Self::new(403, "admin access required", Some("admin_required"));

    // Both (`refuse_credential_namespace_mismatch!`, production behaviour).
    pub const NAMESPACE_MISMATCH: Self = Self::new(
        401,
        "endpoint credential misconfigured",
        Some("credential_namespace_mismatch"),
    );

    // Sandbox API (`SandboxTokenAuthentication`, `SandboxAdminAuthentication`).
    pub const SANDBOX_TOKEN_MISSING: Self =
        Self::new(401, "invalid or missing sandbox token", None);
    pub const SANDBOX_TOKEN_INVALID: Self = Self::new(401, "invalid sandbox token", None);
    // Internal API (`/internal/v1/*`, api-rs service tokens; Rust-only).
    pub const SERVICE_TOKEN_INVALID: Self =
        Self::new(401, "invalid or missing service token", None);

    pub const SANDBOX_ADMIN_REQUIRED: Self = Self::new(
        403,
        "admin changes require a Slack DM from an active Console admin in the bot's workspace",
        None,
    );
}

/// `ConsoleSignIn::DISABLED_MESSAGE`.
pub const DISABLED_MESSAGE: &str = "Your account is disabled.";
