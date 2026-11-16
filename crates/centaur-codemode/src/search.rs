//! BM25 ranking over the connector catalog.
//!
//! Ported from nanocode's MCP tool search
//! (`crates/integrations/mcp-tools/src/search.rs`, see its `SEARCH.md`): the
//! same tokenizer (Unicode alphanumerics, camelCase and snake_case splitting),
//! the same `k1 = 1.2` / `b = 0.75` parameters, the same document-length
//! normalization and smoothed IDF `ln(1 + (N - df + 0.5) / (df + 0.5))`, and
//! the same deterministic tie-break by document index.
//!
//! One document is one connector function. The index is immutable: a catalog
//! reload builds a new one.

use std::collections::HashMap;

use serde::Serialize;

/// bm25 2.3.2's default term-frequency saturation and length normalization.
const K1: f32 = 1.2;
const B: f32 = 0.75;
/// Search result count when a request does not set `limit`, and its cap.
pub const DEFAULT_LIMIT: usize = 8;
pub const MAX_LIMIT: usize = 50;

/// Clamp a requested limit: default 8, at least 1, at most 50.
pub fn limit(requested: Option<usize>) -> usize {
    requested.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT)
}

/// One connector function, indexed once per catalog load.
#[derive(Clone, Debug)]
pub struct Document {
    /// Connector name (its directory).
    pub connector: String,
    /// Connector description, from `connector.toml`.
    pub description: String,
    pub function: String,
    /// `name(args: {...}): Promise<...>`, as cells call it.
    pub signature: String,
    /// The function's JSDoc.
    pub doc: String,
}

impl Document {
    /// The text indexed for this function: the callable name, both names and
    /// the connector description, the JSDoc and the signature.
    fn text(&self) -> String {
        let Self {
            connector,
            description,
            function,
            signature,
            doc,
            ..
        } = self;
        format!("{connector}.{function} {connector} {description} {function} {doc} {signature}")
    }

    /// The first non-empty JSDoc line, which is what a search result shows.
    fn summary(&self) -> &str {
        self.doc
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .unwrap_or_default()
    }

    pub fn hit(&self) -> Hit {
        Hit {
            tool: self.connector.clone(),
            function: self.function.clone(),
            signature: self.signature.clone(),
            description: self.summary().to_owned(),
        }
    }
}

/// One search result, as the HTTP API returns it.
#[derive(Clone, Debug, Serialize)]
pub struct Hit {
    pub tool: String,
    pub function: String,
    pub signature: String,
    pub description: String,
}

/// The immutable index built with a catalog.
#[derive(Clone, Debug, Default)]
pub struct Index {
    documents: Vec<Document>,
    terms: HashMap<String, Term>,
}

#[derive(Clone, Debug)]
struct Term {
    idf: f32,
    /// `(document index, term weight)`.
    postings: Vec<(usize, f32)>,
}

impl Index {
    pub fn new(documents: Vec<Document>) -> Self {
        let tokens = documents
            .iter()
            .map(|document| tokenize(&document.text()))
            .collect::<Vec<_>>();
        let mut terms = HashMap::<String, Term>::new();
        if !tokens.is_empty() {
            let total = tokens.iter().map(Vec::len).sum::<usize>();
            let average_length = total as f32 / tokens.len() as f32;
            for (index, tokens) in tokens.iter().enumerate() {
                let mut frequencies = HashMap::<&str, usize>::new();
                for token in tokens {
                    *frequencies.entry(token).or_default() += 1;
                }
                for (token, frequency) in frequencies {
                    let frequency = frequency as f32;
                    let normalization = 1.0 - B + B * (tokens.len() as f32 / average_length);
                    let weight = frequency * (K1 + 1.0) / (frequency + K1 * normalization);
                    terms
                        .entry(token.to_owned())
                        .or_insert_with(|| Term {
                            idf: 0.0,
                            postings: Vec::new(),
                        })
                        .postings
                        .push((index, weight));
                }
            }
            for term in terms.values_mut() {
                let frequency = term.postings.len() as f32;
                term.idf =
                    (1.0 + (documents.len() as f32 - frequency + 0.5) / (frequency + 0.5)).ln();
            }
        }
        Self { documents, terms }
    }

