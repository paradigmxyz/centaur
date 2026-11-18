//! CSRF defences for browser requests: a constant-time token compare (the
//! token is the session's CSRF secret, see [`super::session`]) and a
//! same-origin check of the `Origin` header.

use subtle::ConstantTimeEq;

/// Constant-time string equality.
pub fn eq(a: &str, b: &str) -> bool {
    a.len() == b.len() && bool::from(a.as_bytes().ct_eq(b.as_bytes()))
}

/// What the origin check needs from the request.
#[derive(Clone, Copy, Debug, Default)]
pub struct RequestOrigin<'a> {
    /// The `Origin` header, if sent.
    pub origin: Option<&'a str>,
    /// The request's authority: `X-Forwarded-Host`, else `Host`.
    pub host: Option<&'a str>,
    /// `CENTAUR_CONSOLE_PUBLIC_URL`, when set.
    pub public_url: Option<&'a str>,
}

impl RequestOrigin<'_> {
    /// Whether a state-changing request came from this site. A request
    /// without an `Origin` header fails even with a valid CSRF token:
    /// browsers send `Origin` on every state-changing request, so its
    /// absence means the request's origin cannot be checked. `Origin: null`
    /// and cross-site origins fail too.
    pub fn same_origin(&self) -> bool {
        let Some(origin) = self.origin else {
            return false;
        };
        let Ok(origin) = url::Url::parse(origin) else {
            return false;
        };
        if !matches!(origin.scheme(), "http" | "https") || origin.host_str().is_none() {
            return false;
        }
        let authority = |url: &url::Url| {
            Some(match url.port() {
                Some(port) => format!("{}:{port}", url.host_str()?.to_ascii_lowercase()),
                None => url.host_str()?.to_ascii_lowercase(),
            })
        };
        if let Some(public) = self.public_url.and_then(|url| url::Url::parse(url).ok())
            && public.scheme() == origin.scheme()
            && authority(&public) == authority(&origin)
        {
            return true;
        }
        let Some(host) = self.host else {
            return false;
        };
        let Ok(host) = url::Url::parse(&format!("{}://{host}", origin.scheme())) else {
            return false;
        };
        authority(&host) == authority(&origin)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(origin: Option<&str>, host: Option<&str>, public_url: Option<&str>) -> bool {
        RequestOrigin {
            origin,
            host,
            public_url,
        }
        .same_origin()
    }

    #[test]
    fn origin_must_match_the_host_or_public_url() {
        assert!(!check(None, Some("console.test"), None));
        assert!(!check(
            None,
            Some("console.test"),
            Some("https://console.test")
        ));
        assert!(check(
            Some("https://console.test"),
            Some("console.test"),
            None
        ));
        assert!(check(
            Some("http://localhost:3000"),
            Some("localhost:3000"),
            None
        ));
        assert!(!check(
            Some("https://evil.test"),
            Some("console.test"),
            None
        ));
        assert!(!check(Some("null"), Some("console.test"), None));
        assert!(!check(
            Some("https://console.test.evil.test"),
            Some("console.test"),
            None
        ));
        assert!(!check(
            Some("https://console.test:8443"),
            Some("console.test"),
            None
        ));
        assert!(check(
            Some("https://console.example"),
            Some("pod-1:3000"),
            Some("https://console.example")
        ));
        assert!(!check(
            Some("http://console.example"),
            Some("pod-1:3000"),
            Some("https://console.example")
        ));
        assert!(!check(Some("https://console.test"), None, None));
    }

    #[test]
    fn compares_tokens_in_constant_time_shape() {
        assert!(eq("abc", "abc"));
        assert!(!eq("abc", "abd"));
        assert!(!eq("abc", "abcd"));
    }
}
