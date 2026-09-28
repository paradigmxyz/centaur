use std::{sync::Arc, time::Duration};

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use futures_util::StreamExt;
use reqwest::{Client, RequestBuilder, StatusCode, header::RETRY_AFTER};
use serde::{Deserialize, Serialize};
use tokio::time::sleep;
use tracing::warn;

use crate::{
    config::{Config, PDF_MIME_TYPE},
    credentials::ConsoleCredentials,
    errors::rejected,
};

const DRIVE_REQUEST_ATTEMPTS: u32 = 4;
const DRIVE_SERVER_RETRY_BASE: Duration = Duration::from_secs(1);

#[derive(Clone)]
pub struct DriveClient {
    http: Client,
    base_url: String,
    credentials: Arc<ConsoleCredentials>,
    max_pdf_bytes: usize,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DriveFile {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub mime_type: String,
    #[serde(default)]
    pub web_view_link: String,
    #[serde(default)]
    pub drive_id: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub md5_checksum: String,
    #[serde(default)]
    pub trashed: bool,
    pub created_time: Option<DateTime<Utc>>,
    pub modified_time: Option<DateTime<Utc>>,
    #[serde(default)]
    pub owners: Vec<Identity>,
    #[serde(default)]
    pub permissions: Vec<Permission>,
}

impl DriveFile {
    pub fn is_active_user_pdf(&self) -> bool {
        !self.trashed
            && self.mime_type == PDF_MIME_TYPE
            && !self.id.is_empty()
            && self.drive_id.is_empty()
    }

