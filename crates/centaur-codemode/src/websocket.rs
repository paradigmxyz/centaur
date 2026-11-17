//! WebSockets for connectors (`ctx.websocket`).
//!
//! A socket belongs to one connector call: the call's host functions are the
//! only way to reach it, and the host closes every socket the call opened when
//! the call ends, however it ends. Nothing outlives a call, so a cell never
//! holds a socket and state that must last longer (a remote browser, say)
//! lives upstream.
//!
//! The handshake is an HTTP/1.1 upgrade sent through the same `reqwest`
//! client settings as `fetch` (proxy, trust roots), so the principal's
//! iron-proxy sees and can inject into it like any other request.

use std::{
    collections::{BTreeMap, HashMap},
    sync::{
        Mutex,
        atomic::{AtomicU32, Ordering},
    },
    time::{Duration, Instant},
};

use futures_util::{
    SinkExt, StreamExt,
    stream::{SplitSink, SplitStream},
};
use serde::Serialize;
use tokio_tungstenite::{
    WebSocketStream,
    tungstenite::{
        Message,
        handshake::{client::generate_key, derive_accept_key},
        protocol::{CloseFrame, Role, WebSocketConfig, frame::coding::CloseCode},
    },
};

/// Sockets one call may open over its lifetime.
pub const MAX_SOCKETS_PER_CALL: usize = 4;
/// One message either way, the same bound as a `fetch` body.
pub const MAX_MESSAGE_BYTES: usize = 10 * 1024 * 1024;
/// Time for the upgrade response.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);
/// Time to say goodbye when the host closes a socket.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(2);
/// The start of a refused upgrade's body kept for the error message.
const MAX_REFUSAL_BODY_BYTES: usize = 2048;
/// Headers the handshake sets itself; a connector may not override them.
const RESERVED_HEADERS: &[&str] = &[
    "connection",
    "upgrade",
    "host",
    "content-length",
    "transfer-encoding",
    "sec-websocket-key",
    "sec-websocket-version",
    "sec-websocket-accept",
    "sec-websocket-extensions",
    "sec-websocket-protocol",
];

type Stream = WebSocketStream<reqwest::Upgraded>;

/// What a socket carried, recorded on its connect request.
#[derive(Clone, Debug, Default, Serialize)]
pub struct WebSocketStats {
    pub messages_sent: u64,
    pub messages_received: u64,
    pub bytes_sent: u64,
    pub bytes_received: u64,
    /// How it ended: `closed by peer (1000)`, `closed by connector`, ...
    #[serde(skip_serializing_if = "Option::is_none")]
    pub close: Option<String>,
}

/// One message read from a socket.
pub enum Incoming {
    Text(String),
    Binary(Vec<u8>),
    /// Nothing arrived in time; the socket is still open.
    Timeout,
}

struct Socket {
    sink: tokio::sync::Mutex<SplitSink<Stream, Message>>,
    stream: tokio::sync::Mutex<SplitStream<Stream>>,
    /// Index of the connect request in the call's record.
    record: usize,
    opened: Instant,
    stats: Mutex<WebSocketStats>,
}

/// The sockets of one connector call.
#[derive(Default)]
pub struct Sockets {
    next: AtomicU32,
    opened: AtomicU32,
    open: Mutex<HashMap<u32, std::sync::Arc<Socket>>>,
}

/// A socket the host closed or that ended: its record index, stats and age.
pub struct Closed {
    pub record: usize,
    pub stats: WebSocketStats,
    pub ms: u64,
}

impl Sockets {
    /// Reserve a slot for a new socket, or refuse once the call has opened
    /// `MAX_SOCKETS_PER_CALL` (open or since closed).
    pub fn reserve(&self) -> Result<(), String> {
        let opened = self.opened.fetch_add(1, Ordering::SeqCst) as usize;
        if opened >= MAX_SOCKETS_PER_CALL {
            self.opened.fetch_sub(1, Ordering::SeqCst);
            return Err(format!(
                "a connector call may open at most {MAX_SOCKETS_PER_CALL} websockets"
            ));
        }
        Ok(())
    }

    /// Give back a reserved slot whose handshake failed: the limit counts
    /// sockets, not attempts.
    pub fn release(&self) {
        self.opened.fetch_sub(1, Ordering::SeqCst);
    }

    /// Register a connected socket and return its handle.
    pub fn insert(&self, stream: Stream, record: usize) -> u32 {
        let (sink, stream) = stream.split();
        let id = self.next.fetch_add(1, Ordering::SeqCst) + 1;
        let socket = Socket {
            sink: tokio::sync::Mutex::new(sink),
            stream: tokio::sync::Mutex::new(stream),
            record,
            opened: Instant::now(),
            stats: Mutex::new(WebSocketStats::default()),
        };
        self.open
            .lock()
            .expect("sockets lock")
            .insert(id, std::sync::Arc::new(socket));
        id
    }

