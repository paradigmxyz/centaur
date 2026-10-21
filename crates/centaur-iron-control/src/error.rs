use thiserror::Error;

use crate::principal::PrincipalDerivationError;

/// Errors returned by the iron-control admin client.
#[derive(Debug, Error)]
pub enum IronControlError {
    /// A session did not carry enough identity metadata to derive its
    /// canonical principal.
    #[error(transparent)]
    PrincipalDerivation(#[from] PrincipalDerivationError),
    /// Preapproved session admission could not find the required principal.
    #[error("session principal {foreign_id} is not preapproved")]
    SessionPrincipalNotPreapproved { foreign_id: String },
    /// The HTTP request could not be sent or the response could not be read.
    #[error("iron-control request to {path} failed: {source}")]
    Transport {
        path: String,
        #[source]
        source: reqwest::Error,
    },
    /// iron-control returned a non-success status. ``body`` is the raw response
    /// text, which carries iron-control's ``{ "error": { "message", "details" } }``
    /// envelope for 4xx validation failures.
    #[error("iron-control {method} {path} returned {status}: {body}")]
    Status {
        method: String,
        path: String,
        status: u16,
        body: String,
    },
    /// A direct read of Console's database failed.
    #[error("console database read failed: {0}")]
    ConsoleDb(#[from] centaur_console_db::Error),
    /// A direct read of Console's database found no such record.
    #[error("iron-control {resource} {ident} not found")]
    NotFound {
        resource: &'static str,
        ident: String,
    },
    /// A 2xx response body did not deserialize into the expected shape.
    #[error("iron-control {path} response did not match the expected schema: {source}")]
    Decode {
        path: String,
        #[source]
        source: reqwest::Error,
    },
}

impl IronControlError {
    /// Whether the requested record does not exist, whether the admin API or
    /// Console's database reported it.
    pub fn is_not_found(&self) -> bool {
        matches!(
            self,
            Self::NotFound { .. } | Self::Status { status: 404, .. }
        )
    }
}

pub type Result<T> = std::result::Result<T, IronControlError>;
