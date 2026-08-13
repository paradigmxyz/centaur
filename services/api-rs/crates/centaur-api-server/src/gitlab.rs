use std::{fs, path::PathBuf, time::Duration};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use centaur_session_core::development::{RepositoryId, ResolvedRepository};
use reqwest::{
    Client, StatusCode, Url,
    header::{HeaderName, HeaderValue},
    redirect::Policy,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::development::{
    RepositoryCatalog, RepositoryResolveError, RepositoryResolver, ResolveRepositoriesFuture,
    SearchRepositoriesFuture,
};

const PRIVATE_TOKEN: HeaderName = HeaderName::from_static("private-token");
const MAX_DESCRIPTION_CHARS: usize = 512;
const MAX_SEARCH_CHARS: usize = 200;
const MAX_PAGE_SIZE: u16 = 100;
const CURSOR_PREFIX: &str = "gitlab-page:";

#[derive(Clone, Debug)]
pub struct GitLabCatalogConfig {
    pub base_url: String,
    pub allow_insecure_http: bool,
    pub token_file: PathBuf,
    pub page_size: u16,
    pub request_timeout: Duration,
}

#[derive(Clone)]
pub struct GitLabCatalog {
    client: Client,
    base_url: Url,
    token_file: PathBuf,
    page_size: u16,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RepositorySummary {
    pub repository_id: RepositoryId,
    pub name: String,
    pub namespace: String,
    pub path_with_namespace: String,
    pub description: Option<String>,
    pub default_branch: Option<String>,
    pub archived: bool,
    pub last_activity_at: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RepositoryPage {
    pub repositories: Vec<RepositorySummary>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Error)]
pub enum GitLabCatalogBuildError {
    #[error("GitLab base URL is invalid")]
    InvalidBaseUrl,
    #[error("GitLab base URL must use HTTPS unless insecure HTTP is explicitly allowed")]
    InsecureBaseUrl,
    #[error("GitLab repository catalog page size must be between 1 and 100")]
    InvalidPageSize,
    #[error("GitLab repository catalog timeout must be greater than zero")]
    InvalidTimeout,
    #[error("GitLab repository catalog token file is unavailable")]
    TokenUnavailable,
    #[error("failed to build GitLab HTTP client")]
    HttpClient,
}

#[derive(Debug, Error)]
pub enum GitLabCatalogError {
    #[error("invalid repository catalog request: {0}")]
    Invalid(String),
    #[error("repository catalog is temporarily unavailable")]
    Unavailable,
}

#[derive(Debug, Deserialize)]
struct GitLabProject {
    id: u64,
    name: String,
    path_with_namespace: String,
    description: Option<String>,
    default_branch: Option<String>,
    archived: bool,
    http_url_to_repo: Option<String>,
    last_activity_at: Option<String>,
}

impl GitLabCatalog {
    fn unavailable(failure_code: &'static str, http_status: Option<u16>) -> GitLabCatalogError {
        tracing::warn!(
            event = "gitlab_repository_catalog_unavailable",
            failure_code,
            http_status,
            "GitLab repository catalog request failed"
        );
        GitLabCatalogError::Unavailable
    }

    fn upstream_failure_code(status: StatusCode) -> &'static str {
        if status.is_redirection() {
            "unexpected_redirect"
        } else if matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) {
            "authentication_rejected"
        } else {
            "upstream_status"
        }
    }

    fn upstream_error(status: StatusCode) -> GitLabCatalogError {
        Self::unavailable(Self::upstream_failure_code(status), Some(status.as_u16()))
    }

    fn transport_error(error: &reqwest::Error) -> GitLabCatalogError {
        let failure_code = if error.is_timeout() {
            "request_timeout"
        } else if error.is_connect() {
            "connection_failed"
        } else {
            "transport_error"
        };
        Self::unavailable(failure_code, None)
    }

    pub fn new(config: GitLabCatalogConfig) -> Result<Self, GitLabCatalogBuildError> {
        let base_url = normalize_base_url(&config.base_url)?;
        if base_url.scheme() != "https" && !config.allow_insecure_http {
            return Err(GitLabCatalogBuildError::InsecureBaseUrl);
        }
        if !(1..=MAX_PAGE_SIZE).contains(&config.page_size) {
            return Err(GitLabCatalogBuildError::InvalidPageSize);
        }
        if config.request_timeout.is_zero() {
            return Err(GitLabCatalogBuildError::InvalidTimeout);
        }
        read_token(&config.token_file).map_err(|_| GitLabCatalogBuildError::TokenUnavailable)?;
        let client = Client::builder()
            .timeout(config.request_timeout)
            .redirect(Policy::none())
            .build()
            .map_err(|_| GitLabCatalogBuildError::HttpClient)?;
        Ok(Self {
            client,
            base_url,
            token_file: config.token_file,
            page_size: config.page_size,
        })
    }

    pub async fn search(
        &self,
        query: Option<&str>,
        cursor: Option<&str>,
    ) -> Result<RepositoryPage, GitLabCatalogError> {
        let page = decode_cursor(cursor)?;
        let query = query.map(str::trim).filter(|query| !query.is_empty());
        if query.is_some_and(|query| query.chars().count() > MAX_SEARCH_CHARS) {
            return Err(GitLabCatalogError::Invalid(format!(
                "search must be at most {MAX_SEARCH_CHARS} characters"
            )));
        }
        let url = self.api_url("api/v4/projects")?;
        let mut request = self.client.get(url).query(&[
            ("membership", "true".to_owned()),
            ("simple", "true".to_owned()),
            ("order_by", "last_activity_at".to_owned()),
            ("sort", "desc".to_owned()),
            ("per_page", self.page_size.to_string()),
            ("page", page.to_string()),
        ]);
        if let Some(query) = query {
            request = request.query(&[("search", query)]);
        }
        let response = self.send(request).await?;
        let next_cursor = next_cursor(response.headers())?;
        let projects = response
            .json::<Vec<GitLabProject>>()
            .await
            .map_err(|_| Self::unavailable("invalid_response", None))?;
        let repositories = projects
            .into_iter()
            .map(RepositorySummary::from_project)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(RepositoryPage {
            repositories,
            next_cursor,
        })
    }

    pub async fn resolve(
        &self,
        repository_ids: &[RepositoryId],
    ) -> Result<Vec<ResolvedRepository>, GitLabCatalogError> {
        let mut repositories = Vec::with_capacity(repository_ids.len());
        for repository_id in repository_ids {
            let project_id = repository_id.project_id();
            let url = self.api_url(&format!("api/v4/projects/{project_id}"))?;
            let response = self.send_allowing_not_found(url).await?;
            let Some(response) = response else {
                return Err(GitLabCatalogError::Invalid(format!(
                    "repository {repository_id} is not visible"
                )));
            };
            let project = response
                .json::<GitLabProject>()
                .await
                .map_err(|_| Self::unavailable("invalid_response", None))?;
            repositories.push(self.resolve_project(repository_id, project)?);
        }
        Ok(repositories)
    }

    async fn send(
        &self,
        request: reqwest::RequestBuilder,
    ) -> Result<reqwest::Response, GitLabCatalogError> {
        let token = self.token_header()?;
        let response = request
            .header(PRIVATE_TOKEN.clone(), token)
            .send()
            .await
            .map_err(|error| Self::transport_error(&error))?;
        if !response.status().is_success() {
            return Err(Self::upstream_error(response.status()));
        }
        Ok(response)
    }

    async fn send_allowing_not_found(
        &self,
        url: Url,
    ) -> Result<Option<reqwest::Response>, GitLabCatalogError> {
        let token = self.token_header()?;
        let response = self
            .client
            .get(url)
            .header(PRIVATE_TOKEN.clone(), token)
            .send()
            .await
            .map_err(|error| Self::transport_error(&error))?;
        if response.status() == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !response.status().is_success() {
            return Err(Self::upstream_error(response.status()));
        }
        Ok(Some(response))
    }

    fn token_header(&self) -> Result<HeaderValue, GitLabCatalogError> {
        let token = read_token(&self.token_file)
            .map_err(|_| Self::unavailable("credential_unavailable", None))?;
        HeaderValue::from_str(&token).map_err(|_| Self::unavailable("credential_unavailable", None))
    }

    fn api_url(&self, path: &str) -> Result<Url, GitLabCatalogError> {
        self.base_url
            .join(path)
            .map_err(|_| Self::unavailable("invalid_configuration", None))
    }

    fn resolve_project(
        &self,
        requested_id: &RepositoryId,
        project: GitLabProject,
    ) -> Result<ResolvedRepository, GitLabCatalogError> {
        if project.id != requested_id.project_id() {
            return Err(Self::unavailable("invalid_response", None));
        }
        if project.archived {
            return Err(GitLabCatalogError::Invalid(format!(
                "repository {requested_id} is archived"
            )));
        }
        let default_branch = clean_required(&project.default_branch).ok_or_else(|| {
            GitLabCatalogError::Invalid(format!("repository {requested_id} has no default branch"))
        })?;
        let clone_url = project
            .http_url_to_repo
            .as_deref()
            .and_then(|value| Url::parse(value).ok())
            .ok_or_else(|| {
                GitLabCatalogError::Invalid(format!(
                    "repository {requested_id} clone URL does not match the configured GitLab origin"
                ))
            })?;
        if !same_origin(&self.base_url, &clone_url)
            || !clone_url.username().is_empty()
            || clone_url.password().is_some()
        {
            return Err(GitLabCatalogError::Invalid(format!(
                "repository {requested_id} clone URL does not match the configured GitLab origin"
            )));
        }
        let relative_path = repository_relative_path(project.id, &project.path_with_namespace);
        Ok(ResolvedRepository {
            repository_id: requested_id.clone(),
            display_name: clean_required(&Some(project.name)).ok_or_else(|| {
                GitLabCatalogError::Invalid(format!(
                    "repository {requested_id} has no display name"
                ))
            })?,
            path_with_namespace: clean_required(&Some(project.path_with_namespace)).ok_or_else(
                || {
                    GitLabCatalogError::Invalid(format!(
                        "repository {requested_id} has no namespace path"
                    ))
                },
            )?,
            default_branch,
            clone_url: clone_url.to_string(),
            relative_path,
        })
    }
}

