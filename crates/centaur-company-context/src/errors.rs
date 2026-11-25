use std::{error::Error, fmt};

#[derive(Debug)]
pub struct Rejected(String);

impl fmt::Display for Rejected {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Error for Rejected {}

pub fn rejected(message: impl Into<String>) -> anyhow::Error {
    Rejected(message.into()).into()
}

pub fn is_rejected(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| cause.is::<Rejected>())
}

/// A source refused the request's credential, though another credential may
/// succeed.
#[derive(Debug)]
pub struct Denied(String);

impl fmt::Display for Denied {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Error for Denied {}

pub fn denied(message: impl Into<String>) -> anyhow::Error {
    Denied(message.into()).into()
}

pub fn is_denied(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| cause.is::<Denied>())
}

#[cfg(test)]
mod tests {
    use anyhow::Context;

    use super::*;

    #[test]
    fn rejection_survives_context() {
        let error = Err::<(), _>(rejected("invalid document"))
            .context("extract PDF")
            .unwrap_err();
        assert!(is_rejected(&error));
    }
}
