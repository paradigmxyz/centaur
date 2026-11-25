//! The stateless Console session cookie.
//!
//! It is an AES-256-GCM cookie named `_centaur_console_session`. The payload
//! carries the user id, its issue time, and an optional return path. It is
//! encrypted with an authenticated cipher, so tampering, a wrong key, or an
//! expired cookie is simply an anonymous request. Unsafe requests are protected
//! by an exact `Origin` check, not by a token in this payload.
//!
//! Cookie shape: `v1.<base64url nonce>.<base64url ciphertext+tag>`. The
//! version is also part of the AEAD associated data, so a future version can
//! reject old cookies explicitly rather than misreading them.

use std::{fmt, time::Duration};

use aes_gcm::{
    Aes256Gcm, KeyInit, Nonce,
    aead::{Aead, Payload, consts::U12},
};
use axum::http::{HeaderMap, HeaderValue, header::COOKIE};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use pbkdf2::pbkdf2_hmac;
use serde::{Deserialize, Serialize};
use sha2::Sha256;

pub(crate) const SESSION_COOKIE_NAME: &str = "_centaur_console_session";
pub(crate) const SESSION_COOKIE_MAX_AGE: Duration = Duration::from_secs(30 * 24 * 60 * 60);

const VERSION: u8 = 1;
const VERSION_PREFIX: &str = "v1";
const KEY_DERIVATION_SALT: &[u8] = b"centaur-console-session-cookie-v1";
const KEY_DERIVATION_ITERATIONS: u32 = 100_000;
const AEAD_AAD: &[u8] = b"_centaur_console_session:v1";
const CLOCK_SKEW_SECONDS: i64 = 300;

/// Encrypts and decrypts Console session cookies. Debug deliberately hides
/// the derived key.
#[derive(Clone)]
pub(crate) struct SessionCookieConfig {
    secret: String,
    key: [u8; 32],
    secure: bool,
    max_age_seconds: i64,
}

impl fmt::Debug for SessionCookieConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SessionCookieConfig")
            .field("cookie_name", &SESSION_COOKIE_NAME)
            .field("secure", &self.secure)
            .field("max_age_seconds", &self.max_age_seconds)
            .finish_non_exhaustive()
    }
}

impl SessionCookieConfig {
    pub(crate) fn new(secret: &str, secure: bool) -> Self {
        let key = derive_key(secret, KEY_DERIVATION_SALT);
        Self {
            secret: secret.to_owned(),
            key,
            secure,
            max_age_seconds: SESSION_COOKIE_MAX_AGE.as_secs() as i64,
        }
    }

    /// Derives another key from the configured secret, with its own label.
    pub(crate) fn derive_key(&self, label: &[u8]) -> [u8; 32] {
        derive_key(&self.secret, label)
    }

    /// Decodes the last cookie with the Console name (duplicate cookies resolve
    /// that way), if it is authentic and still inside its 30-day lifetime.
    pub(crate) fn session_from_headers(&self, headers: &HeaderMap) -> Option<ConsoleSession> {
        let value = cookie_value(headers, SESSION_COOKIE_NAME)?;
        self.decode(value)
    }

    pub(crate) fn decode(&self, value: &str) -> Option<ConsoleSession> {
        let plaintext = open(&self.key, AEAD_AAD, value)?;
        let payload: SessionPayload = serde_json::from_slice(&plaintext).ok()?;
        if payload.v != VERSION {
            return None;
        }
        let now = time::OffsetDateTime::now_utc().unix_timestamp();
        if payload.issued_at > now + CLOCK_SKEW_SECONDS
            || now.saturating_sub(payload.issued_at) > self.max_age_seconds
        {
            return None;
        }
        Some(ConsoleSession {
            user_id: payload.user_id,
            issued_at: payload.issued_at,
            return_to: payload.return_to,
        })
    }

    /// The Set-Cookie value. A browser-session cookie: no `Max-Age`, so it
    /// disappears when the browser session ends; the payload's own issue time
    /// enforces the 30-day cap.
    pub(crate) fn set_cookie_header(&self, session: &ConsoleSession) -> Option<HeaderValue> {
        let value = self.encode(session)?;
        let mut cookie = format!("{SESSION_COOKIE_NAME}={value}; path=/; HttpOnly; SameSite=Lax");
        if self.secure {
            cookie.push_str("; Secure");
        }
        HeaderValue::from_str(&cookie).ok()
    }

