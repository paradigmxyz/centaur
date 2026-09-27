use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use futures_util::StreamExt;
use reqwest::{Client, RequestBuilder};
use serde::{Deserialize, Serialize};

use crate::config::{Config, PDF_MIME_TYPE};

#[derive(Clone)]
pub struct DriveClient {
    http: Client,
    base_url: String,
    access_token: Option<String>,
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
    pub fn new(config: &Config) -> Result<Self> {
        Ok(Self {
            http: Client::builder()
                .timeout(config.extraction_timeout)
                .build()?,
            base_url: config.google_api_base_url.clone(),
            access_token: config.google_access_token.clone(),
            max_pdf_bytes: config.max_pdf_bytes,
        })
    }

    pub async fn start_page_token(&self) -> Result<String> {
        let response = self
            .send(
                self.authorize(
                    self.http
                        .get(format!("{}/changes/startPageToken", self.base_url)),
                ),
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

    pub async fn list_pdfs(&self, page_size: u16, page_token: Option<&str>) -> Result<FilePage> {
        let fields = "nextPageToken,incompleteSearch,files(id,name,mimeType,webViewLink,driveId,version,md5Checksum,trashed,createdTime,modifiedTime,owners(displayName,emailAddress),permissions(id,type,role,emailAddress,domain,allowFileDiscovery))";
        let mut request = self.http.get(format!("{}/files", self.base_url)).query(&[
            (
                "q",
                "mimeType = 'application/pdf' and trashed = false".to_owned(),
            ),
            ("pageSize", page_size.to_string()),
            ("fields", fields.to_owned()),
            ("corpora", "allDrives".to_owned()),
            ("includeItemsFromAllDrives", "true".to_owned()),
            ("supportsAllDrives", "true".to_owned()),
            ("orderBy", "modifiedTime".to_owned()),
        ]);
        if let Some(page_token) = page_token {
            request = request.query(&[("pageToken", page_token)]);
        }
        let page = self
            .send(self.authorize(request))
            .await?
            .json::<FilePage>()
            .await
            .context("decode Drive file page")?;
        if page.incomplete_search {
            bail!("Google Drive reported an incomplete all-drives search");
        }
        Ok(page)
    }

    pub async fn list_changes(&self, page_size: u16, page_token: &str) -> Result<ChangePage> {
        let fields = "nextPageToken,newStartPageToken,changes(fileId,removed,file(id,name,mimeType,webViewLink,driveId,version,md5Checksum,trashed,createdTime,modifiedTime,owners(displayName,emailAddress),permissions(id,type,role,emailAddress,domain,allowFileDiscovery)))";
        let request = self.http.get(format!("{}/changes", self.base_url)).query(&[
            ("pageToken", page_token.to_owned()),
            ("pageSize", page_size.to_string()),
            ("fields", fields.to_owned()),
            ("includeItemsFromAllDrives", "true".to_owned()),
            ("supportsAllDrives", "true".to_owned()),
            ("includeRemoved", "true".to_owned()),
        ]);
        self.send(self.authorize(request))
            .await?
            .json::<ChangePage>()
            .await
            .context("decode Drive change page")
    }

    pub async fn download_pdf(&self, file_id: &str) -> Result<Vec<u8>> {
        let request = self
            .http
            .get(format!("{}/files/{file_id}", self.base_url))
            .query(&[("alt", "media"), ("supportsAllDrives", "true")]);
        let response = self.send(self.authorize(request)).await?;
        if response
            .content_length()
            .is_some_and(|length| length > self.max_pdf_bytes as u64)
        {
            bail!("PDF exceeds the configured byte limit");
        }
        let mut bytes = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.context("read Drive PDF response")?;
            if bytes.len().saturating_add(chunk.len()) > self.max_pdf_bytes {
                bail!("PDF exceeds the configured byte limit");
            }
            bytes.extend_from_slice(&chunk);
        }
        if !bytes.starts_with(b"%PDF-") {
            bail!("Drive response is not a PDF");
        }
        Ok(bytes)
    }

    fn authorize(&self, request: RequestBuilder) -> RequestBuilder {
        match &self.access_token {
            Some(token) => request.bearer_auth(token),
            None => request,
        }
    }

    async fn send(&self, request: RequestBuilder) -> Result<reqwest::Response> {
        request
            .send()
            .await
            .context("send Google Drive request")?
            .error_for_status()
            .context("Google Drive request failed")
    }
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
    }

    #[test]
    fn drive_version_is_the_stable_revision_key() {
        assert_eq!(file(PDF_MIME_TYPE, false).source_version(), "42");
    }
}
