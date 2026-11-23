//! Reads Console's PostgreSQL schema: principals, the credentials granted to
//! them, and the proxy configuration built from those credentials. It also
//! runs Console's scheduled tasks, the only rows it writes (see
//! [`scheduled_tasks`]).

mod api_clients;
pub mod areas;
mod config;
mod conflicts;
mod credentials;
pub mod cron;
mod error;
pub mod errors;
pub mod invalidate;
/// Table descriptors and the generic lookups/deletes (`ConsoleModel`).
pub mod model;
/// The credential shapes the proxy configuration is built from (not table
/// descriptors; those are in [`model`]).
pub mod models;
mod oid;
mod principals;
mod sandbox;
pub mod scheduled_tasks;
pub mod ssh_key;
mod ssh_keys;
pub mod timestamp;
mod users;
pub mod validation;
pub mod write;

pub use api_clients::{ApiClient, ApiClientPrincipal, api_client_principals, find_api_client};
pub use config::{REDACTED, SecretValues, Subject, build_config};
pub use credentials::{SlackChannels, slack_channels};
pub use error::{Error, Result};
pub use errors::FieldErrors;
pub use oid::{decode_oid, oid};
pub use principals::{
    Principal, SlackChannelPermission, effective_config, find_direct_slack_channel_permission,
    find_principal, find_principal_by_foreign_id, find_user_id_by_email,
};
pub use sandbox::{ConsoleProxy, find_console_proxy, slack_bot_home_team};
pub use ssh_keys::{SshKey, ssh_keys};
pub use users::{
    ConsoleUser, ConsoleUserStatus, find_console_api_key, find_console_password_user_by_email,
    find_console_user,
};
