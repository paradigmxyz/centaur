//! JSON Schema (draft 2020-12) validation of config documents. Errors are
//! `"<instance path or (root)> <message>"`, suitable for a field error.

use jsonschema::Validator;
use serde_json::Value;

/// A compiled schema.
pub struct Schema(Validator);

impl Schema {
    /// Compiles `schema`; panics on an invalid schema (schemas are
    /// compiled-in constants).
    pub fn new(schema: &Value) -> Self {
        Self(jsonschema::validator_for(schema).expect("a valid JSON Schema"))
    }

    /// Every violation in `instance`; empty when it is valid.
    pub fn errors(&self, instance: &Value) -> Vec<String> {
        self.0
            .iter_errors(instance)
            .map(|error| {
                let path = error.instance_path().to_string();
                let path = if path.is_empty() {
                    "(root)".to_owned()
                } else {
                    path
                };
                format!("{path} {error}")
            })
            .collect()
    }

    pub fn is_valid(&self, instance: &Value) -> bool {
        self.0.is_valid(instance)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn reports_violations_with_their_paths() {
        let schema = Schema::new(&json!({
            "type": "object",
            "required": ["header"],
            "properties": {"header": {"type": "string", "minLength": 1}, "hosts": {"type": "array", "items": {"type": "string"}}},
            "additionalProperties": false,
        }));
        assert!(schema.is_valid(&json!({"header": "Authorization", "hosts": ["a.test"]})));
        assert!(schema.errors(&json!({"header": "x"})).is_empty());
        let errors = schema.errors(&json!({"header": "", "hosts": [1], "extra": true}));
        assert_eq!(errors.len(), 3, "{errors:?}");
        assert!(errors.iter().any(|error| error.starts_with("/header ")));
        assert!(errors.iter().any(|error| error.starts_with("/hosts/0 ")));
        assert!(errors.iter().any(|error| error.starts_with("(root) ")));
    }
}