    /// Every document the query matches, best score first. The caller applies
    /// visibility and its own limit, so a filtered result can still fill up
    /// from the remaining documents.
    pub fn ranked(&self, query: &str) -> Vec<(usize, f32)> {
        let mut scores = HashMap::<usize, f32>::new();
        for token in tokenize(query) {
            if let Some(term) = self.terms.get(&token) {
                for &(index, weight) in &term.postings {
                    *scores.entry(index).or_default() += term.idf * weight;
                }
            }
        }
        let mut results = scores.into_iter().collect::<Vec<_>>();
        results.sort_unstable_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        results
    }

    pub fn documents(&self) -> &[Document] {
        &self.documents
    }
}

/// Split text into search terms: on non-alphanumerics, and at camelCase word
/// starts (`searchOrganizations`, `HTTPServer`).
pub fn tokenize(input_text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut token = String::new();
    let mut previous = None;
    let mut characters = input_text.chars().peekable();

    while let Some(character) = characters.next() {
        if !character.is_alphanumeric() {
            push_search_token(&mut tokens, &mut token);
            previous = None;
            continue;
        }

        let starts_word = !token.is_empty()
            && character.is_uppercase()
            && (previous.is_some_and(char::is_lowercase)
                || characters.peek().is_some_and(|next| next.is_lowercase()));
        if starts_word {
            push_search_token(&mut tokens, &mut token);
        }
        token.extend(character.to_lowercase());
        previous = Some(character);
    }
    push_search_token(&mut tokens, &mut token);
    tokens
}

fn push_search_token(tokens: &mut Vec<String>, token: &mut String) {
    if !token.is_empty() {
        tokens.push(std::mem::take(token));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document(connector: &str, function: &str, doc: &str, signature: &str) -> Document {
        Document {
            connector: connector.to_owned(),
            description: format!("{connector} connector"),
            function: function.to_owned(),
            signature: signature.to_owned(),
            doc: doc.to_owned(),
        }
    }

    fn index() -> Index {
        Index::new(vec![
            document(
                "crunchbase",
                "search_organizations",
                "Search organizations by name.",
                "search_organizations(args: { query: string }): Promise<{ entities: unknown[] }>",
            ),
            document(
                "slack",
                "post_message",
                "Post a message to a channel.",
                "post_message(args: { channel: string; text: string }): Promise<{ ts: string }>",
            ),
            document(
                "slack",
                "channels",
                "List channels the bot can read.",
                "channels(args: { readable_only?: boolean }): Promise<{ channels: unknown[] }>",
            ),
        ])
    }

    #[test]
    fn tokens_and_limits() {
        assert_eq!(limit(None), DEFAULT_LIMIT);
        assert_eq!(limit(Some(0)), 1);
        assert_eq!(limit(Some(1000)), MAX_LIMIT);
        assert_eq!(
            tokenize("searchOrganizations"),
            vec!["search", "organizations"]
        );
        assert_eq!(
            tokenize("crunchbase.search_organizations"),
            vec!["crunchbase", "search", "organizations"]
        );
        assert_eq!(tokenize("HTTPServer v2"), vec!["http", "server", "v2"]);
        assert_eq!(tokenize("  "), Vec::<String>::new());
    }

    #[test]
    fn ranking_matches_bm25_and_ties_keep_document_order() {
        let index = index();
        let ranked = index.ranked("organizations");
        assert_eq!(ranked[0].0, 0, "{ranked:?}");
        let ranked = index.ranked("list channels readableOnly");
        assert_eq!(ranked[0].0, 2, "{ranked:?}");
        let ranked = index.ranked("POST_MESSAGE");
        assert_eq!(ranked[0].0, 1, "{ranked:?}");
        assert!(index.ranked("zzzz").is_empty());
        // Two equally long documents score the same, so document order decides.
        let index = Index::new(vec![
            document("demo", "aa", "Do it.", "aa(args: {}): number"),
            document("demo", "bb", "Do it.", "bb(args: {}): number"),
        ]);
        let ranked = index.ranked("demo do");
        assert_eq!(
            ranked.iter().map(|(index, _)| *index).collect::<Vec<_>>(),
            vec![0, 1]
        );
    }
}
