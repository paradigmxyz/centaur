//! The console session: an encrypted, authenticated cookie (AES-256-GCM
//! through `cookie`'s private jar) holding the signed-in user id, the issue
//! time, a CSRF secret, and one-shot flash/return-path values. The key is
//! derived from `SECRET_KEY_BASE`. Sessions expire absolutely `max_age`
//! after they were issued, and are re-issued (new CSRF secret) on sign-in.

use chrono::{DateTime, Duration, Utc};
use cookie::{Cookie, CookieJar, Key, SameSite};
use hmac::{Hmac, KeyInit, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

pub const COOKIE_NAME: &str = "_centaur_console";
pub const DEFAULT_MAX_AGE: Duration = Duration::days(14);

/// Derives a named subkey from `SECRET_KEY_BASE` (HMAC-SHA256 of the name).
pub fn derive_key(secret_key_base: &str, name: &str) -> [u8; 32] {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret_key_base.as_bytes()).expect("any key size");
    mac.update(b"centaur-console/");
    mac.update(name.as_bytes());
    mac.finalize().into_bytes().into()
}

/// One-shot messages for the next page or `/me` response.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Flash {
    pub notice: Option<String>,
    pub alert: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
struct Data {
    #[serde(default)]
    user_id: Option<i64>,
    issued_at: i64,
    csrf: String,
    #[serde(default)]
    flash: Flash,
    #[serde(default)]
    return_to: Option<String>,
}

impl Data {
    fn fresh(now: DateTime<Utc>) -> Self {
        Self {
            user_id: None,
            issued_at: now.timestamp(),
            csrf: crate::random::urlsafe_base64_32(),
            flash: Flash::default(),
            return_to: None,
        }
    }
}

/// How sessions are encrypted and expire. `Debug` hides the key.
#[derive(Clone)]
pub struct SessionConfig {
    key: Key,
    pub max_age: Duration,
}

impl std::fmt::Debug for SessionConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionConfig")
            .field("key", &"[REDACTED]")
            .field("max_age", &self.max_age)
            .finish()
    }
}

impl SessionConfig {
    pub fn new(secret_key_base: &str) -> Self {
        let master = derive_key(secret_key_base, "session");
        let mut expanded = [0u8; 64];
        expanded[..32].copy_from_slice(&master);
        expanded[32..].copy_from_slice(&derive_key(secret_key_base, "session-2"));
        Self {
            key: Key::from(&expanded),
            max_age: DEFAULT_MAX_AGE,
        }
    }

    /// The session in a request's `Cookie` header: a fresh, unsaved one
    /// when the cookie is absent, forged, malformed, or past its expiry.
    pub fn load(&self, cookie_header: Option<&str>, now: DateTime<Utc>) -> Session {
        let stored = cookie_header.and_then(|header| {
            let mut jar = CookieJar::new();
            for cookie in Cookie::split_parse(header.to_owned()).flatten() {
                if cookie.name() == COOKIE_NAME {
                    jar.add_original(cookie.into_owned());
                }
            }
            let cookie = jar.private(&self.key).get(COOKIE_NAME)?;
            let data: Data = serde_json::from_str(cookie.value()).ok()?;
            let age = now.timestamp() - data.issued_at;
            (age >= 0 && age < self.max_age.num_seconds()).then_some(data)
        });
        match stored {
            Some(data) => Session {
                data,
                state: State::Loaded,
            },
            None => Session {
                data: Data::fresh(now),
                state: State::Fresh,
            },
        }
    }

    /// The `Set-Cookie` value for `session`, when it changed this request.
    /// `secure` is whether the request or public URL is HTTPS.
    pub fn set_cookie(
        &self,
        session: &Session,
        secure: bool,
        now: DateTime<Utc>,
    ) -> Option<String> {
        if session.state != State::Changed {
            return None;
        }
        let value = serde_json::to_string(&session.data).expect("session serializes");
        let remaining =
            (session.data.issued_at + self.max_age.num_seconds() - now.timestamp()).max(0);
        let cookie = Cookie::build((COOKIE_NAME, value))
            .path("/")
            .http_only(true)
            .same_site(SameSite::Lax)
            .secure(secure)
            .max_age(cookie::time::Duration::seconds(remaining))
            .build();
        let mut jar = CookieJar::new();
        jar.private_mut(&self.key).add(cookie);
        jar.delta().next().map(|cookie| cookie.to_string())
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    /// No valid cookie; nothing to save until something is stored.
    Fresh,
    Loaded,
    Changed,
}

/// One request's session. `Debug` hides everything.
#[derive(Clone)]
pub struct Session {
    data: Data,
    state: State,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("signed_in", &self.data.user_id.is_some())
            .finish_non_exhaustive()
    }
}

impl Session {
    /// A fresh anonymous session (tests).
    pub fn anonymous(now: DateTime<Utc>) -> Self {
        Self {
            data: Data::fresh(now),
            state: State::Fresh,
        }
    }

    fn touch(&mut self) {
        self.state = State::Changed;
    }

    pub fn user_id(&self) -> Option<i64> {
        self.data.user_id
    }

