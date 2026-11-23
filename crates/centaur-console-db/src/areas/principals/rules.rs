//! The principals area's `garde` rules.
//!
//! The field rules live on the typed request structs in [`super::request`] and
//! on the normalized permission draft in [`super::write`]. A write validates the
//! record it will store, so `write` builds a request from the merged draft
//! before validating it; a PATCH field the body did not carry, or carried
//! unchanged, stays absent from that request.
//!
//! `foreign_id` uniqueness needs the database and stays in `write`, as does a
//! role's stored-labels check. Both are merged with these reports through
//! [`crate::validation::Validation`].

use garde::{Error, Result};

use super::{PRINCIPAL_KINDS, SANDBOX_REPO_CACHE_VALUES};
use crate::validation::rules::{BLANK_MESSAGE, INCLUSION_MESSAGE, matches};

pub(crate) const URL_SAFE_PATTERN: &str = r"^[A-Za-z0-9\-._~]+$";

pub(crate) const SLACK_USER_ID_PATTERN: &str = r"^(?:[UW][A-Z0-9]{8,}|USLACK)$";
pub(crate) const SLACK_CHANNEL_ID_PATTERN: &str = r"^[CDG][A-Z0-9]{8,}$";
pub(crate) const SLACK_TEAM_ID_PATTERN: &str = r"^[TE][A-Z0-9]{8,}$";

pub(crate) const EMAIL_PATTERN: &str = r"^[a-zA-Z0-9.!#$%&'*+/=?^_`{|}~-]+@[a-zA-Z0-9](?:[a-zA-Z0-9-]{0,61}[a-zA-Z0-9])?(?:\.[a-zA-Z0-9](?:[a-zA-Z0-9-]{0,61}[a-zA-Z0-9])?)*$";

/// A `foreign_id` must not shadow an opaque id and must be URL-safe. A blank
/// value is no foreign id at all.
pub(crate) fn foreign_id(prefix: &'static str) -> impl FnOnce(&Option<String>, &()) -> Result {
    move |value, _| {
        let Some(value) = value else {
            return Ok(());
        };
        if value.starts_with(prefix) {
            return Err(Error::new("is reserved"));
        }
        if !matches(URL_SAFE_PATTERN, value) {
            return Err(Error::new("is invalid"));
        }
        Ok(())
    }
}

/// `kind` is present and one of the model's kinds.
pub(crate) fn kind(value: &Option<Option<String>>, _: &()) -> Result {
    match value {
        None => Ok(()),
        Some(Some(value)) if value.is_empty() => Err(Error::new(BLANK_MESSAGE)),
        Some(Some(value)) if PRINCIPAL_KINDS.contains(&value.as_str()) => Ok(()),
        _ => Err(Error::new(INCLUSION_MESSAGE)),
    }
}

/// `sandbox_repo_cache` is one of the model's values.
pub(crate) fn repo_cache(value: &Option<Option<String>>, _: &()) -> Result {
    match value {
        None => Ok(()),
        Some(Some(value)) if SANDBOX_REPO_CACHE_VALUES.contains(&value.as_str()) => Ok(()),
        _ => Err(Error::new(INCLUSION_MESSAGE)),
    }
}

/// A nullable Slack field under a format rule. An absent key and an explicit
/// `null` both leave the rule alone.
pub(crate) fn formatted(
    pattern: &'static str,
) -> impl FnOnce(&Option<Option<String>>, &()) -> Result {
    move |value, _| match value {
        Some(Some(value)) if !matches(pattern, value) => Err(Error::new("is invalid")),
        _ => Ok(()),
    }
}

/// A channel id must be present and formatted as one.
pub(crate) fn channel_id(value: &str, _: &()) -> Result {
    if value.is_empty() {
        return Err(Error::new(BLANK_MESSAGE));
    }
    if !matches(SLACK_CHANNEL_ID_PATTERN, value) {
        return Err(Error::new("is invalid"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::areas::principals::PRINCIPAL_RESERVED_PREFIX;

    #[test]
    fn foreign_id_rejects_a_reserved_prefix_and_unsafe_characters() {
        let check = |value: Option<&str>| {
            foreign_id(PRINCIPAL_RESERVED_PREFIX)(&value.map(str::to_owned), &()).is_ok()
        };
        assert!(check(None));
        assert!(check(Some("diff-channel")));
        assert!(!check(Some("prn_abc")));
        assert!(!check(Some("diff channel")));
    }

    #[test]
    fn kind_needs_a_known_non_blank_value() {
        let check = |value: Option<Option<&str>>| {
            kind(&value.map(|value| value.map(str::to_owned)), &()).is_ok()
        };
        assert!(check(None));
        assert!(check(Some(Some("slack_channel"))));
        assert!(!check(Some(None)));
        assert!(!check(Some(Some(""))));
        assert!(!check(Some(Some("not-a-kind"))));
    }

    #[test]
    fn repo_cache_is_one_of_the_model_values_or_unset() {
        let check = |value: Option<Option<&str>>| {
            repo_cache(&value.map(|value| value.map(str::to_owned)), &()).is_ok()
        };
        assert!(check(None));
        assert!(check(Some(Some("all"))));
        assert!(!check(Some(None)));
        assert!(!check(Some(Some("some"))));
    }

    #[test]
    fn formatted_skips_absent_and_null_values() {
        let check = |value: Option<Option<&str>>| {
            formatted(SLACK_USER_ID_PATTERN)(&value.map(|value| value.map(str::to_owned)), &())
                .is_ok()
        };
        assert!(check(None));
        assert!(check(Some(None)));
        assert!(check(Some(Some("USLACK"))));
        assert!(!check(Some(Some("nope"))));
    }

    #[test]
    fn a_channel_id_needs_presence_and_the_slack_format() {
        assert!(channel_id("C0DIFFNEW1", &()).is_ok());
        assert!(channel_id("", &()).is_err());
        assert!(channel_id("nope", &()).is_err());
    }
}
