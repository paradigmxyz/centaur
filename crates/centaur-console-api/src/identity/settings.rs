//! Console sign-in settings from the environment: per provider
//! `CENTAUR_CONSOLE_<PROVIDER>_CLIENT_ID` / `_CLIENT_SECRET` (and
//! `_OKTA_ISSUER`, `_OKTA_TOKEN_ENDPOINT_AUTH_METHOD`), plus
//! `SSO_EMAIL_DOMAINS`, `BOOTSTRAP_ADMINS`, and `PASSWORD_LOGIN_ENABLED`.
//! A provider appears on the login page only when fully configured.

use crate::env::Env;

/// Supported providers, in login-page order.
pub const PROVIDERS: [&str; 3] = ["google", "slack", "okta"];

/// `provider` configuration `field` (`CLIENT_ID`, ...), when non-blank.
pub fn setting<'a>(env: &'a Env, provider: &str, field: &str) -> Option<&'a str> {
    env.present(&format!("{}_{field}", provider.to_ascii_uppercase()))
}

pub fn configured(env: &Env, provider: &str) -> bool {
    PROVIDERS.contains(&provider)
        && setting(env, provider, "CLIENT_ID").is_some()
        && setting(env, provider, "CLIENT_SECRET").is_some()
        && (provider != "okta" || setting(env, provider, "ISSUER").is_some())
}

/// The configured providers.
pub fn providers(env: &Env) -> Vec<&'static str> {
    PROVIDERS
        .into_iter()
        .filter(|provider| configured(env, provider))
        .collect()
}

/// A boolean setting: anything but empty, `0`, `false`, `f`, `off`, `no`.
pub fn truthy(raw: &str) -> bool {
    !matches!(
        raw.trim().to_ascii_lowercase().as_str(),
        "" | "0" | "false" | "f" | "off" | "no"
    )
}

/// Unset enables password sign-in; an empty value disables it.
pub fn password_login_enabled(env: &Env) -> bool {
    env.get("PASSWORD_LOGIN_ENABLED").is_none_or(truthy)
}

pub fn api_clients_enabled(env: &Env) -> bool {
    env.get("API_CLIENTS_ENABLED").is_some_and(truthy)
}

/// Who may sign in with SSO and who becomes an admin on first sign-in.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SsoPolicy {
    /// Lower-case domains; empty allows every domain.
    pub email_domains: Vec<String>,
    /// Lower-case emails that start as admins.
    pub bootstrap_admins: Vec<String>,
}

fn list(raw: Option<&str>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for item in raw
        .unwrap_or_default()
        .split(|c: char| c == ',' || c.is_whitespace())
        .map(|item| item.trim().to_lowercase())
        .filter(|item| !item.is_empty())
    {
        if !out.contains(&item) {
            out.push(item);
        }
    }
    out
}

impl SsoPolicy {
    pub fn from_env(env: &Env) -> Self {
        Self {
            email_domains: list(env.present("SSO_EMAIL_DOMAINS")),
            bootstrap_admins: list(env.present("BOOTSTRAP_ADMINS")),
        }
    }

    pub fn email_allowed(&self, email: Option<&str>) -> bool {
        if self.email_domains.is_empty() {
            return true;
        }
        let email = email.unwrap_or_default().trim().to_lowercase();
        let Some((_, domain)) = email.split_once('@') else {
            return false;
        };
        self.email_domains.iter().any(|allowed| allowed == domain)
    }

    pub fn bootstrap_admin(&self, email: Option<&str>) -> bool {
        let email = email.unwrap_or_default().trim().to_lowercase();
        !email.is_empty() && self.bootstrap_admins.contains(&email)
    }
}

pub fn capitalize(provider: &str) -> String {
    let mut chars = provider.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn providers_need_credentials_and_okta_an_issuer() {
        let env = Env::from_pairs([
            ("CENTAUR_CONSOLE_GOOGLE_CLIENT_ID", "g"),
            ("CENTAUR_CONSOLE_GOOGLE_CLIENT_SECRET", "s"),
            ("CENTAUR_CONSOLE_SLACK_CLIENT_ID", "only-id"),
            ("CENTAUR_CONSOLE_OKTA_CLIENT_ID", "o"),
            ("CENTAUR_CONSOLE_OKTA_CLIENT_SECRET", "s"),
        ]);
        assert_eq!(providers(&env), ["google"]);
        let env = Env::from_pairs([
            ("IRON_CONTROL_OKTA_CLIENT_ID", "o"),
            ("IRON_CONTROL_OKTA_CLIENT_SECRET", "s"),
            ("CENTAUR_CONSOLE_OKTA_ISSUER", "https://idp.example.test"),
        ]);
        assert_eq!(providers(&env), ["okta"]);
    }

    #[test]
    fn password_login_toggle() {
        assert!(password_login_enabled(&Env::default()));
        for (raw, enabled) in [
            ("false", false),
            ("0", false),
            ("", false),
            ("true", true),
            ("yes", true),
        ] {
            let env = Env::from_pairs([("CENTAUR_CONSOLE_PASSWORD_LOGIN_ENABLED", raw)]);
            assert_eq!(password_login_enabled(&env), enabled, "{raw:?}");
        }
    }

    #[test]
    fn sso_policy_lists() {
        let env = Env::from_pairs([
            (
                "CENTAUR_CONSOLE_SSO_EMAIL_DOMAINS",
                "Acme.example, example.org\texample.org",
            ),
            (
                "CENTAUR_CONSOLE_BOOTSTRAP_ADMINS",
                " Boss@Acme.example ,,you@acme.example",
            ),
        ]);
        let policy = SsoPolicy::from_env(&env);
        assert_eq!(policy.email_domains, ["acme.example", "example.org"]);
        assert!(policy.email_allowed(Some(" Worker@ACME.example ")));
        assert!(!policy.email_allowed(Some("worker@other.example")));
        assert!(!policy.email_allowed(None));
        assert!(policy.bootstrap_admin(Some("BOSS@acme.example")));
        assert!(SsoPolicy::default().email_allowed(None));
        assert_eq!(capitalize("okta"), "Okta");
    }
}