impl RepositoryResolver for GitLabCatalog {
    fn resolve<'a>(&'a self, repository_ids: &'a [RepositoryId]) -> ResolveRepositoriesFuture<'a> {
        Box::pin(async move {
            GitLabCatalog::resolve(self, repository_ids)
                .await
                .map_err(|error| match error {
                    GitLabCatalogError::Invalid(message) => {
                        RepositoryResolveError::Invalid(message)
                    }
                    GitLabCatalogError::Unavailable => RepositoryResolveError::Unavailable,
                })
        })
    }
}

impl RepositoryCatalog for GitLabCatalog {
    fn search<'a>(
        &'a self,
        query: Option<&'a str>,
        cursor: Option<&'a str>,
    ) -> SearchRepositoriesFuture<'a> {
        Box::pin(GitLabCatalog::search(self, query, cursor))
    }
}

impl RepositorySummary {
    fn from_project(project: GitLabProject) -> Result<Self, GitLabCatalogError> {
        if project.id == 0 || project.path_with_namespace.trim().is_empty() {
            return Err(GitLabCatalog::unavailable("invalid_response", None));
        }
        let namespace = project
            .path_with_namespace
            .rsplit_once('/')
            .map_or("", |(namespace, _)| namespace)
            .to_owned();
        Ok(Self {
            repository_id: RepositoryId::parse(format!("gitlab:{}", project.id))
                .map_err(|_| GitLabCatalog::unavailable("invalid_response", None))?,
            name: project.name,
            namespace,
            path_with_namespace: project.path_with_namespace,
            description: project
                .description
                .map(|description| bounded_text(&description, MAX_DESCRIPTION_CHARS)),
            default_branch: project.default_branch,
            archived: project.archived,
            last_activity_at: project.last_activity_at,
        })
    }
}

