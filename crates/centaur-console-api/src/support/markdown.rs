//! Markdown to sanitized HTML (skill instructions). Raw HTML in the source
//! is shown as text; the result goes through `ammonia`'s allow-list (no
//! scripts, event handlers, or unsafe URL schemes; links get
//! `rel="noopener noreferrer"`).

use pulldown_cmark::{Event, Options, Parser, html};

pub fn render(markdown: &str) -> String {
    let options =
        Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    let events = Parser::new_ext(markdown, options).map(|event| match event {
        Event::Html(raw) | Event::InlineHtml(raw) => Event::Text(raw),
        other => other,
    });
    let mut unsafe_html = String::new();
    html::push_html(&mut unsafe_html, events);
    ammonia::Builder::default()
        .link_rel(Some("noopener noreferrer"))
        .clean(&unsafe_html)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::render;

    #[test]
    fn renders_common_markdown() {
        let html = render(
            "# Title\n\nSome *emphasis* and `code`.\n\n- one\n- two\n\n| a | b |\n|---|---|\n| 1 | 2 |\n",
        );
        for part in [
            "<h1>Title</h1>",
            "<em>emphasis</em>",
            "<code>code</code>",
            "<li>one</li>",
            "<table>",
        ] {
            assert!(html.contains(part), "{part} in {html}");
        }
    }

    #[test]
    fn neutralizes_scripts_handlers_and_unsafe_links() {
        let html = render(
            "<script>alert(1)</script>\n\n<img src=x onerror=alert(1)>\n\n[click](javascript:alert(1))\n\n[ok](https://example.test)",
        );
        assert!(!html.contains("<script"), "{html}");
        assert!(!html.contains("<img"), "{html}");
        assert!(
            !html.contains("onerror=") || html.contains("&lt;"),
            "{html}"
        );
        assert!(!html.contains("javascript:"), "{html}");
        assert!(html.contains("href=\"https://example.test\""));
        assert!(html.contains("rel=\"noopener noreferrer\""));
    }
}
