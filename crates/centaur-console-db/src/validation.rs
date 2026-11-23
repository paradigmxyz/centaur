//! garde-based request validation, adapted to Console's [`FieldErrors`].
//!
//! Console's data-layer documents declare their field rules with `garde` too
//! (see [`crate::areas::secrets`]); the HTTP request structs do the same, and
//! this module turns garde's report into the ordered `details` hash of the 422
//! `{"error":{"message":"validation failed","details":{"<path>":["<msg>"]}}}`
//! envelope. See `console/README.md` ("Validation with garde") for which rules
//! belong in attributes, which stay hand-written, and how to spell messages.
//!
//! ```ignore
//! use centaur_console_db::FieldErrors;
//! use centaur_console_db::validation::{rules, ValidateFields};
//!
//! #[derive(serde::Deserialize, garde::Validate)]
//! struct CreateApiKey {
//!     #[serde(default)]
//!     #[garde(custom(rules::present_not_blank))]
//!     name: Option<String>,
//! }
//!
//! async fn create(ConsoleData(data): ConsoleData<CreateApiKey>) -> Result<Response, ConsoleError> {
//!     data.validate_fields()?; // 422 validation failed, `details.name`
//!     // ...
//! }
//! ```
//!
//! Two properties of the derive matter for the API contract:
//!
//! - garde validates a struct's named fields **alphabetically** (its derive
//!   holds them in a `BTreeMap`), not in declaration order. Validation messages
//!   and their order are not part of the API contract, so a report is returned
//!   as garde emits it.
//! - `#[garde(custom(...))]` receives the field as declared, so a validator for
//!   `Option<Option<String>>` (a PATCH field) is not the same function as one
//!   for `Option<String>` (a create field). [`rules`] spells both.

use std::borrow::Cow;

use crate::FieldErrors;

/// The 422 `details` key for one garde error path.
///
/// A top-level field keeps its plain name, a nested record is reported under
/// the path-keyed form the secrets document validators already use
/// (`source.config`, `rules[0].cidr`), and a rule on the record as a whole is
/// reported as `base`.
pub fn detail_key(path: &garde::Path) -> Cow<'_, str> {
    if path.is_empty() {
        Cow::Borrowed(FieldErrors::BASE)
    } else {
        Cow::Owned(path.to_string())
    }
}

/// One array of messages per path, in garde's report order (a field's messages
/// keep the order its rules ran in).
impl From<garde::Report> for FieldErrors {
    fn from(report: garde::Report) -> Self {
        let mut errors = Self::new();
        for (path, error) in report.into_inner() {
            errors.add(&detail_key(&path), error.message());
        }
        errors
    }
}

/// One plain line for a report, for a gate the API answers with a bare 400
/// rather than the 422 field envelope: the first message, because those gates
/// carry a single rule (the users page's `status` filter). `None` when the
/// report is empty.
pub fn plain_message(report: &garde::Report) -> Option<String> {
    report
        .iter()
        .next()
        .map(|(_, error)| error.message().to_owned())
}

/// Validate a typed request with garde and hand back Console's field errors.
pub trait ValidateFields: garde::Validate {
    /// `Ok(())`, or the 422 `details` of the failed report.
    fn validate_fields(&self) -> Result<(), FieldErrors>
    where
        Self::Context: Default;

    /// The same, with the context a struct declares with `#[garde(context(..))]`.
    fn validate_fields_with(&self, context: &Self::Context) -> Result<(), FieldErrors>;
}

impl<T: garde::Validate + ?Sized> ValidateFields for T {
    fn validate_fields(&self) -> Result<(), FieldErrors>
    where
        Self::Context: Default,
    {
        self.validate_fields_with(&Self::Context::default())
    }

    fn validate_fields_with(&self, context: &Self::Context) -> Result<(), FieldErrors> {
        self.validate_with(context).map_err(FieldErrors::from)
    }
}

/// Collects one request's garde report and the hand-written messages that need
/// the database or the request context, then fails the handler with all of them.
///
/// Console's routes check some things before the record is saved (a referenced
/// principal that does not exist, a `default_role_ids` entry that names no role)
/// and their errors win over the record's own: append them, call
/// [`Self::finish_if_any`], and only then run the record's rules.
///
/// ```ignore
/// let mut validation = Validation::new();
/// if !reference.is_empty() && principal.is_none() {
///     validation.add_base(format!("No principal matches \"{reference}\"."));
/// }
/// validation.finish_if_any()?; // the reference wins; nothing else has run yet
/// validation.check(&data);
/// validation.into_result()?;
/// ```
#[derive(Clone, Debug, Default)]
pub struct Validation {
    errors: FieldErrors,
}

impl Validation {
    pub fn new() -> Self {
        Self::default()
    }

    /// Append `value`'s garde report.
    pub fn check<T: garde::Validate>(&mut self, value: &T)
    where
        T::Context: Default,
    {
        self.check_with(value, &T::Context::default());
    }

