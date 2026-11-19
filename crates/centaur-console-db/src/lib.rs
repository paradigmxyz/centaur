//! Reads Console's PostgreSQL schema: principals, the credentials granted to
//! them, and the proxy configuration built from those credentials. It also
//! runs Console's scheduled tasks, the only rows it writes (see
//! [`scheduled_tasks`]).

mod api_clients;
mod config;
mod conflicts;
mod credentials;
pub mod cron;
mod error;
pub mod models;
mod oid;
mod principals;
pub mod scheduled_tasks;
mod ssh_keys;

pub use api_clients::{ApiClient, ApiClientPrincipal, api_client_principals, find_api_client};
pub use config::{REDACTED, SecretValues, Subject, build_config};
pub use credentials::{SlackChannels, slack_channels};
pub use error::{Error, Result};
pub use oid::{decode_oid, oid};
pub use principals::{
    Principal, SlackChannelPermission, effective_config, find_direct_slack_channel_permission,
    find_principal, find_principal_by_foreign_id, find_user_id_by_email,
};
pub use ssh_keys::{SshKey, ssh_keys};