fn normalize_base_url(value: &str) -> Result<Url, GitLabCatalogBuildError> {
    let mut url = Url::parse(value.trim()).map_err(|_| GitLabCatalogBuildError::InvalidBaseUrl)?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(GitLabCatalogBuildError::InvalidBaseUrl);
    }
    if !url.path().ends_with('/') {
        let path = format!("{}/", url.path());
        url.set_path(&path);
    }
    Ok(url)
}

fn read_token(path: &PathBuf) -> Result<String, ()> {
    let token = fs::read_to_string(path).map_err(|_| ())?;
    let token = token.trim();
    if token.is_empty() {
        return Err(());
    }
    Ok(token.to_owned())
}

fn clean_required(value: &Option<String>) -> Option<String> {
    value
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn same_origin(expected: &Url, actual: &Url) -> bool {
    expected.scheme() == actual.scheme()
        && expected.host_str() == actual.host_str()
        && expected.port_or_known_default() == actual.port_or_known_default()
}

fn repository_relative_path(project_id: u64, path_with_namespace: &str) -> String {
    let slug = path_with_namespace
        .rsplit('/')
        .next()
        .map(sanitize_path_component)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "repository".to_owned());
    format!("repos/{project_id}-{slug}")
}

fn sanitize_path_component(value: &str) -> String {
    let mut result = String::new();
    let mut delimiter = false;
    for character in value.chars().flat_map(char::to_lowercase) {
        if character.is_ascii_alphanumeric() {
            result.push(character);
            delimiter = false;
        } else if !delimiter && !result.is_empty() {
            result.push('-');
            delimiter = true;
        }
        if result.len() >= 48 {
            break;
        }
    }
    result.trim_end_matches('-').to_owned()
}

