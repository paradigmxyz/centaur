use std::{
    pin::Pin,
    task::{Context, Poll},
    time::SystemTime,
};

use async_trait::async_trait;
use centaur_sandbox_core::{
    ObservedSandbox, SandboxBackend, SandboxError, SandboxHandle, SandboxId, SandboxIo,
    SandboxResult, SandboxSpec, SandboxStatus,
};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    sync::mpsc,
};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, tungstenite::Message};

use crate::BRAIN_SANDBOX_ID_PREFIX;

const BRAIN_BACKEND_NAME: &str = "brain";
const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
const MAX_ERROR_BODY_CHARS: usize = 512;

type BrainSocket = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

/// Parsed brain endpoint: the HTTP base for control calls and the WebSocket
/// base derived from it for the I/O attach.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BrainSandboxConfig {
    http_base: String,
    ws_base: String,
}

impl BrainSandboxConfig {
    /// Parse a brain base URL such as `http://centaur-brain:8790`.
    pub fn parse(raw: &str) -> SandboxResult<Self> {
        let trimmed = raw.trim().trim_end_matches('/');
        if trimmed.is_empty() {
            return Err(SandboxError::InvalidSpec(
                "brain URL must not be empty".to_owned(),
            ));
        }
        let (http_base, ws_base) = if let Some(rest) = trimmed.strip_prefix("http://") {
            (trimmed.to_owned(), format!("ws://{rest}"))
        } else if let Some(rest) = trimmed.strip_prefix("https://") {
            (trimmed.to_owned(), format!("wss://{rest}"))
        } else if let Some(rest) = trimmed.strip_prefix("ws://") {
            (format!("http://{rest}"), trimmed.to_owned())
        } else if let Some(rest) = trimmed.strip_prefix("wss://") {
            (format!("https://{rest}"), trimmed.to_owned())
        } else {
            return Err(SandboxError::InvalidSpec(format!(
                "brain URL must use http(s):// or ws(s)://: {raw:?}"
            )));
        };
        Ok(Self { http_base, ws_base })
    }

    pub fn http_base(&self) -> &str {
        &self.http_base
    }

    pub fn ws_base(&self) -> &str {
        &self.ws_base
    }

    fn sandbox_url(&self, id: &str) -> String {
        format!("{}/v1/sandboxes/{id}", self.http_base)
    }

    fn io_url(&self, id: &str) -> String {
        format!("{}/v1/sandboxes/{id}/io", self.ws_base)
    }
}

/// [`SandboxBackend`] whose sandboxes are logical handles on the long-lived
/// `centaur-brain` service.
pub struct BrainSandboxBackend {
    client: reqwest::Client,
    config: BrainSandboxConfig,
}

impl BrainSandboxBackend {
    pub fn new(url: impl AsRef<str>) -> SandboxResult<Self> {
        let config = BrainSandboxConfig::parse(url.as_ref())?;
        Self::with_config(config)
    }

    pub fn with_config(config: BrainSandboxConfig) -> SandboxResult<Self> {
        let client = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|error| {
                SandboxError::backend_source("failed to build brain HTTP client", error)
            })?;
        Ok(Self { client, config })
    }

    pub fn config(&self) -> &BrainSandboxConfig {
        &self.config
    }

    async fn create_sandbox(&self, spec: SandboxSpec) -> SandboxResult<SandboxHandle> {
        let id = format!("brain-{}", uuid::Uuid::new_v4().simple());
        let thread_key = spec
            .env
            .iter()
            .find(|env| env.name == "CENTAUR_THREAD_KEY")
            .map(|env| env.value.clone());
        let request = CreateSandboxRequest {
            id: id.clone(),
            thread_key,
            labels: spec.labels,
        };
        let url = format!("{}/v1/sandboxes", self.config.http_base);
        let response = self
            .client
            .post(&url)
            .json(&request)
            .send()
            .await
            .map_err(|error| SandboxError::backend_source("brain create request failed", error))?;
        let response = ensure_success(response, "brain create").await?;
        // The brain returns `{"id": "..."}`. Ignore the body (the id we sent is
        // authoritative) but consume it so the connection is reused.
        let _ = response.text().await;
        Ok(SandboxHandle::new(id, BRAIN_BACKEND_NAME))
    }

    async fn sandbox_status(&self, id: &str) -> SandboxResult<SandboxStatus> {
        let response = self
            .client
            .get(self.config.sandbox_url(id))
            .send()
            .await
            .map_err(|error| SandboxError::backend_source("brain status request failed", error))?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(SandboxStatus::Gone);
        }
        let response = ensure_success(response, "brain status").await?;
        let body = response.json::<StatusResponse>().await.map_err(|error| {
            SandboxError::backend_source("brain status response was invalid", error)
        })?;
        Ok(status_from_str(&body.status))
    }

    async fn stop_sandbox(&self, id: &str) -> SandboxResult<()> {
        let response = self
            .client
            .delete(self.config.sandbox_url(id))
            .send()
            .await
            .map_err(|error| SandboxError::backend_source("brain stop request failed", error))?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(());
        }
        ensure_success(response, "brain stop").await?;
        Ok(())
    }

    async fn open_brain_io(&self, id: &str) -> SandboxResult<SandboxIo> {
        let url = self.config.io_url(id);
        let (socket, _response) = tokio_tungstenite::connect_async(url.as_str())
            .await
            .map_err(|error| {
                SandboxError::io_source(format!("brain io attach to {url} failed"), error)
            })?;

        let (stdin_sender, stdin_receiver) = mpsc::unbounded_channel();
        let (stdout_sender, stdout_receiver) = mpsc::unbounded_channel();
        let connection = tokio::spawn(run_connection(socket, stdin_receiver, stdout_sender));

        Ok(SandboxIo::with_guard(
            Box::pin(BrainStdin {
                sender: Some(stdin_sender),
                pending: Vec::new(),
            }),
            Box::pin(BrainStdout {
                receiver: stdout_receiver,
                pending: Vec::new(),
                offset: 0,
            }),
            Box::pin(tokio::io::empty()),
            BrainIoGuard { connection },
        ))
    }
}

