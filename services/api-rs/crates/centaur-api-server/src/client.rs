use std::pin::Pin;

use centaur_session_core::{
    Session, ThreadKey,
    development::{AcceptedDevelopmentTask, RepositorySelectionDraft, RepositorySelectionOutcome},
};
use eventsource_stream::Eventsource;
use futures_util::{Stream, StreamExt};
use reqwest::{Client as HttpClient, StatusCode};
use thiserror::Error;

use crate::types::{
    AcceptDevelopmentTaskRequest, AppendMessagesRequest, AppendMessagesResponse,
    ConfirmDevelopmentSelectionRequest, CreateAddRepositorySelectionRequest, CreateSessionRequest,
    DecideDevelopmentSelectionRequest, ExecuteSessionRequest, ExecuteSessionResponse,
};

#[derive(Clone, Debug)]
pub struct CentaurClient {
    client: HttpClient,
    base_url: String,
}

impl CentaurClient {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self::with_client(HttpClient::new(), base_url)
    }

    pub fn with_client(client: HttpClient, base_url: impl Into<String>) -> Self {
        Self {
            client,
            base_url: base_url.into().trim_end_matches('/').to_owned(),
        }
    }

    pub async fn create_session(
        &self,
        thread_key: &ThreadKey,
        request: CreateSessionRequest,
    ) -> Result<Session, ClientError> {
        self.post_json(&self.session_url(thread_key), &request)
            .await
    }

    pub async fn append_messages(
        &self,
        thread_key: &ThreadKey,
        request: AppendMessagesRequest,
    ) -> Result<AppendMessagesResponse, ClientError> {
        self.post_json(
            &format!("{}/messages", self.session_url(thread_key)),
            &request,
        )
        .await
    }

    pub async fn execute_session(
        &self,
        thread_key: &ThreadKey,
        request: ExecuteSessionRequest,
    ) -> Result<ExecuteSessionResponse, ClientError> {
        self.post_json(
            &format!("{}/execute", self.session_url(thread_key)),
            &request,
        )
        .await
    }

    pub async fn accept_development_task(
        &self,
        request: AcceptDevelopmentTaskRequest,
    ) -> Result<AcceptedDevelopmentTask, ClientError> {
        self.post_json(
            &format!("{}/api/development/tasks", self.base_url),
            &request,
        )
        .await
    }

    pub async fn confirm_development_selection(
        &self,
        selection_flow_id: &str,
        request: ConfirmDevelopmentSelectionRequest,
    ) -> Result<RepositorySelectionOutcome, ClientError> {
        self.post_json(
            &format!(
                "{}/api/development/selections/{}/confirm",
                self.base_url,
                urlencoding::encode(selection_flow_id)
            ),
            &request,
        )
        .await
    }

    pub async fn confirm_development_no_project(
        &self,
        selection_flow_id: &str,
        request: DecideDevelopmentSelectionRequest,
    ) -> Result<RepositorySelectionOutcome, ClientError> {
        self.post_json(
            &format!(
                "{}/api/development/selections/{}/no-project",
                self.base_url,
                urlencoding::encode(selection_flow_id)
            ),
            &request,
        )
        .await
    }

    pub async fn cancel_development_selection(
        &self,
        selection_flow_id: &str,
        request: DecideDevelopmentSelectionRequest,
    ) -> Result<RepositorySelectionOutcome, ClientError> {
        self.post_json(
            &format!(
                "{}/api/development/selections/{}/cancel",
                self.base_url,
                urlencoding::encode(selection_flow_id)
            ),
            &request,
        )
        .await
    }

    pub async fn create_add_repository_selection(
        &self,
        thread_key: &ThreadKey,
    ) -> Result<RepositorySelectionDraft, ClientError> {
        self.post_json(
            &format!(
                "{}/api/development/sessions/{}/repositories",
                self.base_url,
                urlencoding::encode(thread_key.as_str())
            ),
            &CreateAddRepositorySelectionRequest::default(),
        )
        .await
    }

    pub async fn stream_events(
        &self,
        thread_key: &ThreadKey,
        after_event_id: i64,
    ) -> Result<SseEventStream, ClientError> {
        let events_url = format!(
            "{}/events?after_event_id={after_event_id}",
            self.session_url(thread_key)
        );
        let response = self.client.get(&events_url).send().await?;
        let response = ensure_response_success(response).await?;
        let stream = response
            .bytes_stream()
            .eventsource()
            .map(|event| event.map_err(|error| ClientError::EventStream(error.to_string())));
        Ok(Box::pin(stream))
    }

    async fn post_json<T, R>(&self, url: &str, payload: &T) -> Result<R, ClientError>
    where
        T: serde::Serialize + ?Sized,
        R: serde::de::DeserializeOwned,
    {
        let response = self.client.post(url).json(payload).send().await?;
        let response = ensure_response_success(response).await?;
        Ok(response.json().await?)
    }

    fn session_url(&self, thread_key: &ThreadKey) -> String {
        format!(
            "{}/api/session/{}",
            self.base_url,
            urlencoding::encode(thread_key.as_str())
        )
    }
}

pub type SseEventStream = Pin<Box<dyn Stream<Item = Result<SseEvent, ClientError>> + Send>>;
pub type SseEvent = eventsource_stream::Event;

async fn ensure_response_success(
    response: reqwest::Response,
) -> Result<reqwest::Response, ClientError> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }
    let body = response.text().await?;
    Err(ClientError::Api { status, body })
}

#[derive(Debug, Error)]
pub enum ClientError {
    #[error(transparent)]
    Http(#[from] reqwest::Error),
    #[error("HTTP {status}: {body}")]
    Api { status: StatusCode, body: String },
    #[error("event stream parse failed: {0}")]
    EventStream(String),
}