fn bounded_text(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}

fn encode_cursor(page: u64) -> String {
    URL_SAFE_NO_PAD.encode(format!("{CURSOR_PREFIX}{page}"))
}

fn decode_cursor(cursor: Option<&str>) -> Result<u64, GitLabCatalogError> {
    let Some(cursor) = cursor else {
        return Ok(1);
    };
    let decoded = URL_SAFE_NO_PAD
        .decode(cursor)
        .ok()
        .and_then(|value| String::from_utf8(value).ok())
        .and_then(|value| value.strip_prefix(CURSOR_PREFIX).map(str::to_owned))
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|page| *page > 0);
    decoded.ok_or_else(|| GitLabCatalogError::Invalid("cursor is invalid".to_owned()))
}

fn next_cursor(headers: &reqwest::header::HeaderMap) -> Result<Option<String>, GitLabCatalogError> {
    let Some(value) = headers.get("x-next-page") else {
        return Ok(None);
    };
    let value = value
        .to_str()
        .map_err(|_| GitLabCatalog::unavailable("invalid_response", None))?;
    if value.is_empty() {
        return Ok(None);
    }
    let page = value
        .parse::<u64>()
        .ok()
        .filter(|page| *page > 0)
        .ok_or_else(|| GitLabCatalog::unavailable("invalid_response", None))?;
    Ok(Some(encode_cursor(page)))
}

#[cfg(test)]
mod gitlab_catalog_tests {
    use std::{
        collections::HashMap,
        fs,
        path::PathBuf,
        sync::{Arc, Mutex},
        time::Duration,
    };

    use axum::{
        Json, Router,
        body::{Body, to_bytes},
        extract::{Path, Query, State},
        http::{HeaderMap, HeaderValue, Request, StatusCode, header::HeaderName},
        response::{IntoResponse, Response},
        routing::get,
    };
    use centaur_session_core::development::RepositoryId;
    use serde_json::{Value, json};
    use tokio::net::TcpListener;
    use tower::ServiceExt;

    use super::{GitLabCatalog, GitLabCatalogConfig, GitLabCatalogError};
    use crate::{AppState, build_router_with_app_state};

    const PRIVATE_TOKEN: HeaderName = HeaderName::from_static("private-token");

    #[derive(Clone)]
    struct FakeGitLab {
        origin: String,
        token: String,
        requests: Arc<Mutex<Vec<RecordedRequest>>>,
    }

    #[derive(Clone, Debug)]
    struct RecordedRequest {
        path: String,
        query: HashMap<String, String>,
        token_matches: bool,
    }

    impl FakeGitLab {
        fn record(&self, path: String, query: HashMap<String, String>, headers: &HeaderMap) {
            let token_matches = headers
                .get(&PRIVATE_TOKEN)
                .and_then(|value| value.to_str().ok())
                .is_some_and(|value| value == self.token);
            self.requests.lock().unwrap().push(RecordedRequest {
                path,
                query,
                token_matches,
            });
        }
    }

    async fn list_projects(
        State(state): State<FakeGitLab>,
        Query(query): Query<HashMap<String, String>>,
        headers: HeaderMap,
    ) -> Response {
        state.record("/api/v4/projects".to_owned(), query.clone(), &headers);
        let page = query.get("page").map(String::as_str).unwrap_or("1");
        let projects = if page == "2" {
            json!([project(&state.origin, 2, false, Some("main"))])
        } else {
            json!([project(&state.origin, 1, false, Some("main"))])
        };
        let mut response = Json(projects).into_response();
        if page == "1" {
            response
                .headers_mut()
                .insert("x-next-page", HeaderValue::from_static("2"));
        }
        response
    }

