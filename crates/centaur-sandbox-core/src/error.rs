use std::fmt::Display;

use thiserror::Error;

pub type SandboxResult<T> = Result<T, SandboxError>;

/// Boxed error used to carry backend-specific failure causes across the
/// backend-neutral [`SandboxError`] boundary without flattening the chain.
pub type BoxedError = Box<dyn std::error::Error + Send + Sync + 'static>;

#[derive(Debug, Error)]
pub enum SandboxError {
    #[error("sandbox {0} was not found")]
    NotFound(String),

    #[error("operation is unsupported by backend {backend}: {operation}")]
    Unsupported {
        backend: &'static str,
        operation: &'static str,
    },

    #[error("sandbox is not ready: {0}")]
    NotReady(String),

    #[error("sandbox I/O failed: {context}")]
    Io {
        context: String,
        #[source]
        source: Option<BoxedError>,
    },

    #[error("backend operation failed: {context}")]
    Backend {
        context: String,
        #[source]
        source: Option<BoxedError>,
    },

    #[error("invalid sandbox spec: {0}")]
    InvalidSpec(String),
}

impl SandboxError {
    /// I/O failure with only a descriptive context.
    pub fn io(context: impl Into<String>) -> Self {
        Self::Io {
            context: context.into(),
            source: None,
        }
    }

    /// I/O failure preserving the underlying error as `source()`. The cause
    /// is also rendered into the context so `Display` keeps the full story.
    pub fn io_source(context: impl Display, source: impl Into<BoxedError>) -> Self {
        let source = source.into();
        Self::Io {
            context: format!("{context}: {source}"),
            source: Some(source),
        }
    }

    /// Backend failure with only a descriptive context.
    pub fn backend(context: impl Into<String>) -> Self {
        Self::Backend {
            context: context.into(),
            source: None,
        }
    }

    /// Backend failure preserving the underlying error as `source()`. The
    /// cause is also rendered into the context so `Display` keeps the full
    /// story.
    pub fn backend_source(context: impl Display, source: impl Into<BoxedError>) -> Self {
        let source = source.into();
        Self::Backend {
            context: format!("{context}: {source}"),
            source: Some(source),
        }
    }
}
