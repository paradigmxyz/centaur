use std::fmt;

/// A failure reading or interpreting Console's data.
#[derive(Debug)]
pub enum Error {
    Database(sqlx::Error),
    Encryption(active_record_encryption::Error),
    /// A row whose stored data does not have the shape Console writes.
    InvalidData(String),
    /// A row a route named that does not exist. The message is Console's plain
    /// `"<Model> not found"`, not a SQL-ish `RecordNotFound` text.
    NotFound(String),
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Database(error) => write!(formatter, "database operation failed: {error}"),
            Self::Encryption(error) => error.fmt(formatter),
            Self::InvalidData(message) => formatter.write_str(message),
            Self::NotFound(message) => formatter.write_str(message),
        }
    }
}

impl Error {
    /// The uniform not-found error for a Console model name.
    pub fn not_found(model: &str) -> Self {
        Self::NotFound(format!("{model} not found"))
    }

    /// Whether this is Postgres' unique-index violation (`23505`).
    pub fn is_unique_violation(&self) -> bool {
        matches!(
            self,
            Self::Database(sqlx::Error::Database(database))
                if database.code().as_deref() == Some("23505")
        )
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
