//! Reads Console's PostgreSQL schema: principals, the credentials granted to
//! them, and the proxy configuration built from those credentials.

mod config;
mod conflicts;
mod credentials;
mod error;
pub mod models;
mod oid;
mod principals;

pub use config::{REDACTED, SecretValues, Subject, build_config};
pub use credentials::{SlackChannels, slack_channels};
pub use error::{Error, Result};
pub use oid::{decode_oid, oid};
pub use principals::{
    Principal, SlackChannelPermission, effective_config, find_direct_slack_channel_permission,
    find_principal, find_principal_by_foreign_id, find_user_id_by_email,
};
