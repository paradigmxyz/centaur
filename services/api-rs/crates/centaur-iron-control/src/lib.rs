//! Admin client for the [iron-control](https://docs.iron.sh) control plane.
//!
//! iron-control owns the secret/identity model that backs per-sandbox
//! [iron-proxy](https://docs.iron.sh) egress: secrets, the roles that bundle
//! them, the principals that hold roles, and the proxies that sync an
//! effective configuration. This crate is the typed HTTP client the API uses
//! to register that model — it does not run the proxy sync itself.

mod client;
mod error;
mod models;
mod principal;
mod registry;
mod session;
mod util;

pub use client::IronControlClient;
pub use error::{IronControlError, Result};
pub use models::{
    GcpAuthSecretInput, Grant, GrantSecret, Grantee, IdentityInput, InjectConfig,
    OAuthTokenSecretInput, Principal, Proxy, ProxyInput, ReplaceConfig, RequestRule, Role,
    SecretRecord, SecretSource, StaticSecretInput,
};
pub use principal::{PrincipalRef, derive_principal};
pub use registry::{
    RegisterError, RoleSpec, SecretInput, TranslateError, register_role,
    secret_inputs_from_fragment,
};
pub use session::SessionRegistrar;
