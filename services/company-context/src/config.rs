use std::{env, net::SocketAddr, time::Duration};

use anyhow::{Context, Result, bail};

pub const PDF_MIME_TYPE: &str = "application/pdf";
pub const QUEUE_NAME: &str = "company_context";
pub const DRIVE_SCAN_TASK: &str = "drive.scan";
pub const PDF_EXTRACT_TASK: &str = "drive.pdf.extract";
pub const DOCUMENT_EMBED_TASK: &str = "drive.document.embed";
pub const DOCUMENT_DELETE_TASK: &str = "drive.document.delete";

#[derive(Clone, Debug)]
pub struct Config {
    pub database_url: String,
    pub bind_addr: SocketAddr,
    pub google_api_base_url: String,
    pub google_access_token: Option<String>,
    pub openai_base_url: String,
    pub openai_api_key: Option<String>,
    pub embeddings_model: String,
    pub embeddings_dimensions: usize,
    pub scan_interval: Duration,
    pub scan_page_size: u16,
    pub max_scan_pages: usize,
    pub max_pdf_bytes: usize,
    pub max_extracted_bytes: usize,
    pub extraction_timeout: Duration,
    pub chunk_chars: usize,
    pub chunk_overlap_chars: usize,
    pub worker_concurrency: usize,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let database_url = required_env("DATABASE_URL")?;
        let bind_addr = env::var("BIND_ADDR")
            .unwrap_or_else(|_| "0.0.0.0:8080".to_owned())
            .parse()
            .context("BIND_ADDR must be a socket address")?;
        let scan_interval = Duration::from_secs(positive_usize(
            "COMPANY_CONTEXT_SCAN_INTERVAL_SECONDS",
            300,
        )? as u64);
        let scan_page_size = positive_usize("COMPANY_CONTEXT_DRIVE_PAGE_SIZE", 100)?;
        if scan_page_size > 1_000 {
            bail!("COMPANY_CONTEXT_DRIVE_PAGE_SIZE must not exceed 1000");
        }
        let embeddings_dimensions = positive_usize("COMPANY_CONTEXT_EMBEDDINGS_DIMENSIONS", 1536)?;
        if embeddings_dimensions != 1536 {
            bail!("COMPANY_CONTEXT_EMBEDDINGS_DIMENSIONS must be 1536 for the current schema");
        }
        let chunk_chars = positive_usize("COMPANY_CONTEXT_CHUNK_CHARS", 6_000)?;
        let chunk_overlap_chars = nonnegative_usize("COMPANY_CONTEXT_CHUNK_OVERLAP_CHARS", 500)?;
        if chunk_overlap_chars >= chunk_chars {
            bail!(
                "COMPANY_CONTEXT_CHUNK_OVERLAP_CHARS must be smaller than COMPANY_CONTEXT_CHUNK_CHARS"
            );
        }

        Ok(Self {
            database_url,
            bind_addr,
            google_api_base_url: normalized_base_url(
                "GOOGLE_DRIVE_API_BASE_URL",
                "https://www.googleapis.com/drive/v3",
            ),
            google_access_token: optional_env("GOOGLE_DRIVE_ACCESS_TOKEN"),
            openai_base_url: normalized_base_url("OPENAI_BASE_URL", "https://api.openai.com/v1"),
            openai_api_key: optional_env("OPENAI_API_KEY"),
            embeddings_model: env::var("COMPANY_CONTEXT_EMBEDDINGS_MODEL")
                .unwrap_or_else(|_| "text-embedding-3-small".to_owned()),
            embeddings_dimensions,
            scan_interval,
            scan_page_size: scan_page_size as u16,
            max_scan_pages: positive_usize("COMPANY_CONTEXT_MAX_SCAN_PAGES", 10)?,
            max_pdf_bytes: positive_usize("COMPANY_CONTEXT_MAX_PDF_BYTES", 25 * 1024 * 1024)?,
            max_extracted_bytes: positive_usize(
                "COMPANY_CONTEXT_MAX_EXTRACTED_BYTES",
                50 * 1024 * 1024,
            )?,
            extraction_timeout: Duration::from_secs(positive_usize(
                "COMPANY_CONTEXT_EXTRACTION_TIMEOUT_SECONDS",
                120,
            )? as u64),
            chunk_chars,
            chunk_overlap_chars,
            worker_concurrency: positive_usize("COMPANY_CONTEXT_WORKER_CONCURRENCY", 4)?,
        })
    }
}

fn required_env(name: &str) -> Result<String> {
    optional_env(name).with_context(|| format!("{name} is required"))
}

fn optional_env(name: &str) -> Option<String> {
    env::var(name)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn normalized_base_url(name: &str, default: &str) -> String {
    env::var(name)
        .unwrap_or_else(|_| default.to_owned())
        .trim()
        .trim_end_matches('/')
        .to_owned()
}

fn positive_usize(name: &str, default: usize) -> Result<usize> {
    let value = env::var(name).ok();
    let parsed = match value.as_deref() {
        Some(value) => value
            .parse()
            .with_context(|| format!("{name} must be an integer"))?,
        None => default,
    };
    if parsed == 0 {
        bail!("{name} must be greater than zero");
    }
    Ok(parsed)
}

fn nonnegative_usize(name: &str, default: usize) -> Result<usize> {
    match env::var(name) {
        Ok(value) => value
            .parse()
            .with_context(|| format!("{name} must be a nonnegative integer")),
        Err(_) => Ok(default),
    }
}
