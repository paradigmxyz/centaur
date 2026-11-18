//! Short-lived encrypted cookies that bind an in-flight redirect flow (SSO,
//! OAuth) to the browser that started it.

use chrono::Duration;
use cookie::{Cookie, CookieJar, Key, SameSite};
use serde::{Serialize, de::DeserializeOwned};

use super::session::derive_key;

#[derive(Clone)]
pub struct FlowCookies(Key);

impl std::fmt::Debug for FlowCookies {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("FlowCookies([REDACTED])")
    }
}

impl FlowCookies {
    pub fn new(secret_key_base: &str) -> Self {
        let mut expanded = [0u8; 64];
        expanded[..32].copy_from_slice(&derive_key(secret_key_base, "flow"));
        expanded[32..].copy_from_slice(&derive_key(secret_key_base, "flow-2"));
        Self(Key::from(&expanded))
    }

    /// A `Set-Cookie` value holding `value` for `ttl`. `Lax`, because the
    /// callback arrives through a top-level cross-site redirect.
    pub fn set(&self, name: &str, value: &impl Serialize, ttl: Duration, secure: bool) -> String {
        let cookie = Cookie::build((
            name.to_owned(),
            serde_json::to_string(value).expect("flow value serializes"),
        ))
        .path("/")
        .http_only(true)
        .same_site(SameSite::Lax)
        .secure(secure)
        .max_age(cookie::time::Duration::seconds(ttl.num_seconds()))
        .build();
        let mut jar = CookieJar::new();
        jar.private_mut(&self.0).add(cookie);
        jar.delta().next().expect("cookie was added").to_string()
    }

    /// The value in a request's `Cookie` header, when it decrypts.
    pub fn read<T: DeserializeOwned>(&self, name: &str, cookie_header: Option<&str>) -> Option<T> {
        let mut jar = CookieJar::new();
        for cookie in Cookie::split_parse(cookie_header?.to_owned()).flatten() {
            if cookie.name() == name {
                jar.add_original(cookie.into_owned());
            }
        }
        let cookie = jar.private(&self.0).get(name)?;
        serde_json::from_str(cookie.value()).ok()
    }

    /// A `Set-Cookie` value that deletes the cookie.
    pub fn clear(&self, name: &str) -> String {
        Cookie::build((name.to_owned(), ""))
            .path("/")
            .max_age(cookie::time::Duration::ZERO)
            .build()
            .to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_rejects_other_keys() {
        let cookies = FlowCookies::new("k");
        let set = cookies.set("flow", &["nonce", "verifier"], Duration::minutes(10), true);
        assert!(set.contains("HttpOnly") && set.contains("SameSite=Lax") && set.contains("Secure"));
        assert!(!set.contains("verifier"));
        let header = set.split(';').next().unwrap();
        assert_eq!(
            cookies.read::<Vec<String>>("flow", Some(header)),
            Some(vec!["nonce".into(), "verifier".into()])
        );
        assert_eq!(
            FlowCookies::new("other").read::<Vec<String>>("flow", Some(header)),
            None
        );
        assert_eq!(cookies.read::<Vec<String>>("flow", None), None);
        assert!(cookies.clear("flow").contains("Max-Age=0"));
    }
}
