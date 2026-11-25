use std::fmt;

/// A failure migrating Console's schema or bootstrapping its first admin.
#[derive(Debug)]
pub enum SetupError {
    Database(sqlx::Error),
    Migrate(sqlx::migrate::MigrateError),
    /// The database's schema cannot be migrated as it stands.
    Schema(String),
    /// The initial admin settings are unusable.
    Bootstrap(String),
}

impl fmt::Display for SetupError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Database(error) => {
                write!(formatter, "Console database operation failed: {error}")
            }
            Self::Migrate(error) => write!(formatter, "Console migration failed: {error}"),
            Self::Schema(message) | Self::Bootstrap(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for SetupError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Database(error) => Some(error),
            Self::Migrate(error) => Some(error),
            Self::Schema(_) | Self::Bootstrap(_) => None,
        }
    }
}

impl From<sqlx::Error> for SetupError {
    fn from(error: sqlx::Error) -> Self {
        Self::Database(error)
    }
}

impl From<sqlx::migrate::MigrateError> for SetupError {
    fn from(error: sqlx::migrate::MigrateError) -> Self {
        Self::Migrate(error)
    }
}
