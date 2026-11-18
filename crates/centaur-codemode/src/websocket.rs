//! WebSockets for connectors (`ctx.websocket`): text messages only, which is
//! all the Chrome DevTools Protocol needs.
//!
//! A socket belongs to one connector call: the call's host functions are the
//! only way to reach it, and the host closes every socket the call opened when
//! the call ends, however it ends. Nothing outlives a call, so a cell never
//! holds a socket and state that must last longer (a remote browser, say)
//! lives upstream.
//!
//! The handshake is an HTTP/1.1 upgrade sent through the same `reqwest`
//! client settings as `fetch` (proxy, trust roots), so the principal's
//! iron-proxy sees it like any other request.

use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU32, Ordering},
    },
    time::Duration,
};

use futures_util::{
    SinkExt, StreamExt,
    stream::{SplitSink, SplitStream},
};
use tokio_tungstenite::{
    WebSocketStream,
    tungstenite::{
        Message,
        handshake::{client::generate_key, derive_accept_key},
        protocol::{Role, WebSocketConfig},
    },
};

/// Connection attempts one call may make.
pub const MAX_SOCKETS_PER_CALL: usize = 4;
/// One message either way, the same bound as a `fetch` body.
pub const MAX_MESSAGE_BYTES: usize = 10 * 1024 * 1024;
/// Time for the upgrade response.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);
/// Time to say goodbye when a socket is closed.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(2);

type Stream = WebSocketStream<reqwest::Upgraded>;

/// Split so a pending `recv` does not block a `send`.
struct Socket {
    sink: tokio::sync::Mutex<SplitSink<Stream, Message>>,
    stream: tokio::sync::Mutex<SplitStream<Stream>>,
}

/// The sockets of one connector call.
#[derive(Default)]
pub struct Sockets {
    attempts: AtomicU32,
    next: AtomicU32,
    open: Mutex<HashMap<u32, Arc<Socket>>>,
}

impl Sockets {
    /// Count a connection attempt, refusing past `MAX_SOCKETS_PER_CALL`.
    pub fn attempt(&self) -> Result<(), String> {
        if self.attempts.fetch_add(1, Ordering::SeqCst) as usize >= MAX_SOCKETS_PER_CALL {
            return Err(format!(
                "a connector call may open at most {MAX_SOCKETS_PER_CALL} websockets"
            ));
        }
        Ok(())
    }

    /// Register a connected socket and return its handle.
    pub fn insert(&self, stream: Stream) -> u32 {
        let (sink, stream) = stream.split();
        let id = self.next.fetch_add(1, Ordering::SeqCst) + 1;
        let socket = Socket {
            sink: tokio::sync::Mutex::new(sink),
            stream: tokio::sync::Mutex::new(stream),
        };
        self.open
            .lock()
            .expect("sockets lock")
            .insert(id, Arc::new(socket));
        id
    }

    fn get(&self, id: u32) -> Result<Arc<Socket>, String> {
        self.open
            .lock()
            .expect("sockets lock")
            .get(&id)
            .cloned()
            .ok_or_else(|| format!("websocket {id} is not open"))
    }

    fn remove(&self, id: u32) -> Option<Arc<Socket>> {
        self.open.lock().expect("sockets lock").remove(&id)
    }

    pub async fn send(&self, id: u32, text: String) -> Result<(), String> {
        if text.len() > MAX_MESSAGE_BYTES {
            return Err(format!(
                "websocket message is over the {MAX_MESSAGE_BYTES}-byte limit"
            ));
        }
        let socket = self.get(id)?;
        let mut sink = socket.sink.lock().await;
        sink.send(Message::text(text))
            .await
            .map_err(|error| format!("websocket send failed: {error}"))
    }

