use strum::EnumString;

/// How a tool secret's placeholder resolves into an iron-control secret source.
/// The variant fields (`op_vault`, `ttl`) parameterize 1Password refs; consumers
/// (`centaur-iron-control`'s registry, `centaur-perms`' translator) read these
/// directly to build the right [`crate::SourceKind`]-specific source.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourcePolicy {
    pub kind: SourceKind,
    pub op_vault: String,
    pub ttl: String,
}

impl SourcePolicy {
    pub fn env() -> Self {
        Self::new(SourceKind::Env, "ai-agents", "10m")
    }

    pub fn onepassword(op_vault: impl Into<String>, ttl: impl Into<String>) -> Self {
        Self::new(SourceKind::OnePassword, op_vault, ttl)
    }

    pub fn onepassword_connect(op_vault: impl Into<String>, ttl: impl Into<String>) -> Self {
        Self::new(SourceKind::OnePasswordConnect, op_vault, ttl)
    }

    fn new(kind: SourceKind, op_vault: impl Into<String>, ttl: impl Into<String>) -> Self {
        Self {
            kind,
            op_vault: op_vault.into(),
            ttl: ttl.into(),
        }
    }
}

impl Default for SourcePolicy {
    fn default() -> Self {
        Self::env()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, EnumString)]
pub enum SourceKind {
    #[strum(serialize = "env")]
    Env,
    #[strum(serialize = "onepassword")]
    OnePassword,
    #[strum(serialize = "onepassword-connect")]
    OnePasswordConnect,
}