    pub(crate) fn encode(&self, session: &ConsoleSession) -> Option<String> {
        self.encode_payload(&SessionPayload {
            v: VERSION,
            user_id: session.user_id,
            issued_at: session.issued_at,
            return_to: session.return_to.clone(),
        })
    }

    fn encode_payload(&self, payload: &SessionPayload) -> Option<String> {
        let plaintext = serde_json::to_vec(payload).ok()?;
        seal(&self.key, AEAD_AAD, &plaintext)
    }

    #[cfg(test)]
    pub(crate) fn with_max_age_seconds(mut self, seconds: i64) -> Self {
        self.max_age_seconds = seconds;
        self
    }
}

fn derive_key(secret: &str, label: &[u8]) -> [u8; 32] {
    let mut key = [0; 32];
    pbkdf2_hmac::<Sha256>(
        secret.as_bytes(),
        label,
        KEY_DERIVATION_ITERATIONS,
        &mut key,
    );
    key
}

/// Seals bytes as `v1.<base64url nonce>.<base64url ciphertext+tag>`.
pub(crate) fn seal(key: &[u8; 32], aad: &[u8], plaintext: &[u8]) -> Option<String> {
    let mut nonce = [0; 12];
    getrandom::fill(&mut nonce).ok()?;
    let nonce = Nonce::<U12>::from(nonce);
    let ciphertext = Aes256Gcm::new_from_slice(key)
        .ok()?
        .encrypt(
            &nonce,
            Payload {
                msg: plaintext,
                aad,
            },
        )
        .ok()?;
    Some(format!(
        "{VERSION_PREFIX}.{}.{}",
        URL_SAFE_NO_PAD.encode(nonce),
        URL_SAFE_NO_PAD.encode(ciphertext)
    ))
}

/// Opens a value produced by [`seal`].
pub(crate) fn open(key: &[u8; 32], aad: &[u8], value: &str) -> Option<Vec<u8>> {
    let mut parts = value.split('.');
    let (Some(version), Some(nonce), Some(ciphertext), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return None;
    };
    if version != VERSION_PREFIX {
        return None;
    }
    let nonce = URL_SAFE_NO_PAD.decode(nonce).ok()?;
    let nonce = <&Nonce<U12>>::try_from(nonce.as_slice()).ok()?;
    let ciphertext = URL_SAFE_NO_PAD.decode(ciphertext).ok()?;
    Aes256Gcm::new_from_slice(key)
        .ok()?
        .decrypt(
            nonce,
            Payload {
                msg: &ciphertext,
                aad,
            },
        )
        .ok()
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionPayload {
    v: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    user_id: Option<i64>,
    issued_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    return_to: Option<String>,
}

/// The decoded state of one Console session.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ConsoleSession {
    user_id: Option<i64>,
    issued_at: i64,
    return_to: Option<String>,
}

impl ConsoleSession {
    /// A new anonymous session. Sign-in rotates the payload to prevent session
    /// fixation.
    pub(crate) fn anonymous() -> Self {
        Self {
            user_id: None,
            issued_at: time::OffsetDateTime::now_utc().unix_timestamp(),
            return_to: None,
        }
    }

    pub(crate) fn signed_in(user_id: i64) -> Self {
        Self {
            user_id: Some(user_id),
            ..Self::anonymous()
        }
    }

    pub(crate) fn user_id(&self) -> Option<i64> {
        self.user_id
    }

    pub(crate) fn return_to(&self) -> Option<&str> {
        self.return_to.as_deref()
    }

    pub(crate) fn set_return_to(&mut self, return_to: impl Into<String>) {
        self.return_to = Some(return_to.into());
    }

    #[cfg(test)]
    pub(crate) fn with_issued_at(mut self, issued_at: i64) -> Self {
        self.issued_at = issued_at;
        self
    }
}

pub(crate) fn cookie_value<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    // Duplicate cookie names resolve so the last one wins.
    headers
        .get_all(COOKIE)
        .iter()
        .filter_map(|header| header.to_str().ok())
        .flat_map(|header| header.split(';'))
        .filter_map(|cookie| cookie.trim().split_once('='))
        .filter(|(key, _)| *key == name)
        .map(|(_, value)| value)
        .next_back()
}

#[cfg(test)]
mod tests {
    use axum::http::HeaderMap;

    use super::*;

    const SECRET: &str =
        "test-only-console-session-secret-0000000000000000000000000000000000000000000000000000";

