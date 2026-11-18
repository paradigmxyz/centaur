//! `ConsoleEnv`: the console's environment under `CENTAUR_CONSOLE_*`, falling
//! back to the legacy `IRON_CONTROL_*` name when the canonical one is unset.
//! A canonical variable set to the empty string wins over a legacy one
//! (plain `ENV` semantics); callers use [`Env::present`] where empty means
//! unset (`.presence`).
//!
//! The environment is captured once into an [`Env`] so configuration is
//! testable without mutating the process environment.

use std::collections::BTreeMap;

pub const PREFIX: &str = "CENTAUR_CONSOLE";
pub const LEGACY_PREFIX: &str = "IRON_CONTROL";

/// A snapshot of the process environment. `Debug` shows variable names,
/// never values.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Env {
    vars: BTreeMap<String, String>,
}

impl std::fmt::Debug for Env {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Env")
            .field("vars", &centaur_console_db::redact::Keys(self.vars.keys()))
            .finish()
    }
}

impl Env {
    /// The current process environment.
    pub fn from_process() -> Self {
        Self {
            vars: std::env::vars().collect(),
        }
    }

    pub fn from_pairs<'a>(pairs: impl IntoIterator<Item = (&'a str, &'a str)>) -> Self {
        Self {
            vars: pairs
                .into_iter()
                .map(|(key, value)| (key.to_owned(), value.to_owned()))
                .collect(),
        }
    }

    /// `ConsoleEnv.key(suffix)`: the canonical name (for messages).
    pub fn key(suffix: &str) -> String {
        format!("{PREFIX}_{suffix}")
    }

    /// `ConsoleEnv.legacy_key(suffix)`.
    pub fn legacy_key(suffix: &str) -> String {
        format!("{LEGACY_PREFIX}_{suffix}")
    }

    /// `ENV[name]`.
    pub fn var(&self, name: &str) -> Option<&str> {
        self.vars.get(name).map(String::as_str)
    }

    /// `ENV[name].presence`.
    pub fn var_present(&self, name: &str) -> Option<&str> {
        self.var(name).filter(|value| !value.trim().is_empty())
    }

    /// `ConsoleEnv[suffix]`.
    pub fn get(&self, suffix: &str) -> Option<&str> {
        self.var(&Self::key(suffix))
            .or_else(|| self.var(&Self::legacy_key(suffix)))
    }

    /// `ConsoleEnv[suffix].presence`.
    pub fn present(&self, suffix: &str) -> Option<&str> {
        self.get(suffix).filter(|value| !value.trim().is_empty())
    }

    /// `ConsoleEnv.fetch(suffix, default)`.
    pub fn fetch<'a>(&'a self, suffix: &str, default: &'a str) -> &'a str {
        self.get(suffix).unwrap_or(default)
    }

    /// `ConsoleEnv.fetch(suffix)` without a default: the canonical key in
    /// the error, as Rails' KeyError names both.
    pub fn require(&self, suffix: &str) -> Result<&str, String> {
        self.get(suffix).ok_or_else(|| {
            format!(
                "neither {} nor {} is set",
                Self::key(suffix),
                Self::legacy_key(suffix)
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::Env;

    // Ported from test/lib/console_env_test.rb.
    #[test]
    fn reads_the_canonical_then_the_legacy_variable() {
        let env = Env::from_pairs([("CENTAUR_CONSOLE_WIDGET", "new")]);
        assert_eq!(env.get("WIDGET"), Some("new"));
        let env = Env::from_pairs([("IRON_CONTROL_WIDGET", "legacy")]);
        assert_eq!(env.get("WIDGET"), Some("legacy"));
        let env = Env::from_pairs([
            ("CENTAUR_CONSOLE_WIDGET", "new"),
            ("IRON_CONTROL_WIDGET", "legacy"),
        ]);
        assert_eq!(env.get("WIDGET"), Some("new"));
        assert_eq!(Env::default().get("WIDGET"), None);
    }

    #[test]
    fn an_empty_canonical_variable_wins_but_is_not_present() {
        let env = Env::from_pairs([
            ("CENTAUR_CONSOLE_WIDGET", ""),
            ("IRON_CONTROL_WIDGET", "legacy"),
        ]);
        assert_eq!(env.get("WIDGET"), Some(""));
        assert_eq!(env.present("WIDGET"), None);
    }

    #[test]
    fn fetch_and_require() {
        assert_eq!(Env::default().fetch("WIDGET", "5432"), "5432");
        let env = Env::from_pairs([("IRON_CONTROL_WIDGET", "legacy")]);
        assert_eq!(env.fetch("WIDGET", "default"), "legacy");
        assert_eq!(
            Env::default().require("WIDGET").unwrap_err(),
            "neither CENTAUR_CONSOLE_WIDGET nor IRON_CONTROL_WIDGET is set"
        );
        assert_eq!(Env::key("WIDGET"), "CENTAUR_CONSOLE_WIDGET");
        assert_eq!(Env::legacy_key("WIDGET"), "IRON_CONTROL_WIDGET");
    }
}
