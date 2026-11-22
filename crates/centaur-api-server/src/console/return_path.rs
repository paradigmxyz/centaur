//! Console's allowlist for same-origin paths a browser may be sent to after
//! signing in, and from the PWA launch handler.
//!
//! The Rails implementation lives in `services/console/lib/console_return_path.rb`;
//! this is a direct port. Keep the checks in the same order, including the
//! fragment strip, the backslash/control-character refusals, dot-segment
//! rejection, and the exact path patterns.

use std::sync::OnceLock;

use regex::Regex;

const MAX_LENGTH: usize = 4096;
const OAUTH_SLUG: &str = r"[A-Za-z0-9\-._~]+";

fn allowed_patterns() -> &'static [Regex] {
    static PATTERNS: OnceLock<Vec<Regex>> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        [
            r"\A/\z".to_owned(),
            r"\A/console(?:/[A-Za-z0-9_\-./~%:@]*)?\z".to_owned(),
            r"\A/mcp/oauth/authorize(?:\.html)?\z".to_owned(),
            format!(r"\A/oauth/{OAUTH_SLUG}/(?:start|callback)(?:\.html)?\z"),
            r"\A/launch\z".to_owned(),
        ]
        .into_iter()
        .map(|pattern| Regex::new(&pattern).expect("valid Console return-path pattern"))
        .collect()
    })
}

/// The normalized path (with its query) when `raw` is an allowed same-origin
/// return path, else `None`.
pub(crate) fn safe(raw: &str) -> Option<String> {
    if raw.is_empty() || raw.chars().count() > MAX_LENGTH {
        return None;
    }
    // Fragments never reach the server and are dropped from the return path.
    let value = raw.split_once('#').map_or(raw, |(value, _)| value);
    let (path, query) = match value.split_once('?') {
        // Ruby's `String#split("?", 2)` drops a trailing empty field, so
        // `/console?` normalizes to `/console`.
        Some((path, "")) => (path, None),
        Some((path, query)) => (path, Some(query)),
        None => (value, None),
    };
    // Browsers treat "\" like "/" and strip tabs/newlines in the path, so
    // "/\evil.example" or "/\t/evil.example" can turn into a protocol-relative
    // URL. The query is opaque to that resolution (OAuth clients put arbitrary
    // state there, sometimes unencoded), so only control characters and spaces,
    // which could split a Location header, are refused in it.
    if path
        .chars()
        .any(|character| character == '\\' || character.is_control() || character.is_whitespace())
    {
        return None;
    }
    if query.is_some_and(|query| {
        query
            .chars()
            .any(|character| character.is_control() || character.is_whitespace())
    }) {
        return None;
    }
    if !path.starts_with('/') || path.starts_with("//") {
        return None;
    }
    // `URI.parse` only validates the path; the query is kept verbatim.
    if !valid_percent_escapes(path) {
        return None;
    }
    if dot_segment(path) {
        return None;
    }
    if !allowed_patterns()
        .iter()
        .any(|pattern| pattern.is_match(path))
    {
        return None;
    }
    Some(match query {
        Some(query) => format!("{path}?{query}"),
        None => path.to_owned(),
    })
}

/// "." / ".." segments (literal or percent-encoded) are resolved by the
/// browser, so "/console/../api" would escape the allowlisted prefix.
fn dot_segment(path: &str) -> bool {
    path.to_ascii_lowercase().contains("%2e")
        || path
            .split('/')
            .any(|segment| segment == "." || segment == "..")
}

/// `URI.parse` rejects malformed percent escapes; the allowlist patterns let
/// `%` through for valid escapes, so check them explicitly.
fn valid_percent_escapes(value: &str) -> bool {
    let bytes = value.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len()
                || !bytes[index + 1].is_ascii_hexdigit()
                || !bytes[index + 2].is_ascii_hexdigit()
            {
                return false;
            }
            index += 3;
        } else {
            index += 1;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::safe;

    #[test]
    fn console_return_path_allows_the_root_console_and_session_gated_entry_points() {
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
                "/mcp/oauth/authorize.html?client_id=moc_1&redirect_uri=http%3A%2F%2F127.0.0.1%3A1%2Fcb&scope=a+b",
                "/mcp/oauth/authorize.html?client_id=moc_1&redirect_uri=http%3A%2F%2F127.0.0.1%3A1%2Fcb&scope=a+b",
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
                "/oauth/google/start.html?x=1",
                "/oauth/google/start.html?x=1",
            ),
            (
                "/oauth/google/callback?code=x&state=y",
                "/oauth/google/callback?code=x&state=y",
            ),
            (
                "/oauth/google/callback.html?code=x",
                "/oauth/google/callback.html?code=x",
            ),
            (
                "/launch?target=web%2Bcentaur%3A%2F%2Fconsole",
                "/launch?target=web%2Bcentaur%3A%2F%2Fconsole",
            ),
            // Opaque OAuth state may carry an unencoded backslash.
            (
                "/mcp/oauth/authorize?client_id=moc_1&state=opaque\\client-state",
                "/mcp/oauth/authorize?client_id=moc_1&state=opaque\\client-state",
            ),
            // The query is opaque, so an invalid escape there is not parsed.
            (
                "/mcp/oauth/authorize?state=%zz",
                "/mcp/oauth/authorize?state=%zz",
            ),
        ] {
            assert_eq!(safe(raw).as_deref(), Some(expected), "{raw:?}");
        }
    }

    #[test]
    fn console_return_path_rejects_off_origin_protocol_relative_and_smuggled_targets() {
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
            "/console/../api/v1/me",
            "/console/./x/..",
            "/console/%2e%2e/api",
            "/console/%2E%2E/api",
            "/consolex",
            "/api/v1/me",
            "/logout",
            "/login",
            "/pending",
            "/auth/google/callback",
            "/mcp/oauth/token",
            "/mcp/oauth/authorize.json",
            "/mcp/oauth/authorizex",
            "/mcp/oauth/authorize/x",
            "/oauth/../console",
            "/oauth/../start",
            "/oauth/./start",
            "/oauth/%2e%2e/start",
            "/oauth/a%2Fb/start",
            "/oauth/a b/start",
            "/oauth/a@b/start",
            "/mcp/oauth/authorize?state=a\nLocation:%20x",
            "/mcp/oauth/authorize?state=a b",
            "/\\evil.example?x=\\",
            "/console/%zz",
        ] {
            assert_eq!(safe(raw), None, "{raw:?}");
        }
        assert_eq!(safe(&format!("/{}", "a".repeat(5000))), None);
    }

    #[test]
    fn console_return_path_accepts_every_oauth_slug() {
        for slug in ["google", "google~team", "a.b_c-d", "~", "x~"] {
            assert_eq!(
                safe(&format!("/oauth/{slug}/start")).as_deref(),
                Some(format!("/oauth/{slug}/start").as_str()),
                "{slug}"
            );
        }
    }
}
