//! Same-origin return paths for post-login redirects.
//!
//! A return path is a path on the Console origin. [`safe`] resolves the value
//! against a fixed base URL and returns the resolved path and query only when
//! the origin is unchanged, so an absolute URL, a protocol-relative value, or a
//! backslash/whitespace/control-character trick cannot leave the origin.

use url::Url;

const MAX_LENGTH: usize = 4096;
const BASE_URL: &str = "https://console.invalid/";

/// The normalized path (with its query) when `raw` is a same-origin return
/// path, else `None`.
pub(crate) fn safe(raw: &str) -> Option<String> {
    if raw.is_empty() || raw.chars().count() > MAX_LENGTH {
        return None;
    }
    if raw
        .chars()
        .any(|character| character == '\\' || character.is_control() || character.is_whitespace())
    {
        return None;
    }
    if !raw.starts_with('/') || raw.starts_with("//") {
        return None;
    }
    let base = Url::parse(BASE_URL).ok()?;
    let resolved = base.join(raw).ok()?;
    if resolved.origin() != base.origin() {
        return None;
    }
    let mut value = resolved.path().to_owned();
    // Dot segments can collapse to a leading `//`, which a browser reads as a
    // protocol-relative URL to another host.
    if value.starts_with("//") {
        return None;
    }
    if let Some(query) = resolved.query().filter(|query| !query.is_empty()) {
        value.push('?');
        value.push_str(query);
    }
    Some(value)
}

#[cfg(test)]
mod tests {
    use super::safe;

    #[test]
    fn console_return_path_allows_same_origin_paths_and_queries() {
        for (raw, expected) in [
            ("/", "/"),
            ("/console", "/console"),
            ("/console?", "/console"),
            ("/console?#fragment", "/console"),
            (
                "/console/credentials?kind=oauth",
                "/console/credentials?kind=oauth",
            ),
            (
                "/console/principals/prn_123#grants",
                "/console/principals/prn_123",
            ),
            (
                "/mcp/oauth/authorize?client_id=moc_1&state=x",
                "/mcp/oauth/authorize?client_id=moc_1&state=x",
            ),
            (
                "/oauth/google/start?scopes=a+b",
                "/oauth/google/start?scopes=a+b",
            ),
            ("/oauth/google~team/start", "/oauth/google~team/start"),
            (
                "/oauth/acme.google_v2-x/start",
                "/oauth/acme.google_v2-x/start",
            ),
            (
                "/oauth/google/callback?code=x&state=y",
                "/oauth/google/callback?code=x&state=y",
            ),
            ("/api/v1/me", "/api/v1/me"),
        ] {
            assert_eq!(safe(raw).as_deref(), Some(expected), "{raw:?}");
        }
    }

    #[test]
    fn console_return_path_rejects_off_origin_and_smuggled_targets() {
        for raw in [
            "",
            "console",
            "https://evil.example/console",
            "//evil.example/console",
            "///evil.example",
            "/\\evil.example",
            "/\tevil",
            "/console\n//evil",
            "javascript:alert(1)",
            "http:/console",
            "/oauth/a b/start",
            "/mcp/oauth/authorize?state=a\nLocation:%20x",
            "/mcp/oauth/authorize?state=a b",
            "/\\evil.example?x=\\",
            "/..//evil.example",
            "/.//evil.example",
            "/%2e%2e//evil.example",
            "/%2E%2E//evil.example",
            "/console/../..//evil.example",
            "/console/..//evil.example/login",
        ] {
            assert_eq!(safe(raw), None, "{raw:?}");
        }
        assert_eq!(safe(&format!("/{}", "a".repeat(5000))), None);
    }

    #[test]
    fn console_return_path_resolves_dot_segments_in_place() {
        // Dot segments are resolved by the URL parser and stay same-origin.
        for (raw, expected) in [
            ("/console/../api", "/api"),
            ("/console/./x", "/console/x"),
            ("/console/%2e%2e/api", "/api"),
            ("/console/%2E%2E/api", "/api"),
        ] {
            assert_eq!(safe(raw).as_deref(), Some(expected), "{raw:?}");
        }
    }
}
