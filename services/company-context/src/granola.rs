use std::time::Duration;

use anyhow::{Context, Result, bail};
use chrono::{DateTime, FixedOffset, NaiveDate, NaiveDateTime, TimeZone};
use reqwest::{Client, StatusCode, header::CONTENT_TYPE};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tracing::warn;

use crate::{config::Config, credentials::GranolaCredential, errors::rejected};

const MCP_PROTOCOL_VERSION: &str = "2025-03-26";
const MCP_REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

/// Talks to Granola's MCP server with a user's broker credential. The MCP
/// server exposes meetings as XML-like text inside tool results.
#[derive(Clone)]
pub struct GranolaClient {
    http: Client,
    mcp_url: String,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
pub struct Participant {
    pub name: String,
    pub email: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct Meeting {
    pub id: String,
    pub title: String,
    /// Granola's display date, such as `Jul 8, 2026 5:30 PM GMT+2`.
    pub date: String,
    pub owner: Option<Participant>,
    pub attendees: Vec<Participant>,
    pub summary_markdown: String,
}

impl Meeting {
    pub fn occurred_at(&self) -> Option<DateTime<FixedOffset>> {
        parse_meeting_date(&self.date)
    }
}

pub struct McpSession<'a> {
    client: &'a GranolaClient,
    access_token: String,
    session_id: Option<String>,
    rpc_id: u64,
}

impl GranolaClient {
    pub fn new(config: &Config) -> Result<Self> {
        Ok(Self {
            http: Client::builder().timeout(MCP_REQUEST_TIMEOUT).build()?,
            mcp_url: config.granola_mcp_url.clone(),
        })
    }

    pub async fn session(&self, credential: &GranolaCredential) -> Result<McpSession<'_>> {
        let mut session = McpSession {
            client: self,
            access_token: credential.access_token.clone(),
            session_id: None,
            rpc_id: 0,
        };
        session.initialize().await?;
        Ok(session)
    }
}

impl McpSession<'_> {
    pub async fn account_email(&mut self) -> Result<String> {
        let text = self.call_tool("get_account_info", json!({})).await?;
        let account: Value = serde_json::from_str(&text)
            .map_err(|_| rejected("Granola MCP returned an invalid account response"))?;
        Ok(account
            .get("email")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_lowercase())
    }

    pub async fn list_meetings(
        &mut self,
        start: NaiveDate,
        end: NaiveDate,
    ) -> Result<Vec<Meeting>> {
        let text = self
            .call_tool(
                "list_meetings",
                json!({
                    "time_range": "custom",
                    "custom_start": start.to_string(),
                    "custom_end": end.to_string(),
                }),
            )
            .await?;
        let (meetings, reported) = parse_meetings(&text)?;
        if meetings.is_empty() && reported > 0 {
            return Err(rejected(
                "Granola MCP reported meetings that could not be parsed",
            ));
        }
        Ok(meetings)
    }

    pub async fn get_meetings(&mut self, meeting_ids: &[String]) -> Result<Vec<Meeting>> {
        let text = self
            .call_tool("get_meetings", json!({ "meeting_ids": meeting_ids }))
            .await?;
        Ok(parse_meetings(&text)?.0)
    }

    async fn initialize(&mut self) -> Result<()> {
        let result = self
            .request(
                "initialize",
                json!({
                    "protocolVersion": MCP_PROTOCOL_VERSION,
                    "capabilities": {},
                    "clientInfo": { "name": "centaur-company-context", "version": "1.0" },
                }),
            )
            .await?;
        if result.is_none() {
            return Err(rejected("Granola MCP did not acknowledge initialization"));
        }
        if self.session_id.is_some()
            && let Err(error) = self.notify("notifications/initialized").await
        {
            warn!(event = "company_context_granola_initialized_notification_failed", error = %error);
        }
        Ok(())
    }

    async fn call_tool(&mut self, name: &str, arguments: Value) -> Result<String> {
        let result = self
            .request(
                "tools/call",
                json!({ "name": name, "arguments": arguments }),
            )
            .await?
            .unwrap_or_default();
        let text = result
            .get("content")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|content| content.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|content| content.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n");
        if result.get("isError").and_then(Value::as_bool) == Some(true) {
            let detail = text.split_whitespace().collect::<Vec<_>>().join(" ");
            let detail: String = detail.chars().take(1_000).collect();
            let suffix = if detail.is_empty() {
                String::new()
            } else {
                format!(": {detail}")
            };
            return Err(rejected(format!("Granola MCP tool {name} failed{suffix}")));
        }
        Ok(text)
    }

    async fn notify(&mut self, method: &str) -> Result<()> {
        self.send(json!({ "jsonrpc": "2.0", "method": method, "params": {} }))
            .await?;
        Ok(())
    }

    async fn request(&mut self, method: &str, params: Value) -> Result<Option<Value>> {
        self.rpc_id += 1;
        let response = self
            .send(json!({
                "jsonrpc": "2.0",
                "id": self.rpc_id,
                "method": method,
                "params": params,
            }))
            .await?;
        if method == "initialize" {
            self.session_id = response
                .headers()
                .get("mcp-session-id")
                .and_then(|value| value.to_str().ok())
                .filter(|value| !value.is_empty())
                .map(str::to_owned);
        }
        let event_stream = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.starts_with("text/event-stream"));
        let body = response.text().await.context("read Granola MCP response")?;
        let mut payload = decode_mcp_body(&body, event_stream)?;
        if let Some(error) = payload.get("error") {
            return Err(rejected(format!("Granola MCP returned {error}")));
        }
        Ok(payload.get_mut("result").map(Value::take))
    }

    async fn send(&self, payload: Value) -> Result<reqwest::Response> {
        let mut request = self
            .client
            .http
            .post(&self.client.mcp_url)
            .bearer_auth(&self.access_token)
            .header("Accept", "application/json, text/event-stream")
            .json(&payload);
        if let Some(session_id) = &self.session_id {
            request = request
                .header("MCP-Protocol-Version", MCP_PROTOCOL_VERSION)
                .header("MCP-Session-Id", session_id);
        }
        let response = request.send().await.context("send Granola MCP request")?;
        let status = response.status();
        if status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
            bail!("Granola MCP returned HTTP {status}");
        }
        if !status.is_success() {
            return Err(rejected(format!("Granola MCP returned HTTP {status}")));
        }
        Ok(response)
    }
}