    pub fn source_version(&self) -> String {
        if !self.version.is_empty() {
            self.version.clone()
        } else if !self.md5_checksum.is_empty() {
            self.md5_checksum.clone()
        } else {
            self.modified_time
                .map(|value| value.to_rfc3339())
                .unwrap_or_default()
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Identity {
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub email_address: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Permission {
    #[serde(default)]
    pub id: String,
    #[serde(rename = "type", default)]
    pub permission_type: String,
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub email_address: String,
    #[serde(default)]
    pub domain: String,
    pub allow_file_discovery: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FilePage {
    #[serde(default)]
    pub files: Vec<DriveFile>,
    pub next_page_token: Option<String>,
    #[serde(default)]
    pub incomplete_search: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangePage {
    #[serde(default)]
    pub changes: Vec<DriveChange>,
    pub next_page_token: Option<String>,
    pub new_start_page_token: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DriveChange {
    pub file_id: String,
    #[serde(default)]
    pub removed: bool,
    pub file: Option<DriveFile>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StartPageToken {
    start_page_token: String,
}

impl DriveClient {
    pub fn new(config: &Config, credentials: Arc<ConsoleCredentials>) -> Result<Self> {
        Ok(Self {
            http: Client::builder()
                .timeout(config.extraction_timeout)
                .build()?,
            base_url: config.google_api_base_url.clone(),
            credentials,
            max_pdf_bytes: config.max_pdf_bytes,
        })
    }

    pub fn credentials(&self) -> &ConsoleCredentials {
        &self.credentials
    }

    pub async fn start_page_token(&self, credential_id: i64) -> Result<String> {
        let response = self
            .send(
                self.http
                    .get(format!("{}/changes/startPageToken", self.base_url)),
                credential_id,
            )
            .await?
            .json::<StartPageToken>()
            .await
            .context("decode Drive start page token")?;
        if response.start_page_token.is_empty() {
            bail!("Drive returned an empty start page token");
        }
        Ok(response.start_page_token)
    }

    pub async fn list_user_pdfs(
        &self,
        credential_id: i64,
        page_size: u16,
        page_token: Option<&str>,
    ) -> Result<FilePage> {
        let fields = "nextPageToken,incompleteSearch,files(id,name,mimeType,webViewLink,driveId,version,md5Checksum,trashed,createdTime,modifiedTime,owners(displayName,emailAddress),permissions(id,type,role,emailAddress,domain,allowFileDiscovery))";
        let mut request = self.http.get(format!("{}/files", self.base_url)).query(&[
            (
                "q",
                "mimeType = 'application/pdf' and trashed = false".to_owned(),
            ),
            ("pageSize", page_size.to_string()),
            ("fields", fields.to_owned()),
            ("corpora", "user".to_owned()),
            ("includeItemsFromAllDrives", "false".to_owned()),
            ("supportsAllDrives", "true".to_owned()),
            ("orderBy", "modifiedTime".to_owned()),
        ]);
        if let Some(page_token) = page_token {
            request = request.query(&[("pageToken", page_token)]);
        }
        let page = self
            .send(request, credential_id)
            .await?
            .json::<FilePage>()
            .await
            .context("decode Drive file page")?;
        if page.incomplete_search {
            bail!("Google Drive reported an incomplete user corpus search");
        }
        Ok(page)
    }

    pub async fn list_user_changes(
        &self,
        credential_id: i64,
        page_size: u16,
        page_token: &str,
    ) -> Result<ChangePage> {
        let fields = "nextPageToken,newStartPageToken,changes(fileId,removed,file(id,name,mimeType,webViewLink,driveId,version,md5Checksum,trashed,createdTime,modifiedTime,owners(displayName,emailAddress),permissions(id,type,role,emailAddress,domain,allowFileDiscovery)))";
        let request = self.http.get(format!("{}/changes", self.base_url)).query(&[
            ("pageToken", page_token.to_owned()),
            ("pageSize", page_size.to_string()),
            ("fields", fields.to_owned()),
            ("includeItemsFromAllDrives", "false".to_owned()),
            ("supportsAllDrives", "true".to_owned()),
            ("includeRemoved", "true".to_owned()),
        ]);
        self.send(request, credential_id)
            .await?
            .json::<ChangePage>()
            .await
            .context("decode Drive change page")
    }

    pub async fn download_pdf(&self, credential_id: i64, file_id: &str) -> Result<Vec<u8>> {
        let request = self
            .http
            .get(format!("{}/files/{file_id}", self.base_url))
            .query(&[("alt", "media"), ("supportsAllDrives", "true")]);
        let response = self.send(request, credential_id).await?;
        if response
            .content_length()
            .is_some_and(|length| length > self.max_pdf_bytes as u64)
        {
            return Err(rejected("PDF exceeds the configured byte limit"));
        }
        let mut bytes = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.context("read Drive PDF response")?;
            if bytes.len().saturating_add(chunk.len()) > self.max_pdf_bytes {
                return Err(rejected("PDF exceeds the configured byte limit"));
            }
            bytes.extend_from_slice(&chunk);
        }
        if !bytes.starts_with(b"%PDF-") {
            return Err(rejected("Drive response is not a PDF"));
        }
        Ok(bytes)
    }

    async fn send(&self, request: RequestBuilder, credential_id: i64) -> Result<reqwest::Response> {
        for attempt in 1..=DRIVE_REQUEST_ATTEMPTS {
            let access_token = self
                .credentials
                .google_credential(credential_id)
                .await?
                .access_token;
            let response = request
                .try_clone()
                .context("clone Google Drive request for retry")?
                .bearer_auth(access_token)
                .send()
                .await
                .context("send Google Drive request")?;
            let status = response.status();
            if attempt < DRIVE_REQUEST_ATTEMPTS
                && (status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error())
            {
                let delay = if status == StatusCode::TOO_MANY_REQUESTS {
                    retry_after_delay(&response).unwrap_or_else(|| server_retry_delay(attempt))
                } else {
                    server_retry_delay(attempt)
                };
                warn!(
                    event = "company_context_drive_request_retry",
                    credential_id,
                    attempt,
                    status = status.as_u16(),
                    delay_ms = delay.as_millis()
                );
                sleep(delay).await;
                continue;
            }
            if is_permanent_drive_status(status) {
                return Err(rejected(format!(
                    "Google Drive request was rejected with status {status}"
                )));
            }
            return response
                .error_for_status()
                .context("Google Drive request failed");
        }
        unreachable!("Drive request loop always returns on its final attempt")
    }
}

fn server_retry_delay(failed_attempt: u32) -> Duration {
    DRIVE_SERVER_RETRY_BASE.saturating_mul(2_u32.saturating_pow(failed_attempt - 1))
}

fn retry_after_delay(response: &reqwest::Response) -> Option<Duration> {
    let value = response.headers().get(RETRY_AFTER)?.to_str().ok()?;
    parse_retry_after(value, Utc::now())
}

fn parse_retry_after(value: &str, now: DateTime<Utc>) -> Option<Duration> {
    if let Ok(seconds) = value.parse::<u64>() {
        return Some(Duration::from_secs(seconds));
    }
    let retry_at = DateTime::parse_from_rfc2822(value)
        .ok()?
        .with_timezone(&Utc);
    retry_at.signed_duration_since(now).to_std().ok()
}

fn is_permanent_drive_status(status: StatusCode) -> bool {
    matches!(
        status,
        StatusCode::BAD_REQUEST
            | StatusCode::NOT_FOUND
            | StatusCode::METHOD_NOT_ALLOWED
            | StatusCode::GONE
            | StatusCode::LENGTH_REQUIRED
            | StatusCode::PAYLOAD_TOO_LARGE
            | StatusCode::URI_TOO_LONG
            | StatusCode::UNSUPPORTED_MEDIA_TYPE
            | StatusCode::UNPROCESSABLE_ENTITY
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(mime_type: &str, trashed: bool) -> DriveFile {
        DriveFile {
            id: "file-1".to_owned(),
            name: String::new(),
            mime_type: mime_type.to_owned(),
            web_view_link: String::new(),
            drive_id: String::new(),
            version: "42".to_owned(),
            md5_checksum: String::new(),
            trashed,
            created_time: None,
            modified_time: None,
            owners: Vec::new(),
            permissions: Vec::new(),
        }
    }

    #[test]
    fn only_active_non_shared_drive_pdfs_are_processable() {
        assert!(file(PDF_MIME_TYPE, false).is_active_user_pdf());
        assert!(!file(PDF_MIME_TYPE, true).is_active_user_pdf());
        assert!(!file("application/vnd.google-apps.document", false).is_active_user_pdf());

        let mut shared_drive_file = file(PDF_MIME_TYPE, false);
        shared_drive_file.drive_id = "shared-drive-1".to_owned();
        assert!(!shared_drive_file.is_active_user_pdf());
    }

    #[test]
    fn drive_version_is_the_stable_revision_key() {
        assert_eq!(file(PDF_MIME_TYPE, false).source_version(), "42");
    }

    #[test]
    fn retries_servers_exponentially() {
        assert_eq!(server_retry_delay(1), Duration::from_secs(1));
        assert_eq!(server_retry_delay(2), Duration::from_secs(2));
        assert_eq!(server_retry_delay(3), Duration::from_secs(4));
    }

    #[test]
    fn parses_retry_after_seconds_and_dates() {
        let now = DateTime::parse_from_rfc3339("2025-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(parse_retry_after("17", now), Some(Duration::from_secs(17)));
        assert_eq!(
            parse_retry_after("Wed, 01 Jan 2025 00:00:09 GMT", now),
            Some(Duration::from_secs(9))
        );
    }
}
