use std::{io::Write, process::Stdio, time::Duration};

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};
use tempfile::NamedTempFile;
use tokio::{process::Command, time::timeout};

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
        bail!(
            "pdftotext failed: {}",
            stderr.trim().chars().take(500).collect::<String>()
        );
    }
    let output_size = tokio::fs::metadata(output.path())
        .await
        .context("inspect extracted PDF text")?
        .len();
    if output_size > max_output_bytes as u64 {
        bail!("extracted PDF text exceeds the configured byte limit");
    }
    let text = tokio::fs::read_to_string(output.path())
        .await
        .context("read extracted PDF text")?;
    let normalized = normalize_text(&text);
    if normalized.is_empty() {
        bail!("PDF contains no extractable text");
    }
    Ok(normalized)
}

pub fn chunk_text(text: &str, max_chars: usize, overlap_chars: usize) -> Vec<Chunk> {
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
            split_long_text(paragraph, max_chars, overlap_chars, &mut bodies);
            continue;
        }
        let separator = if current.is_empty() { 0 } else { 2 };
        if current.chars().count() + separator + paragraph.chars().count() > max_chars {
            let previous = std::mem::take(&mut current);
            let available_overlap = max_chars.saturating_sub(paragraph.chars().count() + 2);
            let overlap = char_suffix(&previous, overlap_chars.min(available_overlap));
            bodies.push(previous);
            current = overlap;
            if !current.is_empty() {
                current.push_str("\n\n");
            }
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

fn split_long_text(text: &str, max_chars: usize, overlap_chars: usize, output: &mut Vec<String>) {
    let chars: Vec<char> = text.chars().collect();
    let step = max_chars - overlap_chars;
    let mut start = 0;
    while start < chars.len() {
        let end = (start + max_chars).min(chars.len());
        output.push(chars[start..end].iter().collect());
        if end == chars.len() {
            break;
        }
        start += step;
    }
}

fn char_suffix(value: &str, count: usize) -> String {
    let chars: Vec<char> = value.chars().collect();
    chars[chars.len().saturating_sub(count)..].iter().collect()
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
        let chunks = chunk_text(text, 25, 5);
        assert_eq!(
            chunks
                .iter()
                .map(|chunk| chunk.chunk_id.as_str())
                .collect::<Vec<_>>(),
            vec!["000000", "000001", "000002"]
        );
        assert!(chunks.iter().all(|chunk| chunk.body.chars().count() <= 25));
        assert_eq!(chunks, chunk_text(text, 25, 5));
    }

    #[test]
    fn splits_large_unicode_paragraphs_on_character_boundaries() {
        let chunks = chunk_text("éééééééé", 5, 2);
        assert_eq!(
            chunks
                .iter()
                .map(|chunk| chunk.body.as_str())
                .collect::<Vec<_>>(),
            vec!["ééééé", "ééééé"]
        );
    }
}
