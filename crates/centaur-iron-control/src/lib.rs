//! Admin client for the [iron-control](https://docs.iron.sh) control plane.
//!
//! iron-control owns the secret/identity model that backs per-sandbox
//! [iron-proxy](https://docs.iron.sh) egress: secrets, the roles that bundle
//! them, the principals that hold roles, and the proxies that sync an
//! effective configuration. This crate registers that model in the Console
//! database for api-rs; it does not run the proxy sync itself. The Console
//! owns the database, so every write goes through `centaur_console_db`.

mod client;
mod database;
mod error;
mod in_process;
mod models;
mod principal;
mod registry;
mod session;
mod util;

pub use client::IronControlClient;
pub use error::{IronControlError, Result};
pub use models::{
    AwsAuthSecretInput, BrokerCredentialInput, BrokerCredentialRecord, DataEnvelope,
    EffectiveConfig, EffectivePgDsn, EffectiveReplace, EffectiveSecret,
    GCP_ID_TOKEN_ALLOWED_HEADERS, GcpAuthSecretInput, GcpIdTokenSecretInput, Grant, GrantSecret,
    Grantee, HmacSecretHeader, HmacSecretInput, IdentityInput, InjectConfig, OAuthTokenSecretInput,
    PgDsnSecretInput, PgDsnSettingInput, PgDsnSettingValueFromInput, Principal, PrincipalInput,
    Proxy, ProxyInput, ReplaceConfig, RequestRule, Role, SECRET_TYPES, SecretRecord, SecretSource,
    SlackChannelPermissionInput, StaticSecretInput, normalize_gcp_id_token_header,
};
pub use principal::{
    PrincipalDerivationError, PrincipalRef, derive_principal, derive_slack_requester_principal,
};
pub use registry::{
    GCP_AUTH_DEFAULT_SCOPE, RegisterError, RoleSecretWriter, RoleSpec, SecretInput, TranslateError,
    gcp_auth_scopes_or_default, grant_inputs_to_role, register_role, secret_inputs_from_fragment,
    source_from_placeholder, unique_foreign_id,
};
pub use session::SessionRegistrar;
pub use util::{managed_labels, slugify};
