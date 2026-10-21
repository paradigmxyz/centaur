//! Reads Console's PostgreSQL schema: the credentials granted to a principal
//! and the proxy configuration built from them.

mod config;
mod conflicts;
mod credentials;
mod error;
pub mod models;
mod oid;

pub use config::{Subject, build_config};
pub use credentials::{SlackChannels, slack_channels};
pub use error::{Error, Result};
pub use oid::oid;