fn decode_mcp_body(body: &str, event_stream: bool) -> Result<Value> {
    let body = if event_stream {
        body.lines()
            .filter_map(|line| line.strip_prefix("data: "))
            .map(str::trim)
            .next_back()
            .ok_or_else(|| rejected("Granola MCP returned an empty event stream"))?
    } else {
        body
    };
    serde_json::from_str(body).map_err(|_| rejected("Granola MCP returned malformed JSON"))
}

/// Parses meetings from Granola's tool text and returns them with the count
/// Granola reported, so callers can detect meetings that failed to parse.
fn parse_meetings(text: &str) -> Result<(Vec<Meeting>, usize)> {
    let wrapped = format!("<granola_response>{text}</granola_response>");
    let document = roxmltree::Document::parse(&wrapped).map_err(|error| {
        rejected(format!(
            "Granola MCP returned malformed meeting XML: {error}"
        ))
    })?;
    let reported = document
        .descendants()
        .find(|node| node.has_tag_name("meetings_data"))
        .and_then(|node| node.attribute("count"))
        .and_then(|count| count.parse().ok())
        .unwrap_or(0);
    let meetings = document
        .descendants()
        .filter(|node| node.has_tag_name("meeting"))
        .filter_map(|node| {
            let attribute = |name| {
                node.attribute(name)
                    .filter(|value| !value.is_empty())
                    .map(str::to_owned)
            };
            let child_text = |name| {
                node.children()
                    .find(|child| child.has_tag_name(name))
                    .map(|child| {
                        child
                            .descendants()
                            .filter(|descendant| descendant.is_text())
                            .filter_map(|descendant| descendant.text())
                            .collect::<String>()
                    })
                    .unwrap_or_default()
            };
            let attendees = parse_participants(&child_text("known_participants"));
            let owner = attendees
                .iter()
                .find(|participant| participant.name.contains("(note creator)"))
                .or(attendees.first())
                .map(|owner| Participant {
                    name: owner
                        .name
                        .replace("(note creator)", "")
                        .split(" from ")
                        .next()
                        .unwrap_or_default()
                        .trim()
                        .to_owned(),
                    email: owner.email.clone(),
                });
            Some(Meeting {
                id: attribute("id")?,
                title: attribute("title")?,
                date: attribute("date")?,
                owner,
                attendees,
                summary_markdown: child_text("summary").trim().to_owned(),
            })
        })
        .collect();
    Ok((meetings, reported))
}