#[async_trait]
impl SandboxBackend for BrainSandboxBackend {
    fn name(&self) -> &'static str {
        BRAIN_BACKEND_NAME
    }

    async fn create(&self, spec: SandboxSpec) -> SandboxResult<SandboxHandle> {
        self.create_sandbox(spec).await
    }

    async fn open_io(&self, id: &SandboxId) -> SandboxResult<SandboxIo> {
        self.open_brain_io(id.as_str()).await
    }

    async fn read_output_since(
        &self,
        id: &SandboxId,
        since: Option<SystemTime>,
    ) -> SandboxResult<Vec<String>> {
        let mut url = format!("{}/output", self.config.sandbox_url(id.as_str()));
        if let Some(since) = since {
            let since_ms = since
                .duration_since(SystemTime::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_millis())
                .unwrap_or(0);
            url.push_str(&format!("?since_ms={since_ms}"));
        }
        let response =
            self.client.get(&url).send().await.map_err(|error| {
                SandboxError::backend_source("brain output request failed", error)
            })?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(SandboxError::NotFound(id.as_str().to_owned()));
        }
        let response = ensure_success(response, "brain output").await?;
        let body = response.json::<OutputResponse>().await.map_err(|error| {
            SandboxError::backend_source("brain output response was invalid", error)
        })?;
        Ok(body.lines)
    }

    async fn status(&self, id: &SandboxId) -> SandboxResult<SandboxStatus> {
        self.sandbox_status(id.as_str()).await
    }

    async fn observe(&self, id: &SandboxId) -> SandboxResult<ObservedSandbox> {
        let status = self.sandbox_status(id.as_str()).await?;
        Ok(ObservedSandbox::new(
            id.as_str(),
            BRAIN_BACKEND_NAME,
            status,
        ))
    }

    async fn list_observed(&self) -> SandboxResult<Vec<ObservedSandbox>> {
        // Brain sandboxes are cheap logical handles; the control plane's
        // reaper never enumerates them in the spike.
        Ok(Vec::new())
    }

    async fn stop(&self, id: &SandboxId) -> SandboxResult<()> {
        self.stop_sandbox(id.as_str()).await
    }

    async fn assign_iron_control_proxy_principal(
        &self,
        _id: &SandboxId,
        _principal_id: &str,
        _requester_principal_id: Option<&str>,
        _labels: &std::collections::BTreeMap<String, String>,
    ) -> SandboxResult<()> {
        Ok(())
    }

    async fn ensure_iron_control_proxy_resources(
        &self,
        _id: &SandboxId,
        _principal_id: &str,
        _requester_principal_id: Option<&str>,
        _labels: &std::collections::BTreeMap<String, String>,
    ) -> SandboxResult<()> {
        Ok(())
    }

    async fn enable_observability(&self, _id: &SandboxId) -> SandboxResult<()> {
        Ok(())
    }

    async fn pause(&self, _id: &SandboxId) -> SandboxResult<()> {
        Ok(())
    }

    async fn resume(&self, _id: &SandboxId) -> SandboxResult<()> {
        Ok(())
    }
}

/// Whether a sandbox id belongs to the brain backend.
pub fn is_brain_sandbox_id(id: &str) -> bool {
    id.starts_with(BRAIN_SANDBOX_ID_PREFIX)
}

#[derive(Serialize)]
struct CreateSandboxRequest {
    id: String,
    thread_key: Option<String>,
    labels: std::collections::BTreeMap<String, String>,
}

