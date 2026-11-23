//! Validation errors for Console records.
//!
//! Console's API renders `422 {"error":{"message":"validation failed",...}}`
//! with one array per attribute in `details`, and this type keeps that shape: an
//! ordered list rather than a map, so messages appended together stay together.
//! `base` is an ordinary attribute name. Attribute order and message text are
//! not part of the API contract.
//!
//! ```ignore
//! use centaur_console_db::FieldErrors;
//!
//! let mut errors = FieldErrors::new();
//! if name.trim().is_empty() {
//!     errors.add("name", "can't be blank");
//! }
//! if errors.is_empty() {
//!     // write the row
//! } else {
//!     return Err(errors.into()); // ConsoleError: 422 validation failed
//! }
//! ```

use std::fmt;

use serde_json::{Map, Value};

/// Validation errors, one array per attribute, messages in the order they were
/// appended.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FieldErrors {
    entries: Vec<(String, Vec<String>)>,
}

impl FieldErrors {
    /// The attribute for an error on the record as a whole.
    pub const BASE: &'static str = "base";

    pub fn new() -> Self {
        Self::default()
    }

    /// Records one message for `attribute`; an existing attribute appends.
    pub fn add(&mut self, attribute: &str, message: impl Into<String>) {
        if let Some((_, messages)) = self
            .entries
            .iter_mut()
            .find(|(existing, _)| existing == attribute)
        {
            messages.push(message.into());
            return;
        }
        self.entries
            .push((attribute.to_owned(), vec![message.into()]));
    }

    /// [`Self::add`] on [`Self::BASE`].
    pub fn add_base(&mut self, message: impl Into<String>) {
        self.add(Self::BASE, message);
    }

    /// Appends another record's errors, keeping their attributes and order.
    pub fn extend(&mut self, other: Self) {
        for (attribute, messages) in other.entries {
            for message in messages {
                self.add(&attribute, message);
            }
        }
    }

    /// Renames an attribute in place, so a route can report a validator's
    /// attribute under the parameter name its client reads (`content` is
    /// `instructions` on the wire).
    pub fn rename(&mut self, from: &str, to: &str) {
        let Some(index) = self
            .entries
            .iter()
            .position(|(attribute, _)| attribute == from)
        else {
            return;
        };
        let (_, messages) = self.entries.remove(index);
        if let Some((_, existing)) = self
            .entries
            .iter_mut()
            .find(|(attribute, _)| attribute == to)
        {
            existing.extend(messages);
            return;
        }
        self.entries
            .insert(index.min(self.entries.len()), (to.to_owned(), messages));
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The attributes and their messages, in append order.
    pub fn entries(&self) -> impl Iterator<Item = (&str, &[String])> {
        self.entries
            .iter()
            .map(|(attribute, messages)| (attribute.as_str(), messages.as_slice()))
    }

    /// One array of messages per attribute, in append order: the `details` value
    /// of a validation response.
    pub fn into_value(self) -> Value {
        let mut map = Map::new();
        for (attribute, messages) in self.entries {
            map.insert(
                attribute,
                Value::Array(messages.into_iter().map(Value::String).collect()),
            );
        }
        Value::Object(map)
    }
}

impl fmt::Display for FieldErrors {
    /// A single-line form for logs; HTTP responses use [`Self::into_value`].
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let messages: Vec<String> = self
            .entries()
            .map(|(attribute, messages)| format!("{attribute}: {}", messages.join(", ")))
            .collect();
        formatter.write_str(&messages.join("; "))
    }
}

impl From<FieldErrors> for Value {
    fn from(errors: FieldErrors) -> Self {
        errors.into_value()
    }
}

/// The messages of one attribute, as the areas' validators read them.
impl std::ops::Index<&str> for FieldErrors {
    type Output = [String];

    fn index(&self, attribute: &str) -> &Self::Output {
        self.entries
            .iter()
            .find(|(existing, _)| existing == attribute)
            .map(|(_, messages)| messages.as_slice())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn keeps_attribute_and_message_order() {
        let mut errors = FieldErrors::new();
        errors.add("name", "can't be blank");
        errors.add_base("something is wrong");
        errors.add("name", "is too long");
        errors.add("labels", "must be a hash");

        assert_eq!(
            errors.clone().into_value(),
            json!({
                "name": ["can't be blank", "is too long"],
                "base": ["something is wrong"],
                "labels": ["must be a hash"],
            })
        );
        let keys: Vec<&str> = errors.entries().map(|(attribute, _)| attribute).collect();
        assert_eq!(keys, ["name", "base", "labels"]);
        assert_eq!(errors["name"], ["can't be blank", "is too long"]);
        assert_eq!(errors["missing"], [] as [String; 0]);
        assert!(!errors.is_empty());
        assert!(FieldErrors::new().is_empty());
    }

    #[test]
    fn extend_appends_onto_existing_attributes() {
        let mut errors = FieldErrors::new();
        errors.add("rules", "is invalid");
        let mut nested = FieldErrors::new();
        nested.add("rules", "is invalid");
        nested.add("cidr", "is invalid");
        errors.extend(nested);
        assert_eq!(
            errors.into_value(),
            json!({
                "rules": ["is invalid", "is invalid"],
                "cidr": ["is invalid"],
            })
        );
    }

    #[test]
    fn rename_keeps_the_attribute_position() {
        let mut errors = FieldErrors::new();
        errors.add("content", "can't be blank");
        errors.add("name", "is reserved");
        errors.rename("content", "instructions");
        let keys: Vec<&str> = errors.entries().map(|(attribute, _)| attribute).collect();
        assert_eq!(keys, ["instructions", "name"]);
        assert_eq!(errors["instructions"], ["can't be blank"]);
        // A rename onto an existing attribute appends there instead.
        errors.add("content", "is too long");
        errors.rename("content", "name");
        assert_eq!(errors["name"], ["is reserved", "is too long"]);
        errors.rename("missing", "other");
        assert_eq!(errors["other"], [] as [String; 0]);
    }
}
