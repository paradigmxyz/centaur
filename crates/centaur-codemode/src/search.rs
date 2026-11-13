//! BM25 over connector functions: connector name, function name, doc and
//! parameter names.

use std::collections::HashMap;

use serde::Serialize;

use crate::catalog::Catalog;

const K1: f64 = 1.2;
const B: f64 = 0.75;

#[derive(Debug, Serialize)]
pub struct SearchResult {
    pub tool: String,
    pub signature: String,
    pub doc: String,
    pub score: f64,
}

pub fn search(
    catalog: &Catalog,
    allowed: Option<&[String]>,
    query: &str,
    limit: usize,
) -> Vec<SearchResult> {
    let docs: Vec<(String, &str, &str, Vec<String>)> = catalog
        .visible(allowed)
        .flat_map(|connector| {
            connector.functions.iter().map(move |function| {
                let text = format!(
                    "{} {} {} {} {}",
                    connector.name,
                    function.name,
                    function.doc,
                    function.signature,
                    connector.description
                );
                (
                    format!("{}.{}", connector.name, function.name),
                    function.signature.as_str(),
                    function.doc.as_str(),
                    tokens(&text),
                )
            })
        })
        .collect();
    if docs.is_empty() {
        return Vec::new();
    }
    let n = docs.len() as f64;
    let avg_len = docs.iter().map(|doc| doc.3.len()).sum::<usize>() as f64 / n;
    let mut df: HashMap<&str, usize> = HashMap::new();
    for doc in &docs {
        let mut seen: Vec<&str> = doc.3.iter().map(String::as_str).collect();
        seen.sort_unstable();
        seen.dedup();
        for term in seen {
            *df.entry(term).or_default() += 1;
        }
    }
    let query_terms = tokens(query);
    let mut results: Vec<SearchResult> = docs
        .iter()
        .map(|(tool, signature, doc, terms)| {
            let len = terms.len() as f64;
            let score = query_terms
                .iter()
                .map(|term| {
                    let tf = terms.iter().filter(|t| *t == term).count() as f64;
                    if tf == 0.0 {
                        return 0.0;
                    }
                    let df = *df.get(term.as_str()).unwrap_or(&0) as f64;
                    let idf = (1.0 + (n - df + 0.5) / (df + 0.5)).ln();
                    idf * tf * (K1 + 1.0) / (tf + K1 * (1.0 - B + B * len / avg_len))
                })
                .sum();
            SearchResult {
                tool: tool.clone(),
                signature: (*signature).to_owned(),
                doc: (*doc).to_owned(),
                score,
            }
        })
        .filter(|result| result.score > 0.0)
        .collect();
    results.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then_with(|| a.tool.cmp(&b.tool))
    });
    results.truncate(limit);
    results
}

fn tokens(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|token| token.len() > 1)
        .map(str::to_ascii_lowercase)
        .collect()
}