    /// The CSRF secret. Reading it from a fresh session saves the session,
    /// so the token the page shows is the one the next request is checked
    /// against.
    pub fn csrf_token(&mut self) -> &str {
        if self.state == State::Fresh {
            self.touch();
        }
        &self.data.csrf
    }

    /// Constant-time check of a presented CSRF token.
    pub fn csrf_matches(&self, presented: Option<&str>) -> bool {
        presented.is_some_and(|presented| crate::csrf::eq(presented, &self.data.csrf))
    }

    /// Signs `user_id` in with a new CSRF secret and issue time (no session
    /// fixation). The remembered return path survives; flash does not.
    pub fn sign_in(&mut self, user_id: i64, now: DateTime<Utc>) {
        let return_to = self.data.return_to.take();
        self.data = Data::fresh(now);
        self.data.user_id = Some(user_id);
        self.data.return_to = return_to;
        self.touch();
    }

    /// Clears everything and issues a new CSRF secret.
    pub fn sign_out(&mut self, now: DateTime<Utc>) {
        self.data = Data::fresh(now);
        self.touch();
    }

    pub fn set_notice(&mut self, message: impl Into<String>) {
        self.data.flash.notice = Some(message.into());
        self.touch();
    }

    pub fn set_alert(&mut self, message: impl Into<String>) {
        self.data.flash.alert = Some(message.into());
        self.touch();
    }

    /// Takes the flash (once).
    pub fn take_flash(&mut self) -> Flash {
        if self.data.flash == Flash::default() {
            return Flash::default();
        }
        self.touch();
        std::mem::take(&mut self.data.flash)
    }

    pub fn set_return_to(&mut self, path: String) {
        self.data.return_to = Some(path);
        self.touch();
    }

    pub fn return_to(&self) -> Option<&str> {
        self.data.return_to.as_deref()
    }

    pub fn take_return_to(&mut self) -> Option<String> {
        let taken = self.data.return_to.take();
        if taken.is_some() {
            self.touch();
        }
        taken
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> DateTime<Utc> {
        Utc::now()
    }

    fn cookie_header(set_cookie: &str) -> String {
        set_cookie.split(';').next().unwrap().to_owned()
    }

    #[test]
    fn round_trips_and_only_saves_when_changed() {
        let config = SessionConfig::new("secret-key-base");
        let mut session = config.load(None, now());
        assert_eq!(
            config.set_cookie(&session, true, now()),
            None,
            "nothing stored yet"
        );
        let csrf = session.csrf_token().to_owned();
        session.sign_in(42, now());
        let new_csrf = session.csrf_token().to_owned();
        assert_ne!(csrf, new_csrf, "sign-in renews the CSRF secret");
        let set_cookie = config.set_cookie(&session, true, now()).unwrap();
        for attribute in ["HttpOnly", "SameSite=Lax", "Secure", "Path=/", "Max-Age="] {
            assert!(set_cookie.contains(attribute), "{attribute}: {set_cookie}");
        }
        let header = format!("other=1; {}", cookie_header(&set_cookie));
        let loaded = config.load(Some(&header), now());
        assert_eq!(loaded.user_id(), Some(42));
        assert!(loaded.csrf_matches(Some(&new_csrf)));
        assert!(!loaded.csrf_matches(Some(&csrf)));
        assert!(!loaded.csrf_matches(None));
        assert_eq!(config.set_cookie(&loaded, true, now()), None, "unchanged");
    }

    #[test]
    fn rejects_forged_foreign_and_expired_cookies() {
        let config = SessionConfig::new("secret-key-base");
        let mut session = config.load(None, now());
        session.sign_in(7, now());
        let header = cookie_header(&config.set_cookie(&session, false, now()).unwrap());
        // Another deployment's key.
        let other = SessionConfig::new("another-secret");
        assert_eq!(other.load(Some(&header), now()).user_id(), None);
        // Tampered ciphertext.
        let tampered = format!("{}x", header);
        assert_eq!(config.load(Some(&tampered), now()).user_id(), None);
        // Past the absolute expiry.
        let later = now() + DEFAULT_MAX_AGE + Duration::seconds(1);
        assert_eq!(config.load(Some(&header), later).user_id(), None);
        // The value is not readable.
        assert!(!header.contains("\"user_id\""));
    }

    #[test]
    fn flash_and_return_path_are_one_shot_and_return_path_survives_sign_in() {
        let config = SessionConfig::new("k");
        let mut session = config.load(None, now());
        session.set_return_to("/console".into());
        session.set_alert("nope");
        session.sign_in(1, now());
        assert_eq!(session.take_flash(), Flash::default());
        assert_eq!(session.take_return_to().as_deref(), Some("/console"));
        assert_eq!(session.take_return_to(), None);
        session.set_notice("hi");
        assert_eq!(session.take_flash().notice.as_deref(), Some("hi"));
        assert_eq!(session.take_flash(), Flash::default());
        session.sign_out(now());
        assert_eq!(session.user_id(), None);
        assert!(!format!("{session:?} {config:?}").contains("secret"));
    }
}
