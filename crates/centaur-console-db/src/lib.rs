//! Console's PostgreSQL schema.
//!
//! Two layers live here:
//!
//! - the read side other services use (proxy-sync, api-rs, iron-control,
//!   token-broker): principals, the credentials granted to them, and the proxy
//!   configuration built from those credentials (the crate-root exports);
//! - the console's own data layer: migrations and Rails schema adoption
//!   ([`migrate`]), one module per table ([`tables`]), and the shared pieces
//!   every model needs ([`record`], [`json`], [`time`], [`encryption`],
//!   [`tx`], [`sync_config`]).

mod api_clients;
mod config;
mod conflicts;
mod credentials;
pub mod encryption;
mod error;
pub mod keys;
pub mod migrate;
pub mod models;
mod oid;
pub mod personal_ssh_keys;
mod principals;
pub mod redact;
pub mod schedule;
mod ssh_keys;
pub mod sync_config;
#[cfg(feature = "testing")]
pub mod testing;
pub mod time;

pub use api_clients::{ApiClient, ApiClientPrincipal, api_client_principals, find_api_client};
pub use config::{REDACTED, SecretValues, Subject, build_config};
pub use credentials::{SlackChannels, slack_channels};
pub use encryption::{Ciphertext, Encryptor};
pub use error::{Error, Result};
pub use oid::{decode_oid, oid};
pub use principals::{
    Principal, SlackChannelPermission, effective_config, find_direct_slack_channel_permission,
    find_principal, find_principal_by_foreign_id, find_user_id_by_email, valid_slack_team_id,
    valid_slack_user_id,
};
pub use ssh_keys::{SshKey, ssh_keys};
pub use time::Clock;
