use std::{io::Write, process::Stdio, time::Duration};

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use tempfile::NamedTempFile;
use tokio::{process::Command, time::timeout};

use crate::errors::rejected;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Chunk {
    pub chunk_id: String,
    pub ordinal: usize,
    pub body: String,
    pub content_hash: String,
}

pub async fn extract_pdf_text(
    pdf: Vec<u8>,
    extraction_timeout: Duration,
    max_output_bytes: usize,
) -> Result<String> {
    let mut input = NamedTempFile::new().context("create PDF input file")?;
    input.write_all(&pdf).context("write PDF input file")?;
    input.flush().context("flush PDF input file")?;
    let output = NamedTempFile::new().context("create PDF output file")?;

    let mut command = Command::new("pdftotext");
    command
        .arg("-layout")
        .arg("-nopgbrk")
        .arg(input.path())
        .arg(output.path())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let result = timeout(extraction_timeout, command.output())
        .await
        .context("pdftotext timed out")?
        .context("run pdftotext")?;
    if !result.status.success() {
        let stderr = String::from_utf8_lossy(&result.stderr);
        return Err(rejected(format!(
            "pdftotext failed: {}",
            stderr.trim().chars().take(500).collect::<String>()
        )));
    }
    let output_size = tokio::fs::metadata(output.path())
        .await
        .context("inspect extracted PDF text")?
        .len();
    if output_size > max_output_bytes as u64 {
        return Err(rejected(
            "extracted PDF text exceeds the configured byte limit",
        ));
    }
    let text = tokio::fs::read(output.path())
        .await
        .context("read extracted PDF text")?;
    let text = String::from_utf8(text)
        .map_err(|error| rejected(format!("extracted PDF text is not UTF-8: {error}")))?;
    let normalized = normalize_text(&text);
    if normalized.is_empty() {
        return Err(rejected("PDF contains no extractable text"));
    }
    Ok(normalized)
}

pub fn chunk_text(text: &str, max_chars: usize) -> Vec<Chunk> {
    let paragraphs: Vec<&str> = text
        .split("\n\n")
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect();
    let mut bodies = Vec::new();
    let mut current = String::new();
    for paragraph in paragraphs {
        if paragraph.chars().count() > max_chars {
            if !current.is_empty() {
                bodies.push(std::mem::take(&mut current));
            }
            split_long_text(paragraph, max_chars, &mut bodies);
            continue;
        }
        let separator = if current.is_empty() { 0 } else { 2 };
        if current.chars().count() + separator + paragraph.chars().count() > max_chars {
            bodies.push(std::mem::take(&mut current));
        } else if !current.is_empty() {
            current.push_str("\n\n");
        }
        current.push_str(paragraph);
    }
    if !current.trim().is_empty() {
        bodies.push(current);
    }

    bodies
        .into_iter()
        .enumerate()
        .filter_map(|(ordinal, body)| {
            let body = body.trim().to_owned();
            if body.is_empty() {
                return None;
            }
            Some(Chunk {
                chunk_id: format!("{ordinal:06}"),
                ordinal,
                content_hash: hex_sha256(body.as_bytes()),
                body,
            })
        })
        .collect()
}

pub fn hex_sha256(value: &[u8]) -> String {
    format!("{:x}", Sha256::digest(value))
}

fn split_long_text(text: &str, max_chars: usize, output: &mut Vec<String>) {
    let chars: Vec<char> = text.chars().collect();
    for chunk in chars.chunks(max_chars) {
        output.push(chunk.iter().collect());
    }
}

fn normalize_text(text: &str) -> String {
    text.replace("\r\n", "\n")
        .replace('\r', "\n")
        .lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunks_are_stable_and_bounded() {
        let text = "alpha beta gamma\n\ndelta epsilon zeta\n\neta theta iota";
        let chunks = chunk_text(text, 25);
        assert_eq!(
            chunks
                .iter()
                .map(|chunk| chunk.chunk_id.as_str())
                .collect::<Vec<_>>(),
            vec!["000000", "000001", "000002"]
        );
        assert!(chunks.iter().all(|chunk| chunk.body.chars().count() <= 25));
        assert_eq!(chunks, chunk_text(text, 25));
    }

    #[test]
    fn splits_large_unicode_paragraphs_on_character_boundaries() {
        let chunks = chunk_text("éééééééé", 5);
        assert_eq!(
            chunks
                .iter()
                .map(|chunk| chunk.body.as_str())
                .collect::<Vec<_>>(),
            vec!["ééééé", "ééé"]
        );
    }
}