    fn get(&self, id: u32) -> Result<std::sync::Arc<Socket>, String> {
        self.open
            .lock()
            .expect("sockets lock")
            .get(&id)
            .cloned()
            .ok_or_else(|| format!("websocket {id} is not open"))
    }

    pub async fn send(&self, id: u32, message: Outgoing) -> Result<(), String> {
        let socket = self.get(id)?;
        let (message, bytes) = match message {
            Outgoing::Text(text) => {
                let bytes = text.len();
                (Message::text(text), bytes)
            }
            Outgoing::Binary(data) => {
                let bytes = data.len();
                (Message::binary(data), bytes)
            }
        };
        if bytes > MAX_MESSAGE_BYTES {
            return Err(format!(
                "websocket message is over the {MAX_MESSAGE_BYTES}-byte limit"
            ));
        }
        socket
            .sink
            .lock()
            .await
            .send(message)
            .await
            .map_err(|error| format!("websocket send failed: {error}"))?;
        let mut stats = socket.stats.lock().expect("socket stats lock");
        stats.messages_sent += 1;
        stats.bytes_sent += bytes as u64;
        Ok(())
    }

    /// The next text or binary message, `Timeout` when none arrives within
    /// `timeout`, or an error once the socket has closed. A closed socket is
    /// removed; its record is in the returned error's `Closed`.
    pub async fn recv(&self, id: u32, timeout: Duration) -> Result<Incoming, RecvError> {
        let socket = self.get(id).map_err(RecvError::Failed)?;
        let deadline = tokio::time::Instant::now() + timeout;
        let mut stream = socket.stream.lock().await;
        loop {
            let next = match tokio::time::timeout_at(deadline, stream.next()).await {
                Ok(next) => next,
                Err(_) => return Ok(Incoming::Timeout),
            };
            let (incoming, bytes) = match next {
                Some(Ok(Message::Text(text))) => {
                    let bytes = text.len();
                    (Incoming::Text(text.as_str().to_owned()), bytes)
                }
                Some(Ok(Message::Binary(data))) => {
                    let bytes = data.len();
                    (Incoming::Binary(data.to_vec()), bytes)
                }
                // Pings are answered by the protocol layer.
                Some(Ok(Message::Ping(_) | Message::Pong(_) | Message::Frame(_))) => continue,
                Some(Ok(Message::Close(frame))) => {
                    let reason = match frame {
                        Some(frame) => format!("closed by peer ({})", u16::from(frame.code)),
                        None => "closed by peer".to_owned(),
                    };
                    drop(stream);
                    return Err(RecvError::Closed(self.finish(id, reason)));
                }
                Some(Err(error)) => {
                    drop(stream);
                    let reason = format!("failed: {error}");
                    return Err(RecvError::Closed(self.finish(id, reason)));
                }
                None => {
                    drop(stream);
                    let reason = "connection ended".to_owned();
                    return Err(RecvError::Closed(self.finish(id, reason)));
                }
            };
            let mut stats = socket.stats.lock().expect("socket stats lock");
            stats.messages_received += 1;
            stats.bytes_received += bytes as u64;
            return Ok(incoming);
        }
    }

    /// Close one socket from the connector side.
    pub async fn close(&self, id: u32, code: Option<u16>, reason: String) -> Option<Closed> {
        let socket = self.get(id).ok()?;
        let frame = CloseFrame {
            code: code.map_or(CloseCode::Normal, CloseCode::from),
            reason: truncate_reason(reason).into(),
        };
        say_goodbye(&socket, Some(frame)).await;
        Some(self.finish(id, "closed by connector".to_owned()))
    }

    /// Close every socket still open; the host calls this when the call ends.
    pub async fn close_all(&self) -> Vec<Closed> {
        let ids: Vec<u32> = self
            .open
            .lock()
            .expect("sockets lock")
            .keys()
            .copied()
            .collect();
        let mut closed = Vec::with_capacity(ids.len());
        for id in ids {
            if let Ok(socket) = self.get(id) {
                say_goodbye(&socket, None).await;
            }
            closed.push(self.finish(id, "closed when the call ended".to_owned()));
        }
        closed
    }

    /// Remove a socket and report what it carried. Idempotent in effect: a
    /// second finish for the same id reports nothing new.
    fn finish(&self, id: u32, reason: String) -> Closed {
        let socket = self.open.lock().expect("sockets lock").remove(&id);
        match socket {
            Some(socket) => {
                let mut stats = socket.stats.lock().expect("socket stats lock").clone();
                stats.close = Some(reason);
                Closed {
                    record: socket.record,
                    stats,
                    ms: socket.opened.elapsed().as_millis() as u64,
                }
            }
            None => Closed {
                record: usize::MAX,
                stats: WebSocketStats::default(),
                ms: 0,
            },
        }
    }
}

