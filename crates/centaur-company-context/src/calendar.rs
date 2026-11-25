use std::sync::Arc;

use anyhow::{Context, Result};
use chrono::{DateTime, NaiveDate, Utc};
use reqwest::{Client, StatusCode};
use serde::Deserialize;
use serde_json::json;
use url::Url;

use crate::{
    config::Config,
    credentials::{ConsoleCredentials, GoogleProduct},
    drive::send_google,
    extraction::hex_sha256,
};

const CALENDAR_LIST_FIELDS: &str = "nextPageToken,items(id,summary)";
const EVENT_FIELDS: &str = "nextPageToken,nextSyncToken,items(id,status,summary,description,location,htmlLink,start,end,organizer(email,displayName),attendees(email,displayName,resource),recurrence)";
const EVENTS_PAGE_SIZE: &str = "250";

/// Reads calendars and events with users' Google broker credentials.
#[derive(Clone)]
pub struct CalendarClient {
    http: Client,
    base_url: String,
    credentials: Arc<ConsoleCredentials>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CalendarListEntry {
    pub id: String,
    #[serde(default)]
    pub summary: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CalendarListPage {
    #[serde(default)]
    items: Vec<CalendarListEntry>,
    next_page_token: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventPage {
    #[serde(default)]
    pub items: Vec<Event>,
    pub next_page_token: Option<String>,
    pub next_sync_token: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Event {
    pub id: String,
    pub status: String,
    pub summary: String,
    pub description: String,
    pub location: String,
    pub html_link: String,
    pub start: EventTime,
    pub end: EventTime,
    pub organizer: Person,
    pub attendees: Vec<Person>,
    pub recurrence: Vec<String>,
}

/// An event boundary: `date_time` for timed events, `date` for all-day events.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct EventTime {
    pub date_time: Option<DateTime<chrono::FixedOffset>>,
    pub date: Option<NaiveDate>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Person {
    pub email: String,
    pub display_name: String,
    pub resource: bool,
}

/// An event rendered for publication.
#[derive(Debug, PartialEq)]
pub struct RenderedEvent {
    pub title: String,
    pub content_text: String,
    pub url: String,
    pub recurring: bool,
    pub starts_at: Option<DateTime<Utc>>,
    pub ends_at: Option<DateTime<Utc>>,
    pub organizer_email: String,
    pub content_hash: String,
}

impl EventTime {
    /// The boundary in UTC; all-day boundaries are midnight UTC.
    fn utc(&self) -> Option<DateTime<Utc>> {
        self.date_time
            .map(|value| value.with_timezone(&Utc))
            .or_else(|| self.date?.and_hms_opt(0, 0, 0).map(|value| value.and_utc()))
    }

    fn label(&self) -> Option<String> {
        match (self.date_time, self.date) {
            (Some(value), _) => Some(value.format("%Y-%m-%d %H:%M (UTC%:z)").to_string()),
            (None, Some(date)) => Some(format!("{date} (all day)")),
            (None, None) => None,
        }
    }
}

impl Person {
    fn label(&self) -> String {
        match (self.display_name.trim(), self.email.trim()) {
            ("", email) => email.to_owned(),
            (name, "") => name.to_owned(),
            (name, email) => format!("{name} <{email}>"),
        }
    }
}

impl Event {
    /// Renders the event, or returns `None` for a cancelled event.
    /// Recurring events are rendered with the times of their first occurrence.
    pub fn render(&self) -> Option<RenderedEvent> {
        if self.status == "cancelled" {
            return None;
        }
        let title = match self.summary.trim() {
            "" => "Untitled event".to_owned(),
            summary => summary.to_owned(),
        };
        let mut lines = vec![format!("# {title}"), String::new()];
        lines.extend(self.start.label().map(|value| format!("- Starts: {value}")));
        lines.extend(self.end.label().map(|value| format!("- Ends: {value}")));
        if self.status == "tentative" {
            lines.push("- Status: tentative".to_owned());
        }
        if !self.recurrence.is_empty() {
            lines.push(format!("- Recurrence: {}", self.recurrence.join("; ")));
        }
        if !self.location.trim().is_empty() {
            lines.push(format!("- Location: {}", self.location.trim()));
        }
        let organizer = self.organizer.label();
        if !organizer.is_empty() {
            lines.push(format!("- Organizer: {organizer}"));
        }
        // Responses are left out, so an RSVP does not rewrite the document.
        let attendees: Vec<String> = self
            .attendees
            .iter()
            .filter(|attendee| !attendee.resource)
            .map(Person::label)
            .filter(|label| !label.is_empty())
            .collect();
        if !attendees.is_empty() {
            lines.push(format!("- Attendees: {}", attendees.join(", ")));
        }
        let description = plain_text(&self.description);
        if !description.is_empty() {
            lines.extend([String::new(), description]);
        }
        let content_text = lines.join("\n");
        let starts_at = self.start.utc();
        let ends_at = self.end.utc();
        let organizer_email = self.organizer.email.trim().to_lowercase();
        let content_hash = hex_sha256(
            json!([
                title,
                content_text,
                self.html_link,
                starts_at,
                ends_at,
                organizer_email
            ])
            .to_string()
            .as_bytes(),
        );
        Some(RenderedEvent {
            title,
            content_text,
            url: self.html_link.clone(),
            recurring: !self.recurrence.is_empty(),
            starts_at,
            ends_at,
            organizer_email,
            content_hash,
        })
    }
}

/// HTML tags Google Calendar descriptions use. Other `<...>` text, such as an
/// address in angle brackets, is kept.
const HTML_TAGS: [&str; 30] = [
    "a",
    "b",
    "blockquote",
    "br",
    "code",
    "div",
    "em",
    "font",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "hr",
    "html-blob",
    "i",
    "img",
    "li",
    "ol",
    "p",
    "pre",
    "s",
    "span",
    "strong",
    "table",
    "td",
    "tr",
    "u",
    "ul",
];
/// Tags that end a line.
const HTML_BLOCK_TAGS: [&str; 16] = [
    "blockquote",
    "br",
    "div",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "hr",
    "li",
    "ol",
    "p",
    "pre",
    "tr",
    "ul",
];

/// Converts a description, which Google often stores as HTML, to plain text.
fn plain_text(description: &str) -> String {
    let mut text = String::with_capacity(description.len());
    let mut rest = description;
    while let Some(start) = rest.find('<') {
        text.push_str(&rest[..start]);
        let tag = &rest[start..];
        let Some(end) = tag.find('>') else {
            rest = tag;
            break;
        };
        let name = tag[1..end]
            .trim_start_matches('/')
            .split(|c: char| c.is_whitespace() || c == '/')
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
        if HTML_BLOCK_TAGS.contains(&name.as_str()) {
            text.push('\n');
        } else if !HTML_TAGS.contains(&name.as_str()) {
            text.push_str(&tag[..=end]);
        }
        rest = &tag[end + 1..];
    }
    text.push_str(rest);
    let text = text
        .replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&");
    // Trim each line and keep at most one blank line between paragraphs.
    let mut lines: Vec<&str> = Vec::new();
    for line in text.lines().map(str::trim) {
        if !line.is_empty() || lines.last().is_some_and(|last| !last.is_empty()) {
            lines.push(line);
        }
    }
    lines.join("\n").trim().to_owned()
}

impl CalendarClient {
    pub fn new(config: &Config, credentials: Arc<ConsoleCredentials>) -> Result<Self> {
        Ok(Self {
            http: Client::builder()
                .timeout(config.extraction_timeout)
                .build()?,
            base_url: config.google_calendar_api_base_url.clone(),
            credentials,
        })
    }

    /// Lists the calendars the credential can read event details of. Google
    /// redacts the private events of calendars it can only read.
    pub async fn list_calendars(&self, credential_id: i64) -> Result<Vec<CalendarListEntry>> {
        let mut calendars = Vec::new();
        let mut page_token = None;
        loop {
            let mut request = self
                .http
                .get(format!("{}/users/me/calendarList", self.base_url))
                .query(&[
                    ("minAccessRole", "reader"),
                    ("showHidden", "true"),
                    ("fields", CALENDAR_LIST_FIELDS),
                ]);
            if let Some(page_token) = &page_token {
                request = request.query(&[("pageToken", page_token)]);
            }
            let page: CalendarListPage = self
                .send(request, credential_id, "calendarList.list", &[])
                .await?
                .json()
                .await
                .context("decode Google Calendar list page")?;
            calendars.extend(
                page.items
                    .into_iter()
                    .filter(|calendar| !calendar.id.is_empty()),
            );
            page_token = page.next_page_token;
            if page_token.is_none() {
                return Ok(calendars);
            }
        }
    }

    /// Lists a page of events: the page `page_token` names, the changes since
    /// `sync_token`, or else the first page of a full sync. Returns `None` when
    /// Google has expired the tokens and requires a full sync instead.
    pub async fn list_events(
        &self,
        credential_id: i64,
        calendar_id: &str,
        page_token: Option<&str>,
        sync_token: Option<&str>,
    ) -> Result<Option<EventPage>> {
        let mut url = Url::parse(&self.base_url).context("parse Google Calendar API URL")?;
        url.path_segments_mut()
            .map_err(|_| anyhow::anyhow!("Google Calendar API URL cannot have a path"))?
            .extend(["calendars", calendar_id, "events"]);
        let mut request = self
            .http
            .get(url)
            .query(&[("maxResults", EVENTS_PAGE_SIZE), ("fields", EVENT_FIELDS)]);
        // Pages of incremental changes repeat the sync token they follow.
        if let Some(page_token) = page_token {
            request = request.query(&[("pageToken", page_token)]);
        }
        if let Some(sync_token) = sync_token {
            request = request.query(&[("syncToken", sync_token)]);
        }
        let response = self
            .send(request, credential_id, "events.list", &[StatusCode::GONE])
            .await?;
        if response.status() == StatusCode::GONE {
            return Ok(None);
        }
        Ok(Some(
            response
                .json()
                .await
                .context("decode Google Calendar event page")?,
        ))
    }

    async fn send(
        &self,
        request: reqwest::RequestBuilder,
        credential_id: i64,
        operation: &'static str,
        passthrough: &[StatusCode],
    ) -> Result<reqwest::Response> {
        send_google(
            &self.credentials,
            request,
            credential_id,
            GoogleProduct::Calendar,
            operation,
            passthrough,
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(value: serde_json::Value) -> Event {
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn events_render_their_details() {
        let rendered = event(json!({
            "id": "e1",
            "status": "confirmed",
            "summary": "Falcon launch review",
            "description": "Agenda: go/no-go",
            "location": "Room 4",
            "htmlLink": "https://calendar.example.com/e1",
            "start": { "dateTime": "2024-01-02T09:00:00+01:00", "timeZone": "Europe/Berlin" },
            "end": { "dateTime": "2024-01-02T10:00:00+01:00" },
            "organizer": { "email": "Ada@Example.com", "displayName": "Ada" },
            "attendees": [
                { "email": "ada@example.com", "displayName": "Ada", "responseStatus": "accepted" },
                { "email": "bob@example.com", "responseStatus": "needsAction" },
                { "email": "room@resource.example.com", "resource": true }
            ],
            "recurrence": ["RRULE:FREQ=WEEKLY;BYDAY=TU"],
            "unknownField": true
        }))
        .render()
        .unwrap();
        assert_eq!(rendered.title, "Falcon launch review");
        assert_eq!(
            rendered.content_text,
            "# Falcon launch review\n\n\
             - Starts: 2024-01-02 09:00 (UTC+01:00)\n\
             - Ends: 2024-01-02 10:00 (UTC+01:00)\n\
             - Recurrence: RRULE:FREQ=WEEKLY;BYDAY=TU\n\
             - Location: Room 4\n\
             - Organizer: Ada <Ada@Example.com>\n\
             - Attendees: Ada <ada@example.com>, bob@example.com\n\n\
             Agenda: go/no-go"
        );
        assert_eq!(
            rendered.starts_at,
            Some("2024-01-02T08:00:00Z".parse().unwrap())
        );
        assert_eq!(rendered.organizer_email, "ada@example.com");
        assert!(rendered.recurring);
    }

    #[test]
    fn html_descriptions_become_plain_text() {
        assert_eq!(
            plain_text(
                "<b>Agenda</b><br>1. Go/no-go&nbsp;&amp; dates<br><br><br>\
                 <ul><li>Docs: <a href=\"https://example.com\">deck</a></li></ul>\
                 Ping Ada &lt;ada@example.com&gt;"
            ),
            "Agenda\n1. Go/no-go & dates\n\nDocs: deck\n\nPing Ada <ada@example.com>"
        );
        assert_eq!(
            plain_text("Ask <ada@example.com> if 2 < 3"),
            "Ask <ada@example.com> if 2 < 3"
        );
    }

    #[test]
    fn all_day_untitled_and_cancelled_events() {
        let rendered = event(json!({
            "id": "e2",
            "start": { "date": "2024-03-01" },
            "end": { "date": "2024-03-02" }
        }))
        .render()
        .unwrap();
        assert_eq!(
            rendered.content_text,
            "# Untitled event\n\n- Starts: 2024-03-01 (all day)\n- Ends: 2024-03-02 (all day)"
        );
        assert_eq!(
            rendered.starts_at,
            Some("2024-03-01T00:00:00Z".parse().unwrap())
        );
        assert!(!rendered.recurring);

        assert!(
            event(json!({ "id": "e3", "status": "cancelled" }))
                .render()
                .is_none()
        );
    }
}