    fn config() -> SessionCookieConfig {
        SessionCookieConfig::new(SECRET, true)
    }

    #[test]
    fn console_session_cookie_round_trips_state() {
        let config = config();
        let mut session = ConsoleSession::signed_in(42);
        session.set_return_to("/console/credentials?kind=oauth");
        let encoded = config.encode(&session).unwrap();
        assert!(
            !encoded.contains("\"user_id\""),
            "cookie payload must be encrypted"
        );
        assert_eq!(config.decode(&encoded).unwrap(), session);
    }

    #[test]
    fn console_session_cookie_prefers_the_last_duplicate_cookie() {
        let config = config();
        let first = ConsoleSession::anonymous();
        let last = ConsoleSession::signed_in(7);
        let first = config.encode(&first).unwrap();
        let last = config.encode(&last).unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(
            COOKIE,
            format!("_centaur_console_session={first}; _centaur_console_session={last}")
                .parse()
                .unwrap(),
        );
        assert_eq!(
            config.session_from_headers(&headers).unwrap().user_id(),
            Some(7)
        );
    }

    #[test]
    fn console_session_cookie_rejects_tampering_and_wrong_keys() {
        let config = config();
        let session = ConsoleSession::signed_in(42);
        let encoded = config.encode(&session).unwrap();
        for index in [0, 4, encoded.len() / 2, encoded.len() - 1] {
            let mut bytes = encoded.clone().into_bytes();
            bytes[index] = if bytes[index] == b'A' { b'B' } else { b'A' };
            let tampered = String::from_utf8(bytes).unwrap();
            assert_eq!(config.decode(&tampered), None, "{index}");
        }
        assert_eq!(
            SessionCookieConfig::new("different-secret", true).decode(&encoded),
            None
        );
        assert_eq!(config.decode("v2.a.b"), None);
        assert_eq!(config.decode("not-a-cookie"), None);
    }

    #[test]
    fn console_session_cookie_expires_after_its_lifetime() {
        let config = config();
        let now = time::OffsetDateTime::now_utc().unix_timestamp();
        let live = config
            .encode(&ConsoleSession::anonymous().with_issued_at(now - 60))
            .unwrap();
        assert!(config.decode(&live).is_some());
        let expired = config
            .encode(
                &ConsoleSession::anonymous()
                    .with_issued_at(now - SESSION_COOKIE_MAX_AGE.as_secs() as i64 - 1),
            )
            .unwrap();
        assert_eq!(config.decode(&expired), None);
        let future = config
            .encode(&ConsoleSession::anonymous().with_issued_at(now + 3600))
            .unwrap();
        assert_eq!(config.decode(&future), None);
        let short = config.with_max_age_seconds(10);
        let old = short
            .encode(&ConsoleSession::anonymous().with_issued_at(now - 11))
            .unwrap();
        assert_eq!(short.decode(&old), None);
    }

    #[test]
    fn console_session_cookie_header_is_httponly_lax_and_secure_only_over_https() {
        let session = ConsoleSession::anonymous();
        let secure = config()
            .set_cookie_header(&session)
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned();
        assert!(secure.starts_with("_centaur_console_session="));
        assert!(secure.contains("; path=/;"));
        assert!(secure.contains("; HttpOnly;"));
        assert!(secure.contains("; SameSite=Lax"));
        assert!(secure.ends_with("; Secure"));
        assert!(!secure.contains("Max-Age"));

        let insecure = SessionCookieConfig::new(SECRET, false)
            .set_cookie_header(&session)
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned();
        assert!(!insecure.contains("Secure"));
    }

    #[test]
    fn console_session_cookie_reads_only_its_own_name() {
        let config = config();
        let session = ConsoleSession::signed_in(7);
        let value = config.encode(&session).unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(
            COOKIE,
            format!("theme=dark; other={value}; {SESSION_COOKIE_NAME}={value}")
                .parse()
                .unwrap(),
        );
        assert_eq!(config.session_from_headers(&headers), Some(session));
    }

    #[test]
    fn console_session_clock_skew_is_bounded() {
        let config = config();
        let now = time::OffsetDateTime::now_utc().unix_timestamp();
        let slightly_future = config
            .encode(&ConsoleSession::anonymous().with_issued_at(now + 60))
            .unwrap();
        assert!(config.decode(&slightly_future).is_some());
    }
}
