//! Small shared helpers for building stable iron-control identifiers.

/// Lowercase ``value``, collapse runs of non-alphanumerics to single dashes,
/// and trim leading/trailing dashes. Dashes are inserted lazily before the
/// next alphanumeric, so the result never starts or ends with one. An input
/// with no alphanumerics yields ``"x"`` so the slug is always non-empty.
pub(crate) fn slugify(value: &str) -> String {
    let mut out = String::new();
    let mut pending_dash = false;
    for ch in value.chars().flat_map(char::to_lowercase) {
        if ch.is_ascii_alphanumeric() {
            if pending_dash && !out.is_empty() {
                out.push('-');
            }
            out.push(ch);
            pending_dash = false;
        } else {
            pending_dash = true;
        }
    }
    if out.is_empty() { "x".to_owned() } else { out }
}

#[cfg(test)]
mod tests {
    use super::slugify;

    #[test]
    fn collapses_and_trims() {
        assert_eq!(slugify("XAI_API_KEY"), "xai-api-key");
        assert_eq!(slugify("api.github.com"), "api-github-com");
        assert_eq!(slugify("--Edge  Proxy--"), "edge-proxy");
        assert_eq!(slugify("***"), "x");
    }
}