    /// Append `value`'s garde report, with its declared validation context.
    pub fn check_with<T: garde::Validate>(&mut self, value: &T, context: &T::Context) {
        if let Err(report) = value.validate_with(context) {
            self.errors.extend(report.into());
        }
    }

    /// Append errors another validator produced, keeping their order.
    pub fn extend(&mut self, errors: FieldErrors) {
        self.errors.extend(errors);
    }

    pub fn add(&mut self, attribute: &str, message: impl Into<String>) {
        self.errors.add(attribute, message);
    }

    pub fn add_base(&mut self, message: impl Into<String>) {
        self.errors.add_base(message);
    }

    pub fn errors(&self) -> &FieldErrors {
        &self.errors
    }

    pub fn is_empty(&self) -> bool {
        self.errors.is_empty()
    }

    /// Fail now when something has already failed, so a later and lower-precedence
    /// check cannot add to or replace the earlier errors.
    pub fn finish_if_any(&mut self) -> Result<(), FieldErrors> {
        if self.errors.is_empty() {
            return Ok(());
        }
        Err(std::mem::take(&mut self.errors))
    }

    /// `Ok(())`, or everything collected as Console's 422 `details`.
    pub fn into_result(self) -> Result<(), FieldErrors> {
        if self.errors.is_empty() {
            return Ok(());
        }
        Err(self.errors)
    }
}

/// The custom validators Console's request structs share.
///
/// Each covers a shape garde has no built-in rule for; the message is a short
/// one-liner and is not itself contract.
pub mod rules {
    use std::collections::HashMap;
    use std::sync::{LazyLock, RwLock};

    use garde::{Error, Result};

    /// The blank-field message.
    pub const BLANK_MESSAGE: &str = "can't be blank";
    /// The inclusion message.
    pub const INCLUSION_MESSAGE: &str = "is not included in the list";

    /// Whether `value` matches the static `pattern`, compiling it once. An
    /// invalid pattern is a programming error, not input.
    pub fn matches(pattern: &'static str, value: &str) -> bool {
        static CACHE: LazyLock<RwLock<HashMap<&'static str, regex::Regex>>> =
            LazyLock::new(|| RwLock::new(HashMap::new()));
        if let Some(regex) = CACHE
            .read()
            .expect("the cache is not poisoned")
            .get(pattern)
        {
            return regex.is_match(value);
        }
        let regex = regex::Regex::new(pattern).expect("static regex compiles");
        let matched = regex.is_match(value);
        CACHE
            .write()
            .expect("the cache is not poisoned")
            .insert(pattern, regex);
        matched
    }

    /// A create-style field: the key may be absent, but an absent, `null`, or
    /// blank value fails the record's presence validation.
    pub fn present_not_blank(value: &Option<String>, _: &()) -> Result {
        match value {
            Some(value) if !value.trim().is_empty() => Ok(()),
            _ => Err(Error::new(BLANK_MESSAGE)),
        }
    }

    /// A PATCH field: absent keeps the stored value, while an explicit `null` or
    /// a blank value fails the record's presence validation.
    pub fn patch_not_blank(value: &Option<Option<String>>, _: &()) -> Result {
        match value {
            None => Ok(()),
            Some(value) => present_not_blank(value, &()),
        }
    }