    async fn get_project(
        State(state): State<FakeGitLab>,
        Path(project_id): Path<u64>,
        headers: HeaderMap,
    ) -> Response {
        state.record(
            format!("/api/v4/projects/{project_id}"),
            HashMap::new(),
            &headers,
        );
        let project = match project_id {
            1 => project(&state.origin, project_id, false, Some("main")),
            3 => project(&state.origin, project_id, true, Some("main")),
            4 => project(&state.origin, project_id, false, None),
            5 => {
                let mut project = project(&state.origin, project_id, false, Some("main"));
                project["http_url_to_repo"] =
                    json!("http://gitlab.invalid.example/group/project-5.git");
                project
            }
            6 => {
                let mut project = project(&state.origin, project_id, false, Some("main"));
                project["http_url_to_repo"] = json!(format!(
                    "https://{}/group/project-6.git",
                    state.origin.trim_start_matches("http://")
                ));
                project
            }
            7 => {
                let mut project = project(&state.origin, project_id, false, Some("main"));
                let clone_url = reqwest::Url::parse(&state.origin).unwrap();
                let port = clone_url.port().unwrap() + 1;
                project["http_url_to_repo"] = json!(format!(
                    "http://{}:{port}/group/project-7.git",
                    clone_url.host_str().unwrap()
                ));
                project
            }
            _ => return StatusCode::NOT_FOUND.into_response(),
        };
        Json(project).into_response()
    }

    fn project(origin: &str, project_id: u64, archived: bool, branch: Option<&str>) -> Value {
        json!({
            "id": project_id,
            "name": format!("Project {project_id}"),
            "path_with_namespace": format!("platform/project-{project_id}"),
            "description": "A bounded project description",
            "default_branch": branch,
            "archived": archived,
            "http_url_to_repo": format!("{origin}/platform/project-{project_id}.git"),
            "last_activity_at": "2026-08-13T01:02:03.000Z"
        })
    }

    async fn spawn_fake_gitlab() -> (String, Arc<Mutex<Vec<RecordedRequest>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let state = FakeGitLab {
            origin: origin.clone(),
            token: "catalog-test-token".to_owned(),
            requests: requests.clone(),
        };
        let app = Router::new()
            .route("/api/v4/projects", get(list_projects))
            .route("/api/v4/projects/{project_id}", get(get_project))
            .with_state(state);
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (origin, requests)
    }

