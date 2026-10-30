use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;
use url::Url;

#[derive(Default)]
pub(super) struct CitationSources {
    by_ref: BTreeMap<String, String>,
    numbers: BTreeMap<String, usize>,
}

impl CitationSources {
    pub(super) fn record_item(&mut self, item: &Value) {
        if item["type"] == "webSearch"
            && let Some(results) = item["results"].as_array()
        {
            for result in results {
                if let Some(reference) = result["ref_id"].as_str()
                    && !reference.is_empty()
                    && reference
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
                    && let Some(url) = result["url"].as_str().and_then(source_url)
                {
                    self.by_ref.entry(reference.to_owned()).or_insert(url);
                }
            }
        }
    }

    pub(super) fn record_turn(&mut self, turn: &Value) {
        if let Some(items) = turn["items"].as_array() {
            for item in items {
                self.record_item(item);
            }
        }
    }

    pub(super) fn record_thread(&mut self, thread: &Value) {
        if let Some(turns) = thread["turns"].as_array() {
            for turn in turns {
                self.record_turn(turn);
            }
        }
    }

    pub(super) fn render(&mut self, content: &str) -> String {
        let mut links = String::new();
        let mut seen = BTreeSet::new();
        for reference in content.split('').flat_map(|part| part.split(":ship:")) {
            if let Some(url) = self.by_ref.get(reference.trim())
                && seen.insert(url)
            {
                let next = self.numbers.len() + 1;
                let number = self.numbers.entry(url.clone()).or_insert(next);
                links.push_str(&format!(" [{number}]({url})"));
            }
        }
        links
    }
}

fn source_url(value: &str) -> Option<String> {
    if value.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return None;
    }
    let url = Url::parse(value).ok()?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return None;
    }
    Some(url.as_str().replace('(', "%28").replace(')', "%29"))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn only_explicit_safe_reference_urls_are_registered() {
        let mut sources = CitationSources::default();
        for (reference, url) in [
            ("script", "javascript:alert(1)"),
            ("data", "data:text/html,test"),
            ("file", "file:///tmp/test"),
            ("auth", "https://user:secret@example.com/"),
            ("username", "https://user@example.com/"),
            ("newline", "https://example.com/\ninjected"),
            ("space", "https://example.com/a b"),
            ("bad id", "https://example.com/"),
        ] {
            sources.record_item(
                &json!({"type": "webSearch", "results": [{"ref_id": reference, "url": url}]}),
            );
        }
        sources.record_item(&json!({"type": "webSearch", "results": [
            {"ref_id": "missing_url", "title": "https://example.com/"},
            {"url": "https://example.com/"},
            {"ref_id": "turn0search0", "url": "https://example.com/a(b)?q=c(d)", "title": "[untrusted](javascript:test)"},
        ]}));
        assert_eq!(sources.by_ref.len(), 1);
        assert_eq!(
            sources.render("turn0search0"),
            " [1](https://example.com/a%28b%29?q=c%28d%29)"
        );
        assert_eq!(sources.render("scriptmissing_urlunknown"), "");
    }

    #[test]
    fn references_are_numbered_on_use_and_duplicate_urls_share_one_link() {
        let mut sources = CitationSources::default();
        sources.record_item(&json!({"type": "webSearch", "results": [
            {"ref_id": "turn0search0", "url": "https://example.com/a"},
            {"ref_id": "turn0search1", "url": "https://example.com/b"},
            {"ref_id": "turn1view0", "url": "https://example.com/a"},
        ]}));
        assert_eq!(
            sources.render("turn0search1"),
            " [1](https://example.com/b)"
        );
        assert_eq!(
            sources.render("turn0search0turn1view0L1-L3"),
            " [2](https://example.com/a)"
        );
        assert_eq!(
            sources.render("turn0search0:ship:turn0search1"),
            " [2](https://example.com/a) [1](https://example.com/b)"
        );
    }

    #[test]
    fn a_reference_cannot_be_rebound_or_inferred_from_navigation() {
        let mut sources = CitationSources::default();
        for url in [
            "https://example.com/original",
            "https://example.com/replacement",
        ] {
            sources.record_item(
                &json!({"type": "webSearch", "results": [{"ref_id": "ref", "url": url}]}),
            );
        }
        sources.record_item(&json!({"type": "webSearch", "id": "turn0search0", "action": {"type": "openPage", "url": "https://example.com/"}}));
        sources.record_item(&json!({"type": "userMessage", "results": [{"ref_id": "user_ref", "url": "https://example.com/"}]}));
        assert_eq!(sources.render("ref"), " [1](https://example.com/original)");
        assert_eq!(sources.render("turn0search0user_ref"), "");
    }
}