    /// A PATCH field under an inclusion validation: absent keeps the stored
    /// value, and an explicit `null` is out of the list.
    pub fn patch_included_in<'a>(
        allowed: &'a [&'a str],
    ) -> impl FnOnce(&Option<Option<String>>, &()) -> Result + 'a {
        move |value, _| match value {
            None => Ok(()),
            Some(Some(value)) if allowed.contains(&value.as_str()) => Ok(()),
            _ => Err(Error::new(INCLUSION_MESSAGE)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::rules;
    use super::{ValidateFields, Validation, detail_key, plain_message};
    use crate::FieldErrors;
    use garde::Validate;
    use serde::Deserialize;
    use serde_json::json;

    fn entries(errors: FieldErrors) -> Vec<(String, Vec<String>)> {
        errors
            .entries()
            .map(|(attribute, messages)| (attribute.to_owned(), messages.to_vec()))
            .collect()
    }

    #[derive(Debug, Deserialize, Validate)]
    struct Source {
        #[garde(custom(rules::present_not_blank))]
        config: Option<String>,
    }

    #[derive(Debug, Deserialize, Validate)]
    struct Rule {
        #[garde(custom(rules::present_not_blank))]
        cidr: Option<String>,
    }

    #[derive(Debug, Deserialize, Validate)]
    struct Document {
        #[garde(custom(rules::present_not_blank))]
        name: Option<String>,
        #[garde(dive)]
        source: Source,
        #[garde(dive)]
        rules: Vec<Rule>,
        #[garde(custom(rules::patch_included_in(&["none", "public"])))]
        #[serde(default, deserialize_with = "nullable")]
        repo_cache: Option<Option<String>>,
    }

    fn nullable<'de, D>(deserializer: D) -> Result<Option<Option<String>>, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        Option::<String>::deserialize(deserializer).map(Some)
    }

    #[test]
    fn top_level_fields_keep_their_plain_name() {
        let document = Document {
            name: Some(" ".to_owned()),
            source: Source { config: None },
            rules: Vec::new(),
            repo_cache: Some(None),
        };
        // garde's derive validates named fields alphabetically.
        assert_eq!(
            entries(document.validate().unwrap_err().into()),
            [
                ("name".to_owned(), vec!["can't be blank".to_owned()]),
                (
                    "repo_cache".to_owned(),
                    vec!["is not included in the list".to_owned()]
                ),
                (
                    "source.config".to_owned(),
                    vec!["can't be blank".to_owned()]
                ),
            ]
        );
    }

    #[test]
    fn nested_and_per_item_paths_use_the_path_keyed_form() {
        let document = Document {
            name: Some("ok".to_owned()),
            source: Source {
                config: Some("ok".to_owned()),
            },
            rules: vec![
                Rule {
                    cidr: Some("ok".to_owned()),
                },
                Rule { cidr: None },
                Rule {
                    cidr: Some(String::new()),
                },
            ],
            repo_cache: None,
        };
        let errors = FieldErrors::from(document.validate().unwrap_err());
        let keys = errors
            .entries()
            .map(|(attribute, _)| attribute.to_owned())
            .collect::<Vec<_>>();
        assert_eq!(keys, ["rules[1].cidr", "rules[2].cidr"]);
        assert_eq!(errors["rules[1].cidr"], ["can't be blank"]);
    }

    #[test]
    fn a_record_rule_is_reported_under_base() {
        #[derive(Validate)]
        #[garde(custom(whole))]
        struct Record {
            #[garde(skip)]
            count: usize,
        }

        fn whole(value: &Record, _: &()) -> garde::Result {
            if value.count > 1 {
                return Err(garde::Error::new("has too many rows"));
            }
            Ok(())
        }

        let errors = FieldErrors::from(Record { count: 2 }.validate().unwrap_err());
        assert_eq!(
            entries(errors),
            [("base".to_owned(), vec!["has too many rows".to_owned()])]
        );
    }

    #[test]
    fn detail_key_of_an_empty_path_is_base() {
        assert_eq!(detail_key(&garde::Path::empty()), "base");
        assert_eq!(
            detail_key(&garde::Path::new("rules").join(0).join("cidr")),
            "rules[0].cidr"
        );
    }

    #[test]
    fn a_passing_report_is_no_errors() {
        let document = Document {
            name: Some("name".to_owned()),
            source: Source {
                config: Some("config".to_owned()),
            },
            rules: Vec::new(),
            repo_cache: Some(Some("public".to_owned())),
        };
        assert!(document.validate_fields().is_ok());
        assert!(document.validate_fields_with(&()).is_ok());
        assert!(document.validate().is_ok());
    }

    #[test]
    fn a_query_gate_renders_one_plain_message() {
        #[derive(Deserialize, Validate)]
        struct Query {
            #[garde(custom(allowed))]
            #[serde(default)]
            status: Option<String>,
        }

        fn allowed(value: &Option<String>, _: &()) -> garde::Result {
            match value.as_deref() {
                Some("active") => Ok(()),
                Some(_) => Err(garde::Error::new("status must be one of active")),
                None => Ok(()),
            }
        }

        let report = Query {
            status: Some("bogus".to_owned()),
        }
        .validate()
        .unwrap_err();
        assert_eq!(
            plain_message(&report).as_deref(),
            Some("status must be one of active")
        );
        assert_eq!(
            FieldErrors::from(report).into_value(),
            json!({"status": ["status must be one of active"]})
        );
    }

    #[test]
    fn validation_collects_garde_and_hand_written_errors_in_order() {
        let mut validation = Validation::new();
        assert!(validation.is_empty());
        assert!(validation.finish_if_any().is_ok());

        validation.add_base("No principal matches \"nope\".");
        // The reference wins before any field is checked.
        assert_eq!(
            entries(validation.finish_if_any().unwrap_err()),
            [(
                "base".to_owned(),
                vec!["No principal matches \"nope\".".to_owned()]
            )]
        );
        assert!(validation.is_empty());

        validation.check(&Source { config: None });
        validation.add("name", "can't be blank");
        validation.add("name", "is too short");
        validation.check(&Source { config: None });
        assert_eq!(validation.errors()["config"], ["can't be blank"; 2]);
        let errors = validation.into_result().unwrap_err();
        assert_eq!(
            entries(errors),
            [
                ("config".to_owned(), vec!["can't be blank".to_owned(); 2]),
                (
                    "name".to_owned(),
                    vec!["can't be blank".to_owned(), "is too short".to_owned()]
                ),
            ]
        );
    }
}