    async fn spawn_status_gitlab(status: StatusCode) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let app = Router::new().route("/api/v4/projects", get(move || async move { status }));
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        origin
    }

    fn token_file() -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "centaur-gitlab-catalog-token-{}",
            uuid::Uuid::new_v4()
        ));
        fs::write(&path, "catalog-test-token\n").unwrap();
        path
    }

    fn config(base_url: String) -> GitLabCatalogConfig {
        GitLabCatalogConfig {
            base_url,
            allow_insecure_http: true,
            token_file: token_file(),
            page_size: 25,
            request_timeout: Duration::from_secs(2),
        }
    }

    #[test]
    fn gitlab_catalog_rejects_insecure_http_by_default() {
        let mut config = config("http://git.example.test:82".to_owned());
        config.allow_insecure_http = false;
        assert!(matches!(
            GitLabCatalog::new(config),
            Err(super::GitLabCatalogBuildError::InsecureBaseUrl)
        ));
    }

    #[tokio::test]
    async fn gitlab_catalog_search_uses_membership_search_and_opaque_pagination() {
        let (origin, requests) = spawn_fake_gitlab().await;
        let catalog = GitLabCatalog::new(config(origin)).unwrap();

        let first = catalog.search(Some(" platform "), None).await.unwrap();
        assert_eq!(first.repositories.len(), 1);
        assert_eq!(first.repositories[0].repository_id.as_str(), "gitlab:1");
        assert_eq!(first.repositories[0].namespace, "platform");
        assert_eq!(
            first.repositories[0].default_branch.as_deref(),
            Some("main")
        );
        assert!(!first.repositories[0].archived);
        let cursor = first.next_cursor.expect("first page has a cursor");
        assert_ne!(cursor, "2");

        let serialized = serde_json::to_value(&first.repositories[0]).unwrap();
        assert!(serialized.get("http_url_to_repo").is_none());
        assert!(serialized.get("clone_url").is_none());

        let second = catalog.search(None, Some(&cursor)).await.unwrap();
        assert_eq!(second.repositories[0].repository_id.as_str(), "gitlab:2");
        assert!(second.next_cursor.is_none());

        let requests = requests.lock().unwrap();
        assert_eq!(requests[0].path, "/api/v4/projects");
        assert_eq!(requests[0].query.get("membership").unwrap(), "true");
        assert_eq!(requests[0].query.get("simple").unwrap(), "true");
        assert_eq!(requests[0].query.get("search").unwrap(), "platform");
        assert_eq!(requests[0].query.get("per_page").unwrap(), "25");
        assert_eq!(requests[1].query.get("page").unwrap(), "2");
        assert!(requests.iter().all(|request| request.token_matches));
    }

    #[tokio::test]
    async fn gitlab_catalog_classifies_upstream_status_without_exposing_details() {
        for (status, failure_code) in [
            (StatusCode::FOUND, "unexpected_redirect"),
            (StatusCode::UNAUTHORIZED, "authentication_rejected"),
            (StatusCode::FORBIDDEN, "authentication_rejected"),
            (StatusCode::INTERNAL_SERVER_ERROR, "upstream_status"),
        ] {
            let origin = spawn_status_gitlab(status).await;
            let catalog = GitLabCatalog::new(config(origin)).unwrap();

            let error = catalog.search(None, None).await.unwrap_err();

            assert!(matches!(error, GitLabCatalogError::Unavailable));
            assert_eq!(GitLabCatalog::upstream_failure_code(status), failure_code);
            assert_eq!(
                error.to_string(),
                "repository catalog is temporarily unavailable"
            );
        }
    }

    #[tokio::test]
    async fn gitlab_catalog_resolve_rejects_ineligible_and_foreign_projects() {
        let (origin, _) = spawn_fake_gitlab().await;
        let catalog = GitLabCatalog::new(config(origin)).unwrap();

        let resolved = catalog
            .resolve(&[RepositoryId::parse("gitlab:1").unwrap()])
            .await
            .unwrap();
        assert_eq!(resolved[0].relative_path, "repos/1-project-1");

        for (project_id, expected) in [
            (3, "archived"),
            (4, "default branch"),
            (5, "configured GitLab origin"),
            (6, "configured GitLab origin"),
            (7, "configured GitLab origin"),
        ] {
            let error = catalog
                .resolve(&[RepositoryId::parse(format!("gitlab:{project_id}")).unwrap()])
                .await
                .unwrap_err();
            assert!(
                matches!(error, GitLabCatalogError::Invalid(message) if message.contains(expected))
            );
        }
    }

    async fn get_json(app: axum::Router, uri: &str) -> (StatusCode, Value) {
        let response = app
            .oneshot(
                Request::get(uri)
                    .header("authorization", "Bearer gitlab-test-ingress-key")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body = serde_json::from_slice(&bytes).unwrap();
        (status, body)
    }

    #[tokio::test]
    async fn gitlab_catalog_route_handles_disabled_invalid_and_upstream_errors() {
        let disabled = build_router_with_app_state(
            AppState::unready().with_feishu_ingress_key("gitlab-test-ingress-key"),
        );
        assert_eq!(
            get_json(disabled, "/api/development/repositories").await.0,
            StatusCode::NOT_FOUND
        );

        let (origin, _) = spawn_fake_gitlab().await;
        let catalog = Arc::new(GitLabCatalog::new(config(origin)).unwrap());
        let app = build_router_with_app_state(
            AppState::unready()
                .with_feishu_ingress_key("gitlab-test-ingress-key")
                .with_repository_catalog(catalog),
        );
        let (status, body) = get_json(
            app.clone(),
            "/api/development/repositories?cursor=not-a-cursor",
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["ok"], false);

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let unavailable_origin = format!("http://{}", listener.local_addr().unwrap());
        drop(listener);
        let unavailable = Arc::new(GitLabCatalog::new(config(unavailable_origin)).unwrap());
        let app = build_router_with_app_state(
            AppState::unready()
                .with_feishu_ingress_key("gitlab-test-ingress-key")
                .with_repository_catalog(unavailable),
        );
        let (status, body) = get_json(app, "/api/development/repositories").await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body["code"], "repository_catalog_unavailable");
        assert_eq!(body["error"], "internal server error");
    }
}