    /// The next message, or `None` when none arrives within `timeout`. Once
    /// the socket closes or breaks it is removed, and this returns an error.
    pub async fn recv(&self, id: u32, timeout: Duration) -> Result<Option<String>, String> {
        let socket = self.get(id)?;
        let deadline = tokio::time::Instant::now() + timeout;
        let mut stream = socket.stream.lock().await;
        let ended = loop {
            let Ok(next) = tokio::time::timeout_at(deadline, stream.next()).await else {
                return Ok(None);
            };
            match next {
                Some(Ok(Message::Text(text))) => return Ok(Some(text.as_str().to_owned())),
                Some(Ok(Message::Binary(data))) => {
                    return Ok(Some(String::from_utf8_lossy(&data).into_owned()));
                }
                // Pings are answered by the protocol layer.
                Some(Ok(Message::Ping(_) | Message::Pong(_) | Message::Frame(_))) => {}
                Some(Ok(Message::Close(_))) | None => break "closed".to_owned(),
                Some(Err(error)) => break format!("failed: {error}"),
            }
        };
        drop(stream);
        self.remove(id);
        Err(format!("websocket {id} {ended}"))
    }

    /// Close one socket; closing an unknown or closed socket does nothing.
    pub async fn close(&self, id: u32) {
        if let Some(socket) = self.remove(id) {
            goodbye(&socket).await;
        }
    }

    /// Close every socket still open; the host calls this when the call ends.
    pub async fn close_all(&self) {
        let sockets: Vec<_> = self
            .open
            .lock()
            .expect("sockets lock")
            .drain()
            .map(|(_, socket)| socket)
            .collect();
        for socket in sockets {
            goodbye(&socket).await;
        }
    }
}

/// Best effort: send a close frame and flush, bounded by `CLOSE_TIMEOUT`.
async fn goodbye(socket: &Socket) {
    let _ = tokio::time::timeout(CLOSE_TIMEOUT, async {
        let mut sink = socket.sink.lock().await;
        let _ = sink.send(Message::Close(None)).await;
        let _ = sink.close().await;
    })
    .await;
}

/// Open a WebSocket: an HTTP/1.1 upgrade through `http`, which must be an
/// HTTP/1-only client so the upgrade is not negotiated away.
pub async fn connect(
    http: &reqwest::Client,
    url: &str,
    timeout: Duration,
) -> Result<Stream, String> {
    let mut parsed = reqwest::Url::parse(url).map_err(|error| format!("invalid URL: {error}"))?;
    let scheme = match parsed.scheme() {
        "ws" => "http",
        "wss" => "https",
        other => return Err(format!("unsupported websocket URL scheme {other:?}")),
    };
    parsed
        .set_scheme(scheme)
        .map_err(|()| "invalid websocket URL".to_owned())?;

    let key = generate_key();
    let request = http
        .get(parsed)
        .header("connection", "Upgrade")
        .header("upgrade", "websocket")
        .header("sec-websocket-version", "13")
        .header("sec-websocket-key", &key);
    let wait = timeout.min(HANDSHAKE_TIMEOUT);
    let response = tokio::time::timeout(wait, request.send())
        .await
        .map_err(|_| format!("websocket handshake timed out after {}s", wait.as_secs()))?
        .map_err(crate::engine::transport)?;
    if response.status() != reqwest::StatusCode::SWITCHING_PROTOCOLS {
        return Err(format!(
            "websocket handshake refused: HTTP {}",
            response.status().as_u16()
        ));
    }
    let accept = response
        .headers()
        .get("sec-websocket-accept")
        .and_then(|value| value.to_str().ok());
    if accept != Some(derive_accept_key(key.as_bytes()).as_str()) {
        return Err("websocket handshake failed: bad Sec-WebSocket-Accept".to_owned());
    }
    let upgraded = response
        .upgrade()
        .await
        .map_err(|error| format!("websocket upgrade failed: {}", error.without_url()))?;
    let config = WebSocketConfig::default()
        .max_message_size(Some(MAX_MESSAGE_BYTES))
        .max_frame_size(Some(MAX_MESSAGE_BYTES));
    Ok(WebSocketStream::from_raw_socket(upgraded, Role::Client, Some(config)).await)
}
