//! The paths a browser may be sent to after signing in (`?return_to=`, the
//! remembered deep link, the PWA launch target). Anything else is dropped,
//! which closes the open redirect: only `/`, the SPA under `/console`,
//! `/mcp/oauth/authorize`, `/oauth/<slug>/{start,callback}`, and `/launch`,
//! with the query kept and the fragment removed.

pub const MAX_LENGTH: usize = 4096;

/// The normalized path (with its query) when `raw` is an allowed
/// same-origin return path.
pub fn safe(raw: &str) -> Option<String> {
    if raw.is_empty() || raw.len() > MAX_LENGTH {
        return None;
    }
    let raw = raw.split('#').next().unwrap_or_default();
    let (path, query) = match raw.split_once('?') {
        Some((path, query)) => (path, Some(query)),
        None => (raw, None),
    };
    let plain = |c: char| !(c.is_control() || c.is_whitespace());
    if !path.chars().all(|c| plain(c) && c != '\\')
        || !query.is_none_or(|query| query.chars().all(plain))
    {
        return None;
    }
    // Same-origin absolute paths only: no `//host`, no dot segments (also
    // percent-encoded), only RFC 3986 path characters.
    if !path.starts_with('/')
        || path.starts_with("//")
        || path.to_ascii_lowercase().contains("%2e")
        || path
            .split('/')
            .any(|segment| segment == "." || segment == "..")
        || !path
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-._~!$&'()*+,;=:@/%".contains(&b))
    {
        return None;
    }
    allowed(path).then(|| match query {
        Some(query) => format!("{path}?{query}"),
        None => path.to_owned(),
    })
}

fn allowed(path: &str) -> bool {
    let path = path.strip_suffix(".html").unwrap_or(path);
    if matches!(path, "/" | "/launch" | "/mcp/oauth/authorize" | "/console") {
        return true;
    }
    if let Some(rest) = path.strip_prefix("/console/") {
        return rest
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-./~%:@".contains(&b));
    }
    if let Some(rest) = path.strip_prefix("/oauth/")
        && let Some((slug, action)) = rest.split_once('/')
    {
        return !slug.is_empty()
            && slug
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-._~".contains(&b))
            && matches!(action, "start" | "callback");
    }
    false
}

#[cfg(test)]
mod tests {
    use super::safe;

    #[test]
    fn allows_console_and_session_gated_entry_points() {
        for (raw, expected) in [
            ("/", "/"),
            ("/console", "/console"),
            (
                "/console/principals/prn_abc?tab=grants#top",
                "/console/principals/prn_abc?tab=grants",
            ),
            (
                "/mcp/oauth/authorize?client_id=x&state=y",
                "/mcp/oauth/authorize?client_id=x&state=y",
            ),
            ("/oauth/google-docs/start", "/oauth/google-docs/start"),
            (
                "/oauth/slack/callback?code=1",
                "/oauth/slack/callback?code=1",
            ),
            (
                "/launch?target=web%2Bcentaur://console",
                "/launch?target=web%2Bcentaur://console",
            ),
        ] {
            assert_eq!(safe(raw).as_deref(), Some(expected), "{raw}");
        }
    }

    #[test]
    fn refuses_everything_else() {
        for raw in [
            "",
            "#frag",
            "console",
            "//evil.test/console",
            "/\\evil.test",
            "/console/../api/v1/console/me",
            "/console/%2e%2e/api",
            "/console/a b",
            "/console\n/x",
            "https://evil.test/console",
            "/api/v1/console/me",
            "/logout",
            "/oauth/x/y/start",
            "/oauth//start",
            "/console?x=a b",
        ] {
            assert_eq!(safe(raw), None, "{raw:?}");
        }
        assert_eq!(safe(&format!("/console/{}", "a".repeat(5000))), None);
    }
}
