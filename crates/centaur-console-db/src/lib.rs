//! Reads Console's PostgreSQL schema: principals, the credentials granted to
//! them, and the proxy configuration built from those credentials. It also
//! runs Console's scheduled tasks (see [`scheduled_tasks`]) and owns the
//! schema itself: [`migrate`] applies it, and [`bootstrap_initial_admin`]
//! creates the first operator.

mod api_clients;
pub mod areas;
mod bootstrap;
mod config;
mod conflicts;
mod credentials;
pub mod cron;
mod error;
pub mod errors;
pub mod invalidate;
mod migrations;
/// Table descriptors and the generic lookups/deletes (`ConsoleModel`).
pub mod model;
/// The credential shapes the proxy configuration is built from (not table
/// descriptors; those are in [`model`]).
pub mod models;
mod oid;
mod principals;
mod sandbox;
pub mod scheduled_tasks;
mod setup_error;
pub mod slack_bot_channels;
pub mod ssh_key;
mod ssh_keys;
pub mod timestamp;
mod users;
pub mod validation;
pub mod write;

pub use api_clients::{ApiClient, ApiClientPrincipal, api_client_principals, find_api_client};
pub use bootstrap::{Bootstrap, InitialAdmin, bootstrap_initial_admin};
pub use config::{REDACTED, SecretValues, Subject, build_config};
pub use credentials::{SlackChannels, slack_channels};
pub use error::{Error, Result};
pub use errors::FieldErrors;
pub use migrations::{migrate, rails_versions};
pub use oid::{decode_oid, oid};
pub use principals::{
    Principal, SlackChannelPermission, effective_config, find_direct_slack_channel_permission,
    find_principal, find_principal_by_foreign_id, find_user_id_by_email,
};
pub use sandbox::{ConsoleProxy, find_console_proxy, slack_bot_home_team};
pub use setup_error::SetupError;
pub use ssh_keys::{SshKey, ssh_keys};
pub use users::{
    ConsoleUser, ConsoleUserStatus, SYSTEM_USER_EMAIL, find_console_api_key,
    find_console_password_user_by_email, find_console_user, system_user_id,
};
