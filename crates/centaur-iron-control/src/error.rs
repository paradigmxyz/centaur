use thiserror::Error;

use crate::principal::PrincipalDerivationError;

/// Errors returned by the iron-control client.
#[derive(Debug, Error)]
pub enum IronControlError {
    /// A session did not carry enough identity metadata to derive its
    /// canonical principal.
    #[error(transparent)]
    PrincipalDerivation(#[from] PrincipalDerivationError),
    /// Preapproved session admission could not find the required principal.
    #[error("session principal {foreign_id} is not preapproved")]
    SessionPrincipalNotPreapproved { foreign_id: String },
    /// A direct read of Console's database failed.
    #[error("console database read failed: {0}")]
    ConsoleDb(#[from] centaur_console_db::Error),
    /// A direct read of Console's database found no such record.
    #[error("iron-control {resource} {ident} not found")]
    NotFound {
        resource: &'static str,
        ident: String,
    },
    /// A Console write was rejected (validation, a missing referenced row, or
    /// bad input): what the Console API would answer with a 4xx.
    #[error("iron-control in-process {operation} failed: {message}")]
    InProcess {
        operation: &'static str,
        message: String,
    },
}

impl IronControlError {
    /// Whether the requested record does not exist.
    pub fn is_not_found(&self) -> bool {
        matches!(self, Self::NotFound { .. })
    }
}

pub type Result<T> = std::result::Result<T, IronControlError>;