/// Parses `Name <email>` pairs from Granola's participant text.
fn parse_participants(text: &str) -> Vec<Participant> {
    let mut participants = Vec::new();
    let mut rest = text;
    while let Some(open) = rest.find('<') {
        let Some(close) = rest[open..].find('>').map(|close| open + close) else {
            break;
        };
        let name = rest[..open].rsplit(',').next().unwrap_or_default().trim();
        let email = rest[open + 1..close].trim().to_lowercase();
        if !name.is_empty() && !email.is_empty() {
            participants.push(Participant {
                name: name.to_owned(),
                email,
            });
        }
        rest = &rest[close + 1..];
    }
    participants
}

/// Parses dates like `Jul 8, 2026 5:30 PM GMT+2` or `Aug 5, 2026 5:00 PM CST`.
pub fn parse_meeting_date(value: &str) -> Option<DateTime<FixedOffset>> {
    let (local, zone) = value.trim().rsplit_once(' ')?;
    let offset = zone_offset(zone)?;
    let local = NaiveDateTime::parse_from_str(local, "%b %d, %Y %I:%M %p").ok()?;
    offset.from_local_datetime(&local).single()
}

fn zone_offset(zone: &str) -> Option<FixedOffset> {
    let zone = zone.to_ascii_uppercase();
    let minutes = match zone.as_str() {
        "UTC" | "GMT" | "UT" | "Z" => 0,
        "EST" => -5 * 60,
        "EDT" => -4 * 60,
        "CST" => -6 * 60,
        "CDT" => -5 * 60,
        "MST" => -7 * 60,
        "MDT" => -6 * 60,
        "PST" => -8 * 60,
        "PDT" => -7 * 60,
        "AKST" => -9 * 60,
        "AKDT" => -8 * 60,
        "HST" => -10 * 60,
        "BST" | "CET" => 60,
        "CEST" | "EET" => 2 * 60,
        "EEST" => 3 * 60,
        "IST" => 5 * 60 + 30,
        "JST" => 9 * 60,
        "AEST" => 10 * 60,
        "AEDT" => 11 * 60,
        _ => {
            let offset = zone
                .strip_prefix("GMT")
                .or_else(|| zone.strip_prefix("UTC"))
                .unwrap_or(&zone);
            numeric_offset_minutes(offset)?
        }
    };
    FixedOffset::east_opt(minutes * 60)
}

