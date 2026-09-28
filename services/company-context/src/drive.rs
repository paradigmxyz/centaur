use std::{sync::Arc, time::Duration};

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use futures_util::StreamExt;
use reqwest::{Client, RequestBuilder, StatusCode, header::RETRY_AFTER};
use serde::{Deserialize, Serialize};
use tokio::time::sleep;
use tracing::warn;

use crate::{
    config::{Config, FOLDER_MIME_TYPE, PDF_MIME_TYPE},
    credentials::ConsoleCredentials,
    errors::rejected,
};

const DRIVE_REQUEST_ATTEMPTS: u32 = 4;
const FILE_FIELDS: &str = "id,name,mimeType,webViewLink,driveId,version,md5Checksum,trashed,createdTime,modifiedTime,owners(displayName,emailAddress),permissions(id,type,role,emailAddress,domain,allowFileDiscovery)";
/// Drive caps pages at 100 when permissions are requested, and omits Shared
/// Drive permissions for non-members anyway, so walks list without them.
const WALK_PAGE_SIZE: u16 = 1_000;
const WALK_FILE_FIELDS: &str = "id,name,mimeType,webViewLink,driveId,version,md5Checksum,trashed,createdTime,modifiedTime,owners(displayName,emailAddress)";
const FOLDER_OR_PDF_QUERY: &str = "trashed = false and (mimeType = 'application/vnd.google-apps.folder' or mimeType = 'application/pdf')";
/// The user corpus plus Shared Drive items the user can reach without membership.
const ACCESSIBLE_CORPUS: &[(&str, &str)] =
    &[("corpora", "user"), ("includeItemsFromAllDrives", "true")];
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
    pub fn is_active_pdf(&self) -> bool {
        !self.trashed && self.mime_type == PDF_MIME_TYPE && !self.id.is_empty()
    }

    pub fn is_active_folder(&self) -> bool {
        !self.trashed && self.mime_type == FOLDER_MIME_TYPE && !self.id.is_empty()
    }

    /// Whether the file lives in the given Shared Drive, or in My Drive for `None`.
    pub fn belongs_to(&self, shared_drive_id: Option<&str>) -> bool {
        self.drive_id == shared_drive_id.unwrap_or_default()
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
pub struct SharedDrivePage {
    #[serde(default)]
    pub drives: Vec<SharedDrive>,
    pub next_page_token: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct SharedDrive {
    pub id: String,
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

    pub async fn start_page_token(
        &self,
        credential_id: i64,
        shared_drive_id: Option<&str>,
    ) -> Result<String> {
        let mut request = self
            .http
            .get(format!("{}/changes/startPageToken", self.base_url))
            .query(&[("supportsAllDrives", "true")]);
        if let Some(drive_id) = shared_drive_id {
            request = request.query(&[("driveId", drive_id)]);
        }
        let response = self
            .send(request, credential_id)
            .await?
            .json::<StartPageToken>()
            .await
            .context("decode Drive start page token")?;
        if response.start_page_token.is_empty() {
            bail!("Drive returned an empty start page token");
        }
        Ok(response.start_page_token)
    }

    pub async fn list_shared_drives(
        &self,
        credential_id: i64,
        page_token: Option<&str>,
    ) -> Result<SharedDrivePage> {
        let mut request = self
            .http
            .get(format!("{}/drives", self.base_url))
            .query(&[("pageSize", "100"), ("fields", "nextPageToken,drives(id)")]);
        if let Some(page_token) = page_token {
            request = request.query(&[("pageToken", page_token)]);
        }
        self.send(request, credential_id)
            .await?
            .json::<SharedDrivePage>()
            .await
            .context("decode Drive shared drive page")
    }

    /// Lists PDFs in the user's corpus, or in one Shared Drive when `shared_drive_id` is set.
    pub async fn list_pdfs(
        &self,
        credential_id: i64,
        shared_drive_id: Option<&str>,
        page_size: u16,
        page_token: Option<&str>,
    ) -> Result<FilePage> {
        let corpus: &[(&str, &str)] = match shared_drive_id {
            Some(drive_id) => &[
                ("corpora", "drive"),
                ("driveId", drive_id),
                ("includeItemsFromAllDrives", "true"),
            ],
            None => &[("corpora", "user"), ("includeItemsFromAllDrives", "false")],
        };
        self.list_files(
            credential_id,
            &format!("mimeType = '{PDF_MIME_TYPE}' and trashed = false"),
            corpus,
            FILE_FIELDS,
            page_size,
            page_token,
        )
        .await
    }

    /// Lists folders and PDFs shared with the user, including Shared Drive items
    /// shared with users who are not members of that drive.
    pub async fn list_shared_with_me(
        &self,
        credential_id: i64,
        page_token: Option<&str>,
    ) -> Result<FilePage> {
        self.list_files(
            credential_id,
            &format!("sharedWithMe = true and {FOLDER_OR_PDF_QUERY}"),
            ACCESSIBLE_CORPUS,
            WALK_FILE_FIELDS,
            WALK_PAGE_SIZE,
            page_token,
        )
        .await
    }

    /// Lists the folders and PDFs directly inside any of the given folders.
    pub async fn list_folder_children(
        &self,
        credential_id: i64,
        folder_ids: &[String],
        page_token: Option<&str>,
    ) -> Result<FilePage> {
        if folder_ids.is_empty() {
            bail!("no Drive folders to list");
        }
        if !folder_ids.iter().all(|id| is_drive_id(id)) {
            return Err(rejected("Drive folder ID contains unexpected characters"));
        }
        self.list_files(
            credential_id,
            &folder_children_query(folder_ids),
            ACCESSIBLE_CORPUS,
            WALK_FILE_FIELDS,
            WALK_PAGE_SIZE,
            page_token,
        )
        .await
    }

    async fn list_files(
        &self,
        credential_id: i64,
        query: &str,
        corpus: &[(&str, &str)],
        file_fields: &str,
        page_size: u16,
        page_token: Option<&str>,
    ) -> Result<FilePage> {
        let fields = format!("nextPageToken,incompleteSearch,files({file_fields})");
        let mut request = self
            .http
            .get(format!("{}/files", self.base_url))
            .query(&[
                ("q", query),
                ("pageSize", &page_size.to_string()),
                ("fields", &fields),
                ("supportsAllDrives", "true"),
            ])
            .query(corpus);
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
            bail!("Google Drive reported an incomplete corpus search");
        }
        Ok(page)
    }

    pub async fn list_changes(
        &self,
        credential_id: i64,
        shared_drive_id: Option<&str>,
        page_size: u16,
        page_token: &str,
    ) -> Result<ChangePage> {
        let fields =
            format!("nextPageToken,newStartPageToken,changes(fileId,removed,file({FILE_FIELDS}))");
        let mut request = self.http.get(format!("{}/changes", self.base_url)).query(&[
            ("pageToken", page_token.to_owned()),
            ("pageSize", page_size.to_string()),
            ("fields", fields),
            ("supportsAllDrives", "true".to_owned()),
            ("includeRemoved", "true".to_owned()),
        ]);
        request = match shared_drive_id {
            Some(drive_id) => {
                request.query(&[("driveId", drive_id), ("includeItemsFromAllDrives", "true")])
            }
            None => request.query(&[("includeItemsFromAllDrives", "false")]),
        };
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

fn folder_children_query(folder_ids: &[String]) -> String {
    let parents = folder_ids
        .iter()
        .map(|id| format!("'{id}' in parents"))
        .collect::<Vec<_>>()
        .join(" or ");
    format!("({parents}) and {FOLDER_OR_PDF_QUERY}")
}

fn is_drive_id(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
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
    fn only_active_pdfs_are_processable() {
        assert!(file(PDF_MIME_TYPE, false).is_active_pdf());
        assert!(!file(PDF_MIME_TYPE, true).is_active_pdf());
        assert!(!file("application/vnd.google-apps.document", false).is_active_pdf());

        let mut shared_drive_file = file(PDF_MIME_TYPE, false);
        shared_drive_file.drive_id = "shared-drive-1".to_owned();
        assert!(shared_drive_file.is_active_pdf());
    }

    #[test]
    fn files_belong_to_exactly_one_corpus() {
        let my_drive_file = file(PDF_MIME_TYPE, false);
        assert!(my_drive_file.belongs_to(None));
        assert!(!my_drive_file.belongs_to(Some("shared-drive-1")));

        let mut shared_drive_file = file(PDF_MIME_TYPE, false);
        shared_drive_file.drive_id = "shared-drive-1".to_owned();
        assert!(!shared_drive_file.belongs_to(None));
        assert!(shared_drive_file.belongs_to(Some("shared-drive-1")));
        assert!(!shared_drive_file.belongs_to(Some("shared-drive-2")));
    }

    #[test]
    fn folder_ids_cannot_escape_the_parents_query() {
        assert!(is_drive_id("0AbC-d_9"));
        assert!(!is_drive_id(""));
        assert!(!is_drive_id("x' or '1' = '1"));
        assert!(!is_drive_id("x\\"));
    }

    #[test]
    fn batched_folder_query_filters_every_parent() {
        let query = folder_children_query(&["a".to_owned(), "b".to_owned()]);
        assert!(query.starts_with("('a' in parents or 'b' in parents) and trashed = false and ("));
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