#[derive(Deserialize)]
struct StatusResponse {
    status: String,
}

#[derive(Deserialize)]
struct OutputResponse {
    lines: Vec<String>,
}

fn status_from_str(raw: &str) -> SandboxStatus {
    match raw.trim().to_ascii_lowercase().as_str() {
        "created" | "creating" | "pending" => SandboxStatus::Created,
        "running" | "ready" => SandboxStatus::Running,
        "suspended" | "paused" => SandboxStatus::Suspended,
        "stopped" => SandboxStatus::Stopped,
        "gone" | "deleted" => SandboxStatus::Gone,
        other => SandboxStatus::Unknown(other.to_owned()),
    }
}

async fn ensure_success(
    response: reqwest::Response,
    context: &str,
) -> SandboxResult<reqwest::Response> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }
    let body = response.text().await.unwrap_or_default();
    let mut body = body.trim().to_owned();
    if body.chars().count() > MAX_ERROR_BODY_CHARS {
        body = body.chars().take(MAX_ERROR_BODY_CHARS).collect();
    }
    Err(SandboxError::backend(format!(
        "{context} failed with status {status}: {body}"
    )))
}

/// Keeps the WebSocket connection task alive. Dropping it (when the session
/// pipe or stdout pump lets go of the sandbox) aborts the task and closes the
/// socket.
struct BrainIoGuard {
    connection: tokio::task::JoinHandle<()>,
}

impl Drop for BrainIoGuard {
    fn drop(&mut self) {
        self.connection.abort();
    }
}

/// Stdin half: line-delimited bytes in, one WebSocket Text frame per complete
/// line out (trailing newline stripped).
struct BrainStdin {
    sender: Option<mpsc::UnboundedSender<String>>,
    pending: Vec<u8>,
}

impl AsyncWrite for BrainStdin {
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let this = self.get_mut();
        this.pending.extend_from_slice(buf);
        while let Some(newline) = this.pending.iter().position(|byte| *byte == b'\n') {
            let mut line: Vec<u8> = this.pending.drain(..=newline).collect();
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            let line = match String::from_utf8(line) {
                Ok(line) => line,
                Err(error) => {
                    return Poll::Ready(Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        error,
                    )));
                }
            };
            let Some(sender) = this.sender.as_ref() else {
                return Poll::Ready(Err(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "brain stdin is closed",
                )));
            };
            if sender.send(line).is_err() {
                return Poll::Ready(Err(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "brain connection is closed",
                )));
            }
        }
        Poll::Ready(Ok(buf.len()))
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        // Complete lines are already queued; a partial trailing line waits for
        // the rest of its bytes, exactly like a byte stream.
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();
        this.sender = None;
        this.pending.clear();
        Poll::Ready(Ok(()))
    }
}

/// Stdout half: one WebSocket Text frame in, `<frame>\n` bytes out. The
/// channel closes (EOF) when the socket closes, errors, or the guard drops.
struct BrainStdout {
    receiver: mpsc::UnboundedReceiver<Vec<u8>>,
    pending: Vec<u8>,
    offset: usize,
}

impl AsyncRead for BrainStdout {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();
        if this.offset >= this.pending.len() {
            this.pending.clear();
            this.offset = 0;
            match Pin::new(&mut this.receiver).poll_recv(cx) {
                Poll::Ready(Some(bytes)) => this.pending = bytes,
                Poll::Ready(None) => return Poll::Ready(Ok(())),
                Poll::Pending => return Poll::Pending,
            }
        }
        let available = &this.pending[this.offset..];
        let count = available.len().min(buf.remaining());
        buf.put_slice(&available[..count]);
        this.offset += count;
        Poll::Ready(Ok(()))
    }
}

/// Owns the WebSocket and bridges it to the channel halves. Exits when the
/// socket closes, errors, or the stdin side is dropped; dropping the output
/// sender is what turns into stdout EOF.
async fn run_connection(
    mut socket: BrainSocket,
    mut requests: mpsc::UnboundedReceiver<String>,
    outputs: mpsc::UnboundedSender<Vec<u8>>,
) {
    loop {
        tokio::select! {
            message = socket.next() => {
                match message {
                    Some(Ok(Message::Text(text))) => {
                        let mut line = text.to_string().into_bytes();
                        line.push(b'\n');
                        if outputs.send(line).is_err() {
                            break;
                        }
                    }
                    Some(Ok(Message::Binary(bytes))) => {
                        let mut line = bytes.to_vec();
                        line.push(b'\n');
                        if outputs.send(line).is_err() {
                            break;
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(_)) => {}
                    Some(Err(_)) => break,
                }
            }
            line = requests.recv() => {
                match line {
                    Some(line) => {
                        if socket.send(Message::Text(line.into())).await.is_err() {
                            break;
                        }
                    }
                    None => {
                        let _ = socket.close(None).await;
                        break;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