/// Parses `+2`, `-05`, `+5:30`, or `+0530` as minutes east of UTC.
fn numeric_offset_minutes(offset: &str) -> Option<i32> {
    let (sign, digits) = match offset.as_bytes().first()? {
        b'+' => (1, &offset[1..]),
        b'-' => (-1, &offset[1..]),
        _ => return None,
    };
    let (hours, minutes) = match digits.split_once(':') {
        Some(parts) => parts,
        None if digits.len() == 4 => digits.split_at(2),
        None => (digits, "0"),
    };
    let hours: i32 = hours.parse().ok()?;
    let minutes: i32 = minutes.parse().ok()?;
    (hours <= 23 && minutes < 60).then_some(sign * (hours * 60 + minutes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::errors::is_rejected;

    const MEETING: &str = r#"
        <meetings_data count="1">
          <meeting id="meeting-1" title="Planning" date="Jul 8, 2026 5:30 PM GMT+2">
            <known_participants>Ada (note creator) from Acme &lt;Ada@Example.com&gt;
            Bob &lt;bob@example.com&gt;, Cy &lt;cy@example.com&gt;</known_participants>
            <summary> Ship the Granola sync. </summary>
          </meeting>
        </meetings_data>
    "#;

    #[test]
    fn parses_meetings_participants_and_owner() {
        let (meetings, reported) = parse_meetings(MEETING).unwrap();
        assert_eq!(reported, 1);
        let meeting = &meetings[0];
        assert_eq!(meeting.id, "meeting-1");
        assert_eq!(meeting.title, "Planning");
        assert_eq!(meeting.summary_markdown, "Ship the Granola sync.");
        assert_eq!(
            meeting
                .attendees
                .iter()
                .map(|attendee| attendee.email.as_str())
                .collect::<Vec<_>>(),
            ["ada@example.com", "bob@example.com", "cy@example.com"]
        );
        assert_eq!(
            meeting.owner,
            Some(Participant {
                name: "Ada".to_owned(),
                email: "ada@example.com".to_owned(),
            })
        );
        assert_eq!(
            meeting.occurred_at().unwrap().to_rfc3339(),
            "2026-07-08T17:30:00+02:00"
        );
    }

    #[test]
    fn skips_incomplete_meetings_and_rejects_malformed_xml() {
        let (meetings, reported) =
            parse_meetings(r#"<meetings_data count="1"><meeting></meeting></meetings_data>"#)
                .unwrap();
        assert!(meetings.is_empty());
        assert_eq!(reported, 1);
        assert!(parse_meetings("").unwrap().0.is_empty());
        assert!(is_rejected(&parse_meetings("<meeting").unwrap_err()));
    }

    #[test]
    fn parses_meeting_dates_with_named_and_numeric_zones() {
        let parse = |value| parse_meeting_date(value).map(|date| date.to_rfc3339());
        assert_eq!(
            parse("Aug 5, 2026 5:00 PM CST").as_deref(),
            Some("2026-08-05T17:00:00-06:00")
        );
        assert_eq!(
            parse("Jan 12, 2026 9:05 AM GMT").as_deref(),
            Some("2026-01-12T09:05:00+00:00")
        );
        assert_eq!(
            parse("Jan 12, 2026 9:05 AM GMT+5:30").as_deref(),
            Some("2026-01-12T09:05:00+05:30")
        );
        assert_eq!(
            parse("Jan 12, 2026 9:05 PM UTC-0800").as_deref(),
            Some("2026-01-12T21:05:00-08:00")
        );
        assert_eq!(parse("Jan 12, 2026 9:05 PM Mars"), None);
        assert_eq!(parse("tomorrow"), None);
    }

    /// Serves a minimal Granola MCP endpoint that requires the session ID it issues.
    async fn fake_mcp(
        headers: axum::http::HeaderMap,
        axum::Json(body): axum::Json<Value>,
    ) -> axum::response::Response {
        use axum::{http::StatusCode, response::IntoResponse};

        assert_eq!(headers["authorization"], "Bearer token-1");
        let method = body["method"].as_str().unwrap();
        if method == "initialize" {
            let message = json!({ "jsonrpc": "2.0", "id": body["id"], "result": {} });
            return (
                [
                    ("content-type", "text/event-stream"),
                    ("mcp-session-id", "session-1"),
                ],
                format!("event: message\ndata: {message}\n\n"),
            )
                .into_response();
        }
        assert_eq!(headers["mcp-session-id"], "session-1");
        if method == "notifications/initialized" {
            return StatusCode::ACCEPTED.into_response();
        }
        let params = &body["params"];
        let result = match params["name"].as_str().unwrap() {
            "get_account_info" => json!({
                "content": [{ "type": "text", "text": "{\"email\":\" Owner@Example.com \"}" }]
            }),
            "get_meetings" if params["arguments"]["meeting_ids"][0] == "too-many" => json!({
                "isError": true,
                "content": [{ "type": "text", "text": "Invalid meeting_ids:\n maximum 10" }]
            }),
            "list_meetings" => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
            _ => return StatusCode::UNAUTHORIZED.into_response(),
        };
        axum::Json(json!({ "jsonrpc": "2.0", "id": body["id"], "result": result })).into_response()
    }

    #[tokio::test]
    async fn mcp_session_calls_tools_and_classifies_failures() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                axum::Router::new().route("/mcp", axum::routing::post(fake_mcp)),
            )
            .await
        });
        let client = GranolaClient {
            http: Client::new(),
            mcp_url: format!("http://{address}/mcp"),
        };
        let credential = GranolaCredential {
            id: 1,
            access_token: "token-1".to_owned(),
            provider_email: String::new(),
            provider_subject: String::new(),
        };
        let mut session = client.session(&credential).await.unwrap();

        assert_eq!(session.account_email().await.unwrap(), "owner@example.com");
        let tool_error = session
            .get_meetings(&["too-many".to_owned()])
            .await
            .unwrap_err();
        assert!(is_rejected(&tool_error));
        assert_eq!(
            tool_error.to_string(),
            "Granola MCP tool get_meetings failed: Invalid meeting_ids: maximum 10"
        );
        let date = NaiveDate::from_ymd_opt(2026, 7, 8).unwrap();
        let unavailable = session.list_meetings(date, date).await.unwrap_err();
        assert!(!is_rejected(&unavailable), "server errors are retried");
        let unauthorized = session
            .get_meetings(&["meeting-1".to_owned()])
            .await
            .unwrap_err();
        assert!(is_rejected(&unauthorized), "client errors are not retried");
        server.abort();
    }

    #[test]
    fn decodes_the_last_event_stream_message() {
        let body = "event: message\ndata: {\"id\":1}\n\ndata: {\"id\":2}\n";
        assert_eq!(decode_mcp_body(body, true).unwrap()["id"], 2);
        assert!(is_rejected(&decode_mcp_body("", true).unwrap_err()));
        assert!(is_rejected(&decode_mcp_body("nope", false).unwrap_err()));
    }
}
