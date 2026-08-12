use std::{collections::HashSet, future::Future, pin::Pin};

use axum::{
    Json, Router,
    body::Body,
    extract::{Path, Query, State, rejection::JsonRejection},
    http::{HeaderMap, HeaderValue, header},
    response::Response,
    routing::{get, post},
};
use centaur_session_core::{
    MessageRole, ThreadKey,
    development::{
        AcceptDevelopmentTask, ConfirmRepositorySelection, RepositoryId, RepositorySelectionDraft,
        RepositorySelectionOutcome, ResolvedRepository,
    },
};
use thiserror::Error;

use crate::{
    ApiError,
    api_jwt::{bearer_token, verify_console_jwt},
    gitlab::{GitLabCatalogError, RepositoryPage},
    routes::AppState,
    types::{
        AcceptDevelopmentTaskRequest, ConfirmDevelopmentSelectionRequest,
        CreateAddRepositorySelectionRequest, DecideDevelopmentSelectionRequest,
    },
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DevelopmentPrincipal {
    pub principal_id: String,
    pub is_admin: bool,
}

pub trait DevelopmentAuthorizer: Send + Sync {
    fn authorize(&self, headers: &HeaderMap) -> Result<DevelopmentPrincipal, ApiError>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ConsoleJwtDevelopmentAuthorizer;

#[derive(serde::Deserialize)]
struct DevelopmentJwtClaims {
    sub: String,
    #[serde(default)]
    centaur_admin: bool,
}

impl DevelopmentAuthorizer for ConsoleJwtDevelopmentAuthorizer {
    fn authorize(&self, headers: &HeaderMap) -> Result<DevelopmentPrincipal, ApiError> {
        let token = bearer_token(headers)?;
        let claims = verify_console_jwt::<DevelopmentJwtClaims>(token)?;
        Ok(DevelopmentPrincipal {
            principal_id: claims.sub,
            is_admin: claims.centaur_admin,
        })
    }
}

pub type ResolveRepositoriesFuture<'a> = Pin<
    Box<dyn Future<Output = Result<Vec<ResolvedRepository>, RepositoryResolveError>> + Send + 'a>,
>;

pub trait RepositoryResolver: Send + Sync {
    fn resolve<'a>(&'a self, repository_ids: &'a [RepositoryId]) -> ResolveRepositoriesFuture<'a>;
}

pub type SearchRepositoriesFuture<'a> =
    Pin<Box<dyn Future<Output = Result<RepositoryPage, GitLabCatalogError>> + Send + 'a>>;

pub trait RepositoryCatalog: RepositoryResolver {
    fn search<'a>(
        &'a self,
        query: Option<&'a str>,
        cursor: Option<&'a str>,
    ) -> SearchRepositoriesFuture<'a>;
}

#[derive(Debug, Error)]
pub enum RepositoryResolveError {
    #[error("repository catalog is not configured")]
    Disabled,
    #[error("repository catalog rejected the request: {0}")]
    Invalid(String),
    #[error("repository catalog is temporarily unavailable")]
    Unavailable,
}

pub(crate) fn development_router() -> Router<AppState> {
    Router::new()
        .route("/api/development/repositories", get(search_repositories))
        .route("/api/development/tasks", post(accept_development_task))
        .route(
            "/api/development/changesets/{changeset_id}",
            get(get_changeset),
        )
        .route(
            "/api/development/changesets/{changeset_id}/artifacts/{artifact_ref}",
            get(get_changeset_artifact),
        )
        .route(
            "/api/development/selections/{selection_flow_id}/confirm",
            post(confirm_development_selection),
        )
        .route(
            "/api/development/selections/{selection_flow_id}/no-project",
            post(confirm_no_project),
        )
        .route(
            "/api/development/selections/{selection_flow_id}/cancel",
            post(cancel_development_selection),
        )
        .route(
            "/api/development/sessions/{thread_key}/repositories",
            post(create_add_repository_selection),
        )
}

async fn get_changeset(
    State(state): State<AppState>,
    Path(changeset_id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<centaur_session_core::development::DevelopmentChangeSet>, ApiError> {
    let principal = state.development_authorizer().authorize(&headers)?;
    let changeset = state
        .runtime()?
        .get_changeset(&changeset_id, &principal.principal_id, principal.is_admin)
        .await?;
    Ok(Json(changeset))
}

async fn get_changeset_artifact(
    State(state): State<AppState>,
    Path((changeset_id, artifact_ref)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let principal = state.development_authorizer().authorize(&headers)?;
    let content = state
        .runtime()?
        .get_changeset_artifact(
            &changeset_id,
            &artifact_ref,
            &principal.principal_id,
            principal.is_admin,
        )
        .await?;
    let mut response = Response::new(Body::from(content));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/x-diff; charset=utf-8"),
    );
    response.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-store"),
    );
    Ok(response)
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RepositoryCatalogQuery {
    query: Option<String>,
    cursor: Option<String>,
}

async fn search_repositories(
    State(state): State<AppState>,
    Query(query): Query<RepositoryCatalogQuery>,
) -> Result<Json<RepositoryPage>, ApiError> {
    let page = state
        .repository_catalog()?
        .search(query.query.as_deref(), query.cursor.as_deref())
        .await
        .map_err(repository_catalog_error)?;
    Ok(Json(page))
}

async fn accept_development_task(
    State(state): State<AppState>,
    request: Result<Json<AcceptDevelopmentTaskRequest>, JsonRejection>,
) -> Result<Json<centaur_session_core::development::AcceptedDevelopmentTask>, ApiError> {
    let Json(request) = development_json(request)?;
    if request.message.role != MessageRole::User {
        return Err(ApiError::BadRequest(
            "development task message role must be user".to_owned(),
        ));
    }
    let accepted = state
        .runtime()?
        .accept_development_task(AcceptDevelopmentTask {
            channel: request.channel,
            platform_event_id: request.platform_event_id,
            platform_message_id: request.platform_message_id,
            harness_type: request.harness_type,
            initiator: request.initiator,
            message: request.message,
            session_metadata: request.session_metadata,
        })
        .await?;
    Ok(Json(accepted))
}

async fn confirm_development_selection(
    State(state): State<AppState>,
    Path(selection_flow_id): Path<String>,
    request: Result<Json<ConfirmDevelopmentSelectionRequest>, JsonRejection>,
) -> Result<Json<RepositorySelectionOutcome>, ApiError> {
    let Json(request) = development_json(request)?;
    let repositories = state
        .repository_resolver()?
        .resolve(&request.repository_ids)
        .await
        .map_err(repository_resolve_error)?;
    validate_resolved_repositories(&request.repository_ids, &repositories)?;
    let outcome = state
        .runtime()?
        .confirm_repository_selection(&ConfirmRepositorySelection {
            selection_flow_id,
            expected_version: request.expected_version,
            decided_by_principal_id: request.decided_by_principal_id,
            repositories,
        })
        .await?;
    Ok(Json(outcome))
}

async fn confirm_no_project(
    State(state): State<AppState>,
    Path(selection_flow_id): Path<String>,
    request: Result<Json<DecideDevelopmentSelectionRequest>, JsonRejection>,
) -> Result<Json<RepositorySelectionOutcome>, ApiError> {
    let Json(request) = development_json(request)?;
    let outcome = state
        .runtime()?
        .confirm_repository_selection(&ConfirmRepositorySelection {
            selection_flow_id,
            expected_version: request.expected_version,
            decided_by_principal_id: request.decided_by_principal_id,
            repositories: Vec::new(),
        })
        .await?;
    Ok(Json(outcome))
}

async fn cancel_development_selection(
    State(state): State<AppState>,
    Path(selection_flow_id): Path<String>,
    request: Result<Json<DecideDevelopmentSelectionRequest>, JsonRejection>,
) -> Result<Json<RepositorySelectionOutcome>, ApiError> {
    let Json(request) = development_json(request)?;
    let outcome = state
        .runtime()?
        .cancel_repository_selection(
            &selection_flow_id,
            request.expected_version,
            &request.decided_by_principal_id,
        )
        .await?;
    Ok(Json(outcome))
}

async fn create_add_repository_selection(
    State(state): State<AppState>,
    Path(raw_thread_key): Path<String>,
    request: Result<Json<CreateAddRepositorySelectionRequest>, JsonRejection>,
) -> Result<Json<RepositorySelectionDraft>, ApiError> {
    let Json(_request) = development_json(request)?;
    let thread_key = ThreadKey::try_from(raw_thread_key)?;
    let draft = state
        .runtime()?
        .create_add_repository_selection(&thread_key)
        .await?;
    Ok(Json(draft))
}

fn development_json<T>(request: Result<Json<T>, JsonRejection>) -> Result<Json<T>, ApiError> {
    request.map_err(|_| ApiError::BadRequest("invalid development request body".to_owned()))
}

fn validate_resolved_repositories(
    requested: &[RepositoryId],
    resolved: &[ResolvedRepository],
) -> Result<(), ApiError> {
    let requested = requested.iter().collect::<HashSet<_>>();
    let resolved = resolved
        .iter()
        .map(|repository| &repository.repository_id)
        .collect::<HashSet<_>>();
    if requested.len() != resolved.len() || requested != resolved {
        return Err(ApiError::Internal(
            "repository resolver returned a mismatched project set".to_owned(),
        ));
    }
    Ok(())
}

fn repository_resolve_error(error: RepositoryResolveError) -> ApiError {
    match error {
        RepositoryResolveError::Disabled => ApiError::NotFound(error.to_string()),
        RepositoryResolveError::Invalid(_) => ApiError::BadRequest(error.to_string()),
        RepositoryResolveError::Unavailable => ApiError::RepositoryCatalogUnavailable,
    }
}

fn repository_catalog_error(error: GitLabCatalogError) -> ApiError {
    match error {
        GitLabCatalogError::Invalid(message) => ApiError::BadRequest(message),
        GitLabCatalogError::Unavailable => ApiError::RepositoryCatalogUnavailable,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use async_trait::async_trait;
    use axum::{
        body::{Body, to_bytes},
        http::{Request, StatusCode, header},
    };
    use centaur_iron_control::{IronControlError, Principal};
    use centaur_sandbox_core::{
        ObservedSandbox, SandboxBackend, SandboxError, SandboxHandle, SandboxId, SandboxIo,
        SandboxResult, SandboxSpec, SandboxStatus,
    };
    use centaur_session_core::development::{
        CollectedChangeSetRepositoryState, CompleteChangeSetCollection,
        CompleteChangeSetRepository, CompleteWorkspacePreparation, ConfirmRepositorySelection,
        PreparedRepositorySnapshot, RepositoryId, ResolvedRepository,
    };
    use centaur_session_runtime::{SandboxRuntime, SessionPrincipalRegistrar, SessionRuntime};
    use centaur_session_sqlx::PgSessionStore;
    use serde_json::{Value, json};
    use tower::ServiceExt;

    use super::{
        DevelopmentAuthorizer, DevelopmentPrincipal, RepositoryResolveError, RepositoryResolver,
        ResolveRepositoriesFuture,
    };
    use crate::{ApiError, AppState, build_router_with_app_state};

    #[derive(Clone, Copy)]
    struct TestRegistrar;

    #[async_trait]
    impl SessionPrincipalRegistrar for TestRegistrar {
        async fn register_session(
            &self,
            _thread_key: &str,
            _metadata: Option<&Value>,
        ) -> Result<Principal, IronControlError> {
            Ok(Principal {
                id: "prn_development_test".to_owned(),
                foreign_id: Some("development-test".to_owned()),
                name: "Development Test".to_owned(),
                labels: Default::default(),
                sandbox_observability_enabled: true,
                sandbox_api_server_enabled: true,
            })
        }

        async fn get_principal(&self, principal: &str) -> Result<Principal, IronControlError> {
            let mut value = self.register_session("unused", None).await?;
            value.id = principal.to_owned();
            Ok(value)
        }
    }

    #[derive(Default)]
    struct TestBackend;

    #[async_trait]
    impl SandboxBackend for TestBackend {
        fn name(&self) -> &'static str {
            "development-api-test"
        }

        async fn create(&self, _spec: SandboxSpec) -> SandboxResult<SandboxHandle> {
            Err(SandboxError::io(
                "development API test must not create a sandbox",
            ))
        }

        async fn open_io(&self, _id: &SandboxId) -> SandboxResult<SandboxIo> {
            Err(SandboxError::io(
                "development API test must not open sandbox IO",
            ))
        }

        async fn status(&self, _id: &SandboxId) -> SandboxResult<SandboxStatus> {
            Ok(SandboxStatus::Gone)
        }

        async fn observe(&self, id: &SandboxId) -> SandboxResult<ObservedSandbox> {
            Ok(ObservedSandbox::new(
                id.clone(),
                self.name(),
                SandboxStatus::Gone,
            ))
        }

        async fn list_observed(&self) -> SandboxResult<Vec<ObservedSandbox>> {
            Ok(Vec::new())
        }

        async fn stop(&self, _id: &SandboxId) -> SandboxResult<()> {
            Ok(())
        }

        async fn pause(&self, _id: &SandboxId) -> SandboxResult<()> {
            Ok(())
        }

        async fn resume(&self, _id: &SandboxId) -> SandboxResult<()> {
            Ok(())
        }
    }

    struct TestResolver;

    struct TestAuthorizer;

    impl DevelopmentAuthorizer for TestAuthorizer {
        fn authorize(
            &self,
            headers: &axum::http::HeaderMap,
        ) -> Result<DevelopmentPrincipal, ApiError> {
            let value = headers
                .get(header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.strip_prefix("Bearer "))
                .ok_or_else(|| ApiError::Unauthorized("missing bearer token".to_owned()))?;
            let (principal_id, is_admin) = match value {
                "principal-1" => ("principal-1", false),
                "principal-2" => ("principal-2", false),
                "admin-1" => ("admin-1", true),
                _ => return Err(ApiError::Unauthorized("invalid bearer token".to_owned())),
            };
            Ok(DevelopmentPrincipal {
                principal_id: principal_id.to_owned(),
                is_admin,
            })
        }
    }

    impl RepositoryResolver for TestResolver {
        fn resolve<'a>(
            &'a self,
            repository_ids: &'a [RepositoryId],
        ) -> ResolveRepositoriesFuture<'a> {
            Box::pin(async move {
                repository_ids
                    .iter()
                    .map(|repository_id| {
                        let project_id = repository_id.project_id();
                        Ok(ResolvedRepository {
                            repository_id: repository_id.clone(),
                            display_name: format!("project-{project_id}"),
                            path_with_namespace: format!("group/project-{project_id}"),
                            default_branch: "main".to_owned(),
                            clone_url: format!(
                                "http://git.example.internal:82/group/project-{project_id}.git"
                            ),
                            relative_path: format!("repos/{project_id}-project-{project_id}"),
                        })
                    })
                    .collect::<Result<Vec<_>, RepositoryResolveError>>()
            })
        }
    }

    async fn test_app() -> Option<(axum::Router, PgSessionStore)> {
        let Ok(url) = std::env::var("SESSION_RUNTIME_TEST_DATABASE_URL") else {
            eprintln!("skipping: SESSION_RUNTIME_TEST_DATABASE_URL not set");
            return None;
        };
        let store = PgSessionStore::connect(&url)
            .await
            .expect("connect test db");
        store.run_migrations().await.expect("run migrations");
        let pool = store.pool().clone();
        let runtime = SessionRuntime::new(
            store.clone(),
            SandboxRuntime::backend(Arc::new(TestBackend), SandboxSpec::new("test")),
            TestRegistrar,
        );
        Some((
            build_router_with_app_state(
                AppState::ready_with_pool(runtime, None, Some(pool))
                    .with_repository_resolver(Arc::new(TestResolver))
                    .with_development_authorizer(Arc::new(TestAuthorizer)),
            ),
            store,
        ))
    }

    fn intake_body(suffix: &str) -> Value {
        json!({
            "channel": {
                "platform": "feishu",
                "tenant_key": format!("tenant-{suffix}"),
                "conversation_key": format!("chat-{suffix}"),
                "root_message_id": format!("message-{suffix}")
            },
            "platform_event_id": format!("event-{suffix}"),
            "platform_message_id": format!("message-{suffix}"),
            "harness_type": "codex",
            "initiator": {"principal_id": "principal-1"},
            "message": {
                "client_message_id": format!("message-{suffix}"),
                "role": "user",
                "parts": [{"type": "text", "text": "Fix the test"}],
                "metadata": {"source": "feishu"}
            },
            "session_metadata": {"source": "feishu"}
        })
    }

    async fn post(app: axum::Router, uri: &str, body: Value) -> (StatusCode, Value) {
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(uri)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body = serde_json::from_slice(&body).unwrap_or(Value::Null);
        (status, body)
    }

    async fn get(
        app: axum::Router,
        uri: &str,
        bearer: Option<&str>,
    ) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
        let mut request = Request::builder().method("GET").uri(uri);
        if let Some(bearer) = bearer {
            request = request.header(header::AUTHORIZATION, format!("Bearer {bearer}"));
        }
        let response = app
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec();
        (status, headers, body)
    }

    #[tokio::test]
    async fn development_task_route_is_idempotent() {
        let Some((app, _store)) = test_app().await else {
            return;
        };
        let suffix = uuid::Uuid::new_v4().to_string();
        let body = intake_body(&suffix);

        let (created_status, created) =
            post(app.clone(), "/api/development/tasks", body.clone()).await;
        let (replay_status, replay) = post(app, "/api/development/tasks", body).await;

        assert_eq!(created_status, StatusCode::OK);
        assert_eq!(replay_status, StatusCode::OK);
        assert_eq!(created["created"], true);
        assert_eq!(replay["created"], false);
        assert_eq!(created["thread_key"], replay["thread_key"]);
        assert_eq!(created["execution_id"], replay["execution_id"]);
    }

    #[tokio::test]
    async fn development_selection_routes_reject_authority_fields_and_stale_versions() {
        let Some((app, _store)) = test_app().await else {
            return;
        };
        let suffix = uuid::Uuid::new_v4().to_string();
        let (_, accepted) = post(app.clone(), "/api/development/tasks", intake_body(&suffix)).await;
        let flow_id = accepted["selection_flow_id"].as_str().unwrap();
        let uri = format!("/api/development/selections/{flow_id}/confirm");

        for invalid in [
            json!({"expected_version": 1, "decided_by_principal_id": "principal-1", "repository_ids": ["42"]}),
            json!({"expected_version": 1, "decided_by_principal_id": "principal-1", "repository_ids": ["gitlab:42"], "clone_url": "http://attacker.invalid/repo.git"}),
            json!({"expected_version": 1, "decided_by_principal_id": "principal-1", "repository_ids": ["gitlab:42"], "role": "admin"}),
        ] {
            let (status, _) = post(app.clone(), &uri, invalid).await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
        }

        let mut non_user = intake_body(&uuid::Uuid::new_v4().to_string());
        non_user["message"]["role"] = json!("system");
        assert_eq!(
            post(app.clone(), "/api/development/tasks", non_user)
                .await
                .0,
            StatusCode::BAD_REQUEST
        );

        let valid = json!({
            "expected_version": 1,
            "decided_by_principal_id": "principal-1",
            "repository_ids": ["gitlab:42"]
        });
        let unauthorized = json!({
            "expected_version": 1,
            "decided_by_principal_id": "principal-2",
            "repository_ids": ["gitlab:42"]
        });
        assert_eq!(
            post(app.clone(), &uri, unauthorized).await.0,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            post(app.clone(), &uri, valid.clone()).await.0,
            StatusCode::OK
        );
        assert_eq!(post(app, &uri, valid).await.0, StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn development_no_project_cancel_and_add_routes_share_durable_state() {
        let Some((app, store)) = test_app().await else {
            return;
        };
        let no_project_suffix = uuid::Uuid::new_v4().to_string();
        let (_, no_project) = post(
            app.clone(),
            "/api/development/tasks",
            intake_body(&no_project_suffix),
        )
        .await;
        let no_project_uri = format!(
            "/api/development/selections/{}/no-project",
            no_project["selection_flow_id"].as_str().unwrap()
        );
        let decision = json!({
            "expected_version": 1,
            "decided_by_principal_id": "principal-1"
        });
        let (status, confirmed) = post(app.clone(), &no_project_uri, decision).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(confirmed["repository_ids"], json!([]));
        assert_eq!(confirmed["workspace_state"], "provisioning");

        let cancel_suffix = uuid::Uuid::new_v4().to_string();
        let (_, cancellable) = post(
            app.clone(),
            "/api/development/tasks",
            intake_body(&cancel_suffix),
        )
        .await;
        let cancel_uri = format!(
            "/api/development/selections/{}/cancel",
            cancellable["selection_flow_id"].as_str().unwrap()
        );
        let (status, cancelled) = post(
            app.clone(),
            &cancel_uri,
            json!({
                "expected_version": 1,
                "decided_by_principal_id": "principal-1"
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(cancelled["state"], "cancelled");

        sqlx::query(
            "update session_executions set status = 'completed', blocking_reason = null, completed_at = now() where thread_key = $1",
        )
        .bind(no_project["thread_key"].as_str().unwrap())
        .execute(store.pool())
        .await
        .expect("complete no-project execution");
        sqlx::query("update session_workspaces set state = 'ready' where thread_key = $1")
            .bind(no_project["thread_key"].as_str().unwrap())
            .execute(store.pool())
            .await
            .expect("mark workspace ready");
        let add_uri = format!(
            "/api/development/sessions/{}/repositories",
            urlencoding::encode(no_project["thread_key"].as_str().unwrap())
        );
        let (status, draft) = post(app.clone(), &add_uri, json!({})).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(draft["kind"], "add");

        let add_confirm_uri = format!(
            "/api/development/selections/{}/confirm",
            draft["selection_flow_id"].as_str().unwrap()
        );
        let (status, added) = post(
            app,
            &add_confirm_uri,
            json!({
                "expected_version": 1,
                "decided_by_principal_id": "principal-1",
                "repository_ids": ["gitlab:84"]
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(added["repository_ids"], json!(["gitlab:84"]));
        assert_eq!(added["execution_blocker"], Value::Null);
    }

    #[tokio::test]
    async fn changeset_routes_require_owner_or_admin_and_serve_immutable_artifact() {
        use sha2::{Digest, Sha256};

        let Some((app, store)) = test_app().await else {
            return;
        };
        let suffix = uuid::Uuid::new_v4().to_string();
        let (_, accepted) = post(app.clone(), "/api/development/tasks", intake_body(&suffix)).await;
        let flow_id = accepted["selection_flow_id"].as_str().unwrap();
        let workspace_id = accepted["workspace_id"].as_str().unwrap();
        let execution_id = accepted["execution_id"].as_str().unwrap();
        store
            .confirm_repository_selection(&ConfirmRepositorySelection {
                selection_flow_id: flow_id.to_owned(),
                expected_version: 1,
                decided_by_principal_id: "principal-1".to_owned(),
                repositories: vec![ResolvedRepository {
                    repository_id: "gitlab:42".parse().unwrap(),
                    display_name: "Project".to_owned(),
                    path_with_namespace: "group/project".to_owned(),
                    default_branch: "main".to_owned(),
                    clone_url: "http://git.example.internal:82/group/project.git".to_owned(),
                    relative_path: "repos/42-project".to_owned(),
                }],
            })
            .await
            .unwrap();
        let workspace_claim = store
            .claim_workspace_preparation(
                workspace_id,
                "workspace-owner",
                std::time::Duration::from_secs(30),
            )
            .await
            .unwrap();
        let base_sha = "a".repeat(40);
        store
            .complete_workspace_preparation(&CompleteWorkspacePreparation {
                workspace_id: workspace_id.to_owned(),
                attempt: workspace_claim.workspace.preparation_attempt,
                lease_owner: "workspace-owner".to_owned(),
                storage_ref: "workspace-api-review".to_owned(),
                prepared: vec![PreparedRepositorySnapshot {
                    repository_id: "gitlab:42".parse().unwrap(),
                    base_sha: base_sha.clone(),
                    local_branch: "centaur/review".to_owned(),
                    head_sha: base_sha.clone(),
                }],
                failed: Vec::new(),
            })
            .await
            .unwrap();
        store.complete_execution(execution_id).await.unwrap();
        let changeset = store
            .begin_changeset_collection(execution_id, "collector")
            .await
            .unwrap()
            .unwrap();
        let patch = b"diff --git a/README.md b/README.md\n".to_vec();
        let head_sha = "b".repeat(40);
        let patch_hash = format!("sha256:{}", hex::encode(Sha256::digest(&patch)));
        let completed = store
            .complete_changeset_collection(&CompleteChangeSetCollection {
                changeset_id: changeset.changeset_id.clone(),
                lease_owner: "collector".to_owned(),
                repositories: vec![CompleteChangeSetRepository {
                    repository_id: "gitlab:42".parse().unwrap(),
                    state: CollectedChangeSetRepositoryState::Changed,
                    base_sha: base_sha.clone(),
                    recorded_head_sha: base_sha,
                    head_sha: Some(head_sha.clone()),
                    commit_metadata: json!([{"sha": head_sha}]),
                    changed_file_count: 1,
                    additions: 1,
                    deletions: 0,
                    patch_hash: Some(patch_hash),
                    patch: patch.clone(),
                    test_evidence: json!([]),
                    failure_code: None,
                    failure_message: None,
                }],
            })
            .await
            .unwrap()
            .unwrap();
        let summary_uri = format!("/api/development/changesets/{}", completed.changeset_id);

        assert_eq!(
            get(app.clone(), &summary_uri, None).await.0,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            get(
                app.clone(),
                &format!("{summary_uri}?signature=not-authority"),
                Some("principal-2"),
            )
            .await
            .0,
            StatusCode::FORBIDDEN
        );
        for bearer in ["principal-1", "admin-1"] {
            let (status, _, body) = get(app.clone(), &summary_uri, Some(bearer)).await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(
                serde_json::from_slice::<Value>(&body).unwrap()["changeset_id"],
                completed.changeset_id
            );
        }

        let artifact_ref = completed.repositories[0]
            .patch_artifact_ref
            .as_deref()
            .unwrap();
        let artifact_uri = format!(
            "/api/development/changesets/{}/artifacts/{}",
            completed.changeset_id,
            urlencoding::encode(artifact_ref)
        );
        assert_eq!(
            get(app.clone(), &artifact_uri, Some("principal-2")).await.0,
            StatusCode::FORBIDDEN
        );
        let (status, headers, body) = get(app, &artifact_uri, Some("principal-1")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(headers[header::CONTENT_TYPE], "text/x-diff; charset=utf-8");
        assert_eq!(body, patch);
    }
}
