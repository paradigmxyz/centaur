use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use chrono::{DateTime, NaiveDate, NaiveDateTime, SecondsFormat, Utc};
use reqwest::{Client, StatusCode};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::{config::Config, errors::rejected, telemetry};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
/// The largest page Attio's list endpoints return.
const PAGE_SIZE: &str = "200";

/// Talks to Attio's REST API with a workspace credential.
#[derive(Clone)]
pub struct AttioClient {
    http: Client,
    base_url: String,
}

/// A meeting as Attio lists it. Fields are defaulted because the meetings API
/// is in beta.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(default)]
pub struct Meeting {
    pub id: MeetingId,
    pub title: String,
    pub description: String,
    pub start: MeetingTime,
    pub end: MeetingTime,
    pub participants: Vec<Participant>,
    pub linked_records: Vec<LinkedRecord>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(default)]
pub struct MeetingId {
    pub meeting_id: String,
}

/// A `datetime` for a timed meeting, or a `date` for an all-day one.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(default)]
pub struct MeetingTime {
    pub datetime: Option<String>,
    pub date: Option<String>,
}

impl MeetingTime {
    /// The instant, taking all-day dates and datetimes without an offset as UTC.
    pub fn instant(&self) -> Option<DateTime<Utc>> {
        if let Some(datetime) = self.datetime.as_deref() {
            return DateTime::parse_from_rfc3339(datetime)
                .map(|datetime| datetime.with_timezone(&Utc))
                .or_else(|_| {
                    NaiveDateTime::parse_from_str(datetime, "%Y-%m-%dT%H:%M:%S%.f")
                        .map(|datetime| datetime.and_utc())
                })
                .ok();
        }
        let date = NaiveDate::parse_from_str(self.date.as_deref()?, "%Y-%m-%d").ok()?;
        Some(date.and_hms_opt(0, 0, 0)?.and_utc())
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(default)]
pub struct Participant {
    pub email_address: Option<String>,
    pub name: Option<String>,
    pub is_organizer: bool,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(default)]
pub struct LinkedRecord {
    pub object_slug: String,
    pub record_id: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct CallRecording {
    pub id: CallRecordingId,
    pub status: String,
    pub web_url: String,
    pub transcript: Option<Transcript>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct CallRecordingId {
    pub call_recording_id: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Transcript {
    pub segments: Vec<Segment>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Segment {
    pub speech: String,
    pub speaker: Speaker,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Speaker {
    pub name: String,
}

#[derive(Deserialize)]
struct Page<T> {
    data: Vec<T>,
    #[serde(default)]
    pagination: Pagination,
}

#[derive(Default, Deserialize)]
struct Pagination {
    next_cursor: Option<String>,
}

#[derive(Deserialize)]
struct Single<T> {
    data: T,
}

impl AttioClient {
    pub fn new(config: &Config) -> Result<Self> {
        Ok(Self {
            http: Client::builder().timeout(REQUEST_TIMEOUT).build()?,
            base_url: config.attio_api_base_url.clone(),
        })
    }

    /// Lists the meetings that end at or after `ends_from` and start before
    /// `starts_before`.
    pub async fn list_meetings(
        &self,
        token: &str,
        ends_from: DateTime<Utc>,
        starts_before: DateTime<Utc>,
    ) -> Result<Vec<Meeting>> {
        let ends_from = ends_from.to_rfc3339_opts(SecondsFormat::Secs, true);
        let starts_before = starts_before.to_rfc3339_opts(SecondsFormat::Secs, true);
        self.list(
            token,
            "list_meetings",
            "/meetings",
            &[
                ("sort", "start_asc"),
                ("ends_from", &ends_from),
                ("starts_before", &starts_before),
            ],
        )
        .await
    }

    pub async fn list_call_recordings(
        &self,
        token: &str,
        meeting_id: &str,
    ) -> Result<Vec<CallRecording>> {
        self.list(
            token,
            "list_call_recordings",
            &format!("/meetings/{meeting_id}/call_recordings"),
            &[],
        )
        .await
    }

    /// Gets a call recording with its transcript.
    pub async fn call_recording(
        &self,
        token: &str,
        meeting_id: &str,
        call_recording_id: &str,
    ) -> Result<CallRecording> {
        let single: Single<CallRecording> = self
            .get(
                token,
                "get_call_recording",
                &format!("/meetings/{meeting_id}/call_recordings/{call_recording_id}"),
                &[],
            )
            .await?;
        Ok(single.data)
    }

    async fn list<T: DeserializeOwned>(
        &self,
        token: &str,
        operation: &str,
        path: &str,
        query: &[(&str, &str)],
    ) -> Result<Vec<T>> {
        let mut items = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let mut params = query.to_vec();
            params.push(("limit", PAGE_SIZE));
            if let Some(cursor) = &cursor {
                params.push(("cursor", cursor));
            }
            let page: Page<T> = self.get(token, operation, path, &params).await?;
            items.extend(page.data);
            match page.pagination.next_cursor {
                Some(next) if !next.is_empty() && cursor.as_ref() != Some(&next) => {
                    cursor = Some(next);
                }
                _ => return Ok(items),
            }
        }
    }

    async fn get<T: DeserializeOwned>(
        &self,
        token: &str,
        operation: &str,
        path: &str,
        query: &[(&str, &str)],
    ) -> Result<T> {
        let started = Instant::now();
        let response = self
            .http
            .get(format!("{}{path}", self.base_url))
            .bearer_auth(token)
            .query(query)
            .send()
            .await;
        telemetry::upstream_response("attio", operation, started, &response);
        let response = response.context("send Attio request")?;
        let status = response.status();
        if status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
            bail!("Attio {operation} returned HTTP {status}");
        }
        if !status.is_success() {
            return Err(rejected(format!(
                "Attio {operation} returned HTTP {status}"
            )));
        }
        let body = response.bytes().await.context("read Attio response")?;
        serde_json::from_slice(&body).map_err(|error| {
            rejected(format!(
                "Attio {operation} returned malformed JSON: {error}"
            ))
        })
    }
}

#[cfg(test)]
mod tests {
    use axum::{
        Json, Router,
        extract::{Path, Query},
        http::{HeaderMap, StatusCode},
        response::{IntoResponse, Response},
        routing::get,
    };
    use serde_json::{Value, json};
    use std::collections::HashMap;

    use super::*;
    use crate::errors::is_rejected;

    async fn meetings(
        headers: HeaderMap,
        Query(query): Query<HashMap<String, String>>,
    ) -> Response {
        assert_eq!(headers["authorization"], "Bearer token-1");
        assert_eq!(query["ends_from"], "2026-07-01T00:00:00Z");
        assert_eq!(query["starts_before"], "2026-07-08T00:00:00Z");
        let meeting = |id: &str| {
            json!({
                "id": { "workspace_id": "ws-1", "meeting_id": id },
                "title": "Planning",
                "description": "",
                "is_all_day": false,
                "start": { "datetime": "2026-07-02T09:00:00+02:00", "timezone": "Europe/Berlin" },
                "end": { "date": "2026-07-03" },
                "participants": [
                    { "status": "accepted", "is_organizer": true, "email_address": "ada@example.com", "name": null }
                ],
                "linked_records": [],
                "created_at": "2026-07-01T00:00:00Z",
                "created_by_actor": { "type": "system", "id": null }
            })
        };
        let page = match query.get("cursor").map(String::as_str) {
            None => json!({ "data": [meeting("m-1")], "pagination": { "next_cursor": "c-2" } }),
            Some("c-2") => {
                json!({ "data": [meeting("m-2")], "pagination": { "next_cursor": null } })
            }
            Some(_) => return StatusCode::BAD_REQUEST.into_response(),
        };
        Json(page).into_response()
    }

    async fn recording(Path((meeting_id, recording_id)): Path<(String, String)>) -> Response {
        match (meeting_id.as_str(), recording_id.as_str()) {
            ("m-1", "r-1") => Json(json!({ "data": {
                "id": { "workspace_id": "ws-1", "meeting_id": "m-1", "call_recording_id": "r-1" },
                "status": "completed",
                "web_url": "https://app.attio.com/r-1",
                "transcript": { "segments": [
                    { "speech": "Hello.", "start_time": 0.0, "end_time": 1.0, "speaker": { "name": "Ada" } }
                ], "raw_transcript": "Hello." }
            } }))
            .into_response(),
            ("m-busy", _) => StatusCode::TOO_MANY_REQUESTS.into_response(),
            _ => (StatusCode::NOT_FOUND, Json(Value::Null)).into_response(),
        }
    }

    #[tokio::test]
    async fn lists_every_page_and_classifies_failures() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let router = Router::new().route("/meetings", get(meetings)).route(
            "/meetings/{meeting_id}/call_recordings/{recording_id}",
            get(recording),
        );
        let server = tokio::spawn(async move { axum::serve(listener, router).await });
        let client = AttioClient {
            http: Client::new(),
            base_url: format!("http://{address}"),
        };
        let at = |value: &str| DateTime::parse_from_rfc3339(value).unwrap().to_utc();

        let meetings = client
            .list_meetings(
                "token-1",
                at("2026-07-01T00:00:00Z"),
                at("2026-07-08T00:00:00Z"),
            )
            .await
            .unwrap();
        assert_eq!(
            meetings
                .iter()
                .map(|meeting| meeting.id.meeting_id.as_str())
                .collect::<Vec<_>>(),
            ["m-1", "m-2"]
        );
        assert_eq!(
            meetings[0].start.instant(),
            Some(at("2026-07-02T07:00:00Z"))
        );
        assert_eq!(meetings[0].end.instant(), Some(at("2026-07-03T00:00:00Z")));
        assert_eq!(
            meetings[0].participants[0].email_address.as_deref(),
            Some("ada@example.com")
        );

        let recording = client
            .call_recording("token-1", "m-1", "r-1")
            .await
            .unwrap();
        assert_eq!(recording.status, "completed");
        assert_eq!(
            recording.transcript.unwrap().segments[0].speaker.name,
            "Ada"
        );
        let missing = client
            .call_recording("token-1", "m-gone", "r-1")
            .await
            .unwrap_err();
        assert!(is_rejected(&missing), "client errors are not retried");
        let limited = client
            .call_recording("token-1", "m-busy", "r-1")
            .await
            .unwrap_err();
        assert!(!is_rejected(&limited), "rate limits are retried");
        server.abort();
    }

    #[test]
    fn datetimes_without_an_offset_are_utc() {
        let time = MeetingTime {
            datetime: Some("2026-07-02T09:00:00".to_owned()),
            date: None,
        };
        assert_eq!(
            time.instant().unwrap().to_rfc3339(),
            "2026-07-02T09:00:00+00:00"
        );
        assert_eq!(MeetingTime::default().instant(), None);
    }
}
