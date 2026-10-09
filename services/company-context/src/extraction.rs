use std::{ffi::OsString, io::Write, path::Path, process::Stdio, time::Duration};

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

/// Office and e-book formats converted by pandoc: Slack filetype, MIME type,
/// pandoc reader, and the signature the file must start with.
const PANDOC_FORMATS: &[(&str, &str, &str, &[u8])] = &[
    (
        "docx",
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "docx",
        ZIP_SIGNATURE,
    ),
    (
        "pptx",
        "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "pptx",
        ZIP_SIGNATURE,
    ),
    (
        "xlsx",
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "xlsx",
        ZIP_SIGNATURE,
    ),
    (
        "odt",
        "application/vnd.oasis.opendocument.text",
        "odt",
        ZIP_SIGNATURE,
    ),
    ("epub", "application/epub+zip", "epub", ZIP_SIGNATURE),
    ("rtf", "application/rtf", "rtf", b"{\\rtf"),
    ("rtf", "text/rtf", "rtf", b"{\\rtf"),
];
const ZIP_SIGNATURE: &[u8] = b"PK\x03\x04";
/// Caps pandoc's heap, as pandoc recommends for untrusted input.
const PANDOC_MAX_HEAP: &str = "-M512m";

/// Returns the pandoc reader for a file's Slack filetype or MIME type.
pub fn pandoc_reader(filetype: &str, mimetype: &str) -> Option<&'static str> {
    PANDOC_FORMATS
        .iter()
        .find(|(known_filetype, known_mimetype, _, _)| {
            *known_filetype == filetype || *known_mimetype == mimetype
        })
        .map(|(_, _, reader, _)| *reader)
}

pub async fn extract_pdf_text(
    pdf: Vec<u8>,
    extraction_timeout: Duration,
    max_output_bytes: usize,
) -> Result<String> {
    convert(
        "pdftotext",
        "PDF",
        &pdf,
        extraction_timeout,
        max_output_bytes,
        |input, output| {
            vec![
                "-layout".into(),
                "-nopgbrk".into(),
                input.into(),
                output.into(),
            ]
        },
    )
    .await
}

/// Converts a document in one of pandoc's input formats to plain text.
/// Sandboxed pandoc reads no other files and makes no network requests.
pub async fn extract_pandoc_text(
    document: Vec<u8>,
    reader: &str,
    extraction_timeout: Duration,
    max_output_bytes: usize,
) -> Result<String> {
    let signature = PANDOC_FORMATS
        .iter()
        .find(|(_, _, known_reader, _)| *known_reader == reader)
        .map(|(_, _, _, signature)| *signature)
        .with_context(|| format!("unsupported pandoc reader {reader}"))?;
    if !document.starts_with(signature) {
        return Err(rejected(format!("file is not a {reader} document")));
    }
    convert(
        "pandoc",
        &format!("{reader} document"),
        &document,
        extraction_timeout,
        max_output_bytes,
        |input, output| {
            vec![
                "+RTS".into(),
                PANDOC_MAX_HEAP.into(),
                "-RTS".into(),
                "--sandbox".into(),
                "--from".into(),
                reader.into(),
                "--to".into(),
                "plain".into(),
                "--wrap=none".into(),
                "--output".into(),
                output.into(),
                input.into(),
            ]
        },
    )
    .await
}