/// Why `recv` returned no message.
pub enum RecvError {
    /// The socket closed or broke; it is no longer open.
    Closed(Closed),
    /// Bad handle.
    Failed(String),
}

/// One message to send.
pub enum Outgoing {
    Text(String),
    Binary(Vec<u8>),
}

/// Best effort: send a close frame and flush, bounded by `CLOSE_TIMEOUT`.
async fn say_goodbye(socket: &Socket, frame: Option<CloseFrame>) {
    let _ = tokio::time::timeout(CLOSE_TIMEOUT, async {
        let mut sink = socket.sink.lock().await;
        let _ = sink.send(Message::Close(frame)).await;
        let _ = sink.close().await;
    })
    .await;
}

/// Close reasons are at most 123 bytes on the wire.
fn truncate_reason(mut reason: String) -> String {
    if reason.len() > 123 {
        let mut end = 123;
        while !reason.is_char_boundary(end) {
            end -= 1;
        }
        reason.truncate(end);
    }
    reason
}

/// A connected socket and the subprotocol the server chose.
pub struct Connected {
    pub stream: Stream,
    pub protocol: Option<String>,
}

/// Open a WebSocket: an HTTP/1.1 upgrade through `http`, which must be an
/// HTTP/1-only client so the upgrade is not negotiated away.
pub async fn connect(
    http: &reqwest::Client,
    url: &str,
    headers: &BTreeMap<String, String>,
    protocols: &[String],
    timeout: Option<Duration>,
) -> Result<Connected, String> {
    let mut parsed = reqwest::Url::parse(url).map_err(|error| format!("invalid URL: {error}"))?;
    let scheme = match parsed.scheme() {
        "ws" => "http",
        "wss" => "https",
        other => return Err(format!("unsupported websocket URL scheme {other:?}")),
    };
    parsed
        .set_scheme(scheme)
        .map_err(|()| "invalid websocket URL".to_owned())?;
    for name in headers.keys() {
        if RESERVED_HEADERS.contains(&name.to_ascii_lowercase().as_str()) {
            return Err(format!(
                "the {name} header is set by the websocket handshake"
            ));
        }
    }
    for protocol in protocols {
        if protocol.is_empty() || !protocol.bytes().all(|b| b.is_ascii_graphic() && b != b',') {
            return Err(format!("invalid websocket subprotocol {protocol:?}"));
        }
    }

    let key = generate_key();
    let mut request = http
        .get(parsed)
        .header("connection", "Upgrade")
        .header("upgrade", "websocket")
        .header("sec-websocket-version", "13")
        .header("sec-websocket-key", &key);
    if !protocols.is_empty() {
        request = request.header("sec-websocket-protocol", protocols.join(", "));
    }
    for (name, value) in headers {
        request = request.header(name, value);
    }
    let wait = timeout.unwrap_or(HANDSHAKE_TIMEOUT).min(HANDSHAKE_TIMEOUT);
    let mut response = tokio::time::timeout(wait, request.send())
        .await
        .map_err(|_| format!("websocket handshake timed out after {}s", wait.as_secs()))?
        .map_err(crate::engine::transport)?;

    let status = response.status();
    if status != reqwest::StatusCode::SWITCHING_PROTOCOLS {
        let mut body = Vec::new();
        while let Ok(Ok(Some(chunk))) =
            tokio::time::timeout(Duration::from_secs(2), response.chunk()).await
        {
            body.extend_from_slice(&chunk);
            if body.len() >= MAX_REFUSAL_BODY_BYTES {
                body.truncate(MAX_REFUSAL_BODY_BYTES);
                break;
            }
        }
        let body = String::from_utf8_lossy(&body);
        return Err(format!(
            "websocket handshake refused: HTTP {}{}",
            status.as_u16(),
            if body.trim().is_empty() {
                String::new()
            } else {
                format!(": {}", body.trim())
            }
        ));
    }
    let header = |name: &str| {
        response
            .headers()
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned)
    };
    if header("sec-websocket-accept").as_deref() != Some(derive_accept_key(key.as_bytes()).as_str())
    {
        return Err("websocket handshake failed: bad Sec-WebSocket-Accept".to_owned());
    }
    let protocol = header("sec-websocket-protocol");
    if let Some(chosen) = &protocol
        && !protocols.iter().any(|offered| offered == chosen)
    {
        return Err(format!(
            "websocket handshake failed: server chose subprotocol {chosen:?}, which was not offered"
        ));
    }
    let upgraded = response
        .upgrade()
        .await
        .map_err(|error| format!("websocket upgrade failed: {}", error.without_url()))?;
    let config = WebSocketConfig::default()
        .max_message_size(Some(MAX_MESSAGE_BYTES))
        .max_frame_size(Some(MAX_MESSAGE_BYTES));
    let stream = WebSocketStream::from_raw_socket(upgraded, Role::Client, Some(config)).await;
    Ok(Connected { stream, protocol })
}
