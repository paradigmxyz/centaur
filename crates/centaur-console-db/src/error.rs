use std::fmt;

/// A failure reading or interpreting Console's data.
#[derive(Debug)]
pub enum Error {
    Database(sqlx::Error),
    Encryption(active_record_encryption::Error),
    /// A row whose stored data does not have the shape Console writes.
    InvalidData(String),
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Database(error) => write!(formatter, "database operation failed: {error}"),
            Self::Encryption(error) => error.fmt(formatter),
            Self::InvalidData(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for Error {}

impl From<sqlx::Error> for Error {
    fn from(error: sqlx::Error) -> Self {
        Self::Database(error)
    }
}

impl From<active_record_encryption::Error> for Error {
    fn from(error: active_record_encryption::Error) -> Self {
        Self::Encryption(error)
    }
}

pub type Result<T> = std::result::Result<T, Error>;