/// Runs a converter from a temporary input file to a temporary output file.
/// Both files are removed when the conversion finishes or fails.
async fn convert(
    program: &str,
    label: &str,
    document: &[u8],
    extraction_timeout: Duration,
    max_output_bytes: usize,
    args: impl FnOnce(&Path, &Path) -> Vec<OsString>,
) -> Result<String> {
    let mut input = NamedTempFile::new().with_context(|| format!("create {label} input file"))?;
    input
        .write_all(document)
        .with_context(|| format!("write {label} input file"))?;
    input
        .flush()
        .with_context(|| format!("flush {label} input file"))?;
    let output = NamedTempFile::new().with_context(|| format!("create {label} output file"))?;

    let mut command = Command::new(program);
    command
        .args(args(input.path(), output.path()))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let result = timeout(extraction_timeout, command.output())
        .await
        .with_context(|| format!("{program} timed out"))?
        .with_context(|| format!("run {program}"))?;
    if !result.status.success() {
        let stderr = String::from_utf8_lossy(&result.stderr);
        return Err(rejected(format!(
            "{program} failed: {}",
            stderr.trim().chars().take(500).collect::<String>()
        )));
    }
    let output_size = tokio::fs::metadata(output.path())
        .await
        .with_context(|| format!("inspect extracted {label} text"))?
        .len();
    if output_size > max_output_bytes as u64 {
        return Err(rejected(format!(
            "extracted {label} text exceeds the configured byte limit"
        )));
    }
    let text = tokio::fs::read(output.path())
        .await
        .with_context(|| format!("read extracted {label} text"))?;
    let text = String::from_utf8(text)
        .map_err(|error| rejected(format!("extracted {label} text is not UTF-8: {error}")))?;
    let normalized = normalize_text(&text);
    if normalized.is_empty() {
        return Err(rejected(format!("{label} contains no extractable text")));
    }
    Ok(normalized)
}

pub fn extract_google_doc_text(document: Vec<u8>, max_output_bytes: usize) -> Result<String> {
    extract_plain_text(document, max_output_bytes, "exported Google Doc")
}

/// Decodes a plain-text file, such as a Slack snippet.
pub fn extract_plain_text(
    document: Vec<u8>,
    max_output_bytes: usize,
    label: &str,
) -> Result<String> {
    if document.len() > max_output_bytes {
        return Err(rejected(format!(
            "{label} exceeds the configured byte limit"
        )));
    }
    let text = String::from_utf8(document)
        .map_err(|error| rejected(format!("{label} is not UTF-8: {error}")))?;
    if text.contains('\0') {
        return Err(rejected(format!("{label} is not text")));
    }
    // Drive's plain-text export starts with a UTF-8 byte order mark.
    let normalized = normalize_text(text.strip_prefix('\u{feff}').unwrap_or(&text));
    if normalized.is_empty() {
        return Err(rejected(format!("{label} contains no extractable text")));
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

pub(crate) fn split_long_text(text: &str, max_chars: usize, output: &mut Vec<String>) {
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
    use crate::errors::is_rejected;

    #[test]
    fn google_doc_text_is_utf8_normalized_and_nonempty() {
        assert_eq!(
            extract_google_doc_text("\u{feff}heading\r\nbody  \r\n".into(), 100).unwrap(),
            "heading\nbody"
        );
        assert!(extract_google_doc_text(vec![0xff], 100).is_err());
        assert!(extract_google_doc_text("\u{feff}  \n".into(), 100).is_err());
        assert!(extract_google_doc_text(b"too large".to_vec(), 3).is_err());
    }

    #[test]
    fn binary_files_are_not_plain_text() {
        assert!(is_rejected(
            &extract_plain_text(b"a\0b".to_vec(), 100, "file").unwrap_err()
        ));
    }

    #[tokio::test]
    async fn office_documents_are_converted_to_plain_text() {
        let timeout = Duration::from_secs(30);
        // The signature is checked before pandoc runs.
        let error = extract_pandoc_text(b"plain text".to_vec(), "docx", timeout, 1_000)
            .await
            .unwrap_err();
        assert!(is_rejected(&error), "{error}");

        let directory = tempfile::tempdir().unwrap();
        let docx = directory.path().join("report.docx");
        let Ok(status) = std::process::Command::new("pandoc")
            .args(["--from", "markdown", "--output"])
            .arg(&docx)
            .stdin(Stdio::piped())
            .spawn()
            .and_then(|mut child| {
                child
                    .stdin
                    .take()
                    .unwrap()
                    .write_all(b"# Quarterly report\n\nRevenue grew **12%**.\n")?;
                child.wait()
            })
        else {
            eprintln!("skipping: install pandoc to test Office document conversion");
            return;
        };
        assert!(status.success());
        let text = extract_pandoc_text(std::fs::read(&docx).unwrap(), "docx", timeout, 1_000)
            .await
            .unwrap();
        assert_eq!(text, "Quarterly report\n\nRevenue grew 12%.");
        let error = extract_pandoc_text(std::fs::read(&docx).unwrap(), "docx", timeout, 10)
            .await
            .unwrap_err();
        assert!(is_rejected(&error), "{error}");
    }

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
