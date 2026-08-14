mod api_jwt;
mod auth;
pub mod client;
mod error;
mod mcp;
mod routes;
mod slack_proxy;
mod tool_discovery;
pub mod types;

pub use auth::{ApiAuthConfig, ApiAuthConfigError};
pub use centaur_session_runtime::{SandboxRuntime, SessionRuntime};
pub use error::ApiError;
pub use routes::{
    AppState, build_router_with_app_state, build_router_with_runtime,
    build_router_with_session_and_workflow_runtime, build_router_with_session_runtime,
};
pub use tool_discovery::{
    DiscoveredToolProxyFragment, ToolDiscoveryConfig, ToolDiscoveryError,
    discover_persona_registry, discover_tool_proxy_fragment,
};

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    };

    use async_trait::async_trait;
    use axum::{
        body::{Body, to_bytes},
        http::{Method, Request, StatusCode, header},
    };
    use centaur_sandbox_core::{
        ObservedSandbox, SandboxBackend, SandboxError, SandboxHandle, SandboxId, SandboxIo,
        SandboxResult, SandboxSpec, SandboxStatus,
    };
    use centaur_session_runtime::{SandboxRuntime, SessionPrincipalRegistrar};
    use centaur_session_sqlx::PgSessionStore;
    use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
    use serde_json::{Value, json};
    use sqlx::PgPool;
    use tower::ServiceExt;

    use super::{
        ApiAuthConfig, AppState, build_router_with_app_state,
        build_router_with_runtime as build_router_with_iron_control,
    };

    fn test_auth() -> ApiAuthConfig {
        ApiAuthConfig::testing("test-secret")
    }

    fn test_auth_with_slack() -> ApiAuthConfig {
        ApiAuthConfig::testing_with_slack_ingress("test-slackbot-key", "test-secret")
    }

    fn console_token() -> String {
        encode(
            &Header::new(Algorithm::HS256),
            &json!({
                "iss": "centaur-console",
                "sub": "centaur-console",
                "aud": "centaur-api",
                "iat": 1_700_000_000i64,
                "exp": 4_102_444_800i64,
                "token_use": "console_service",
            }),
            &EncodingKey::from_secret(b"test-secret"),
        )
        .unwrap()
    }

    fn principal_token(subject: &str) -> String {
        encode(
            &Header::new(Algorithm::HS256),
            &json!({
                "iss": "centaur-console",
                "sub": subject,
                "aud": "centaur-api",
                "iat": 1_700_000_000i64,
                "exp": 4_102_444_800i64,
                "slack": {
                    "upload_channels": [],
                    "download_channels": [],
                    "history_channels": []
                }
            }),
            &EncodingKey::from_secret(b"test-secret"),
        )
        .unwrap()
    }

    #[derive(Clone, Copy)]
    struct TestSessionPrincipalRegistrar;

    #[async_trait]
    impl SessionPrincipalRegistrar for TestSessionPrincipalRegistrar {
        async fn register_session(
            &self,
            _thread_key: &str,
            _metadata: Option<&Value>,
        ) -> Result<centaur_iron_control::Principal, centaur_iron_control::IronControlError>
        {
            Ok(test_principal("prn_test"))
        }

        async fn register_requester(
            &self,
            _thread_key: &str,
            _metadata: Option<&Value>,
        ) -> Result<Option<centaur_iron_control::Principal>, centaur_iron_control::IronControlError>
        {
            Ok(None)
        }

        async fn get_principal(
            &self,
            principal: &str,
        ) -> Result<centaur_iron_control::Principal, centaur_iron_control::IronControlError>
        {
            Ok(test_principal(principal))
        }
    }

    fn test_principal(id: &str) -> centaur_iron_control::Principal {
        centaur_iron_control::Principal {
            id: id.to_owned(),
            foreign_id: Some("test".to_owned()),
            name: "Test".to_owned(),
            labels: Default::default(),
            sandbox_observability_enabled: true,
            sandbox_api_server_enabled: true,
        }
    }

    fn build_router_with_runtime(
        store: PgSessionStore,
        sandbox_runtime: SandboxRuntime,
    ) -> axum::Router {
        build_router_with_iron_control(
            store,
            sandbox_runtime,
            TestSessionPrincipalRegistrar,
            test_auth(),
        )
    }

    #[tokio::test]
    async fn router_builds() {
        let pool =
            PgPool::connect_lazy("postgres://postgres:postgres@localhost/centaur_test").unwrap();
        let _router = build_router_with_runtime(
            PgSessionStore::new(pool),
            SandboxRuntime::backend(Arc::new(TestBackend::default()), SandboxSpec::new("test")),
        );
    }

    #[tokio::test]
    async fn metrics_endpoint_renders_http_request_metrics() {
        let pool =
            PgPool::connect_lazy("postgres://postgres:postgres@localhost/centaur_test").unwrap();
        let app = build_router_with_runtime(
            PgSessionStore::new(pool),
            SandboxRuntime::backend(Arc::new(TestBackend::default()), SandboxSpec::new("test")),
        );

        let app = app
            .oneshot(
                Request::builder()
                    .uri("/healthz")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(app.status(), StatusCode::OK);

        let pool =
            PgPool::connect_lazy("postgres://postgres:postgres@localhost/centaur_test").unwrap();
        let app = build_router_with_runtime(
            PgSessionStore::new(pool),
            SandboxRuntime::backend(Arc::new(TestBackend::default()), SandboxSpec::new("test")),
        );
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/metrics")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body = String::from_utf8(body.to_vec()).unwrap();
        assert!(
            body.contains(
                r#"http_server_requests_total{method="GET",route="/healthz",status="200"}"#
            )
        );
    }

    #[tokio::test]
    async fn healthz_is_available_before_runtime_is_ready() {
        let app = build_router_with_app_state(AppState::unready(test_auth()));

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/healthz")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn protected_routes_reject_anonymous_requests_before_handlers() {
        for request in [
            Request::builder()
                .method(Method::POST)
                .uri("/api/session/not-a-valid-thread-key")
                .body(Body::from("not-json"))
                .unwrap(),
            Request::builder()
                .uri("/api/slack/channels")
                .body(Body::empty())
                .unwrap(),
            Request::builder()
                .method(Method::POST)
                .uri("/api/sandboxes/drain")
                .body(Body::empty())
                .unwrap(),
            Request::builder()
                .uri("/api/workflows/runs")
                .body(Body::empty())
                .unwrap(),
            Request::builder()
                .uri("/api/admin/slack/archive-imports")
                .body(Body::empty())
                .unwrap(),
        ] {
            let response = build_router_with_app_state(AppState::unready(test_auth()))
                .oneshot(request)
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
            assert_eq!(
                response.headers().get(header::WWW_AUTHENTICATE),
                Some(&"Bearer".parse().unwrap())
            );
        }
    }

    #[tokio::test]
    async fn console_service_jwt_passes_every_capability_route_family() {
        for request in [
            Request::builder()
                .uri("/api/session/slack%3AC123%3A123.456")
                .header(header::AUTHORIZATION, format!("Bearer {}", console_token()))
                .body(Body::empty())
                .unwrap(),
            Request::builder()
                .method(Method::POST)
                .uri("/api/session/slack%3AC123%3A123.456/interrupt")
                .header(header::AUTHORIZATION, format!("Bearer {}", console_token()))
                .body(Body::empty())
                .unwrap(),
            Request::builder()
                .method(Method::POST)
                .uri("/api/sandboxes/drain")
                .header(header::AUTHORIZATION, format!("Bearer {}", console_token()))
                .body(Body::empty())
                .unwrap(),
            Request::builder()
                .uri("/api/workflows/runs")
                .header(header::AUTHORIZATION, format!("Bearer {}", console_token()))
                .body(Body::empty())
                .unwrap(),
            Request::builder()
                .method(Method::POST)
                .uri("/api/workflows/events")
                .header(header::AUTHORIZATION, format!("Bearer {}", console_token()))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"event_name":"test.event","payload":{}}"#))
                .unwrap(),
            Request::builder()
                .uri("/api/admin/slack/archive-imports")
                .header(header::AUTHORIZATION, format!("Bearer {}", console_token()))
                .body(Body::empty())
                .unwrap(),
            Request::builder()
                .uri("/api/admin/slack/dm-sync/checkpoints")
                .header(header::AUTHORIZATION, format!("Bearer {}", console_token()))
                .body(Body::empty())
                .unwrap(),
            Request::builder()
                .uri("/api/admin/google/docs-sync/checkpoint")
                .header(header::AUTHORIZATION, format!("Bearer {}", console_token()))
                .body(Body::empty())
                .unwrap(),
            Request::builder()
                .uri("/api/admin/granola/sync/checkpoint")
                .header(header::AUTHORIZATION, format!("Bearer {}", console_token()))
                .body(Body::empty())
                .unwrap(),
        ] {
            let response = build_router_with_app_state(AppState::unready(test_auth()))
                .oneshot(request)
                .await
                .unwrap();
            assert_ne!(response.status(), StatusCode::UNAUTHORIZED);
            assert_ne!(response.status(), StatusCode::FORBIDDEN);
        }

        let slack_proxy = build_router_with_app_state(AppState::unready(test_auth()))
            .oneshot(
                Request::builder()
                    .uri("/api/slack/channels")
                    .header(header::AUTHORIZATION, format!("Bearer {}", console_token()))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(slack_proxy.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn public_and_protocol_routes_are_outside_the_general_auth_gate() {
        for uri in [
            "/healthz",
            "/readyz",
            "/metrics",
            "/.well-known/oauth-protected-resource",
            "/.well-known/oauth-protected-resource/mcp",
        ] {
            let response = build_router_with_app_state(AppState::unready(test_auth()))
                .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_ne!(response.status(), StatusCode::UNAUTHORIZED, "{uri}");
        }

        let response = build_router_with_app_state(AppState::unready(test_auth()))
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/webhooks/example")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_ne!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn ingress_key_is_platform_scoped_and_capability_limited() {
        let authorized = format!("Bearer {}", "test-slackbot-key");
        for (request, expected) in [
            (
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/session/slack%3AC123%3A123.456")
                    .header(header::AUTHORIZATION, &authorized)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"harness_type":"codex"}"#))
                    .unwrap(),
                StatusCode::SERVICE_UNAVAILABLE,
            ),
            (
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/session/discord%3A123%3A456")
                    .header(header::AUTHORIZATION, &authorized)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"harness_type":"codex"}"#))
                    .unwrap(),
                StatusCode::FORBIDDEN,
            ),
            (
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/sandboxes/drain")
                    .header(header::AUTHORIZATION, &authorized)
                    .body(Body::empty())
                    .unwrap(),
                StatusCode::FORBIDDEN,
            ),
            (
                Request::builder()
                    .uri("/api/workflows/runs")
                    .header(header::AUTHORIZATION, &authorized)
                    .body(Body::empty())
                    .unwrap(),
                StatusCode::FORBIDDEN,
            ),
            (
                Request::builder()
                    .uri("/api/slack/channels")
                    .header(header::AUTHORIZATION, &authorized)
                    .body(Body::empty())
                    .unwrap(),
                StatusCode::FORBIDDEN,
            ),
            (
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/workflows/events")
                    .header(header::AUTHORIZATION, &authorized)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"event_name":"test.event","payload":{}}"#))
                    .unwrap(),
                StatusCode::SERVICE_UNAVAILABLE,
            ),
        ] {
            let response = build_router_with_app_state(AppState::unready(test_auth_with_slack()))
                .oneshot(request)
                .await
                .unwrap();
            assert_eq!(response.status(), expected);
        }
    }

    #[tokio::test]
    async fn principal_jwt_is_read_only_and_archive_exception_is_subject_scoped() {
        let principal = principal_token("prn_sandbox");
        let write_response = build_router_with_app_state(AppState::unready(test_auth()))
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/session/slack%3AC123%3A123.456")
                    .header(header::AUTHORIZATION, format!("Bearer {principal}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"harness_type":"codex"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(write_response.status(), StatusCode::FORBIDDEN);

        let pool =
            PgPool::connect_lazy("postgres://postgres:postgres@localhost/centaur_test").unwrap();
        let state = AppState::unready(test_auth());
        state.mark_ready_with_workflow_host(
            centaur_session_runtime::SessionRuntime::new(
                PgSessionStore::new(pool),
                SandboxRuntime::backend(Arc::new(TestBackend::default()), SandboxSpec::new("test")),
                TestSessionPrincipalRegistrar,
            ),
            None,
            None,
            "prn_workflow_host".to_owned(),
        );

        let other_response = build_router_with_app_state(state.clone())
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/admin/slack/archive-imports/import-1/download-url")
                    .header(header::AUTHORIZATION, format!("Bearer {principal}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(other_response.status(), StatusCode::FORBIDDEN);

        let workflow_host = principal_token("prn_workflow_host");
        let host_response = build_router_with_app_state(state)
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/admin/slack/archive-imports/import-1/download-url")
                    .header(header::AUTHORIZATION, format!("Bearer {workflow_host}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(host_response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn healthz_decodes_slack_client_bearer_jwt_when_present() {
        let app = build_router_with_app_state(AppState::unready(test_auth()));
        let token = encode(
            &Header::new(Algorithm::HS256),
            &json!({
                "iss": "centaur-console",
                "sub": "principal_123",
                "aud": "centaur-api",
                "iat": 1_700_000_000i64,
                "exp": 4_102_444_800i64,
                "slack": {
                    "upload_channels": ["C123456789"],
                    "download_channels": ["C987654321"],
                    "history_channels": ["C111111111"]
                }
            }),
            &EncodingKey::from_secret(b"test-secret"),
        )
        .unwrap();

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/healthz")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body.get("ok").and_then(Value::as_bool), Some(true));
        assert_eq!(
            body.pointer("/slack_client_jwt/claims/sub")
                .and_then(Value::as_str),
            Some("principal_123")
        );
        assert_eq!(
            body.pointer("/slack_client_jwt/claims/slack/upload_channels/0")
                .and_then(Value::as_str),
            Some("C123456789")
        );
        assert_eq!(
            body.pointer("/slack_client_jwt/claims/slack/download_channels/0")
                .and_then(Value::as_str),
            Some("C987654321")
        );
        assert_eq!(
            body.pointer("/slack_client_jwt/claims/slack/history_channels/0")
                .and_then(Value::as_str),
            Some("C111111111")
        );
    }

    #[tokio::test]
    async fn readyz_reports_starting_until_runtime_is_ready() {
        let state = AppState::unready(test_auth());
        let app = build_router_with_app_state(state.clone());

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/readyz")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);

        let pool =
            PgPool::connect_lazy("postgres://postgres:postgres@localhost/centaur_test").unwrap();
        state.mark_ready(
            centaur_session_runtime::SessionRuntime::new(
                PgSessionStore::new(pool),
                SandboxRuntime::backend(Arc::new(TestBackend::default()), SandboxSpec::new("test")),
                TestSessionPrincipalRegistrar,
            ),
            None,
            None,
        );
        let app = build_router_with_app_state(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/readyz")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn runtime_routes_report_unavailable_until_runtime_is_ready() {
        for mut request in [
            Request::builder()
                .method(Method::GET)
                .uri("/api/session/slack%3AC123%3A123.456")
                .body(Body::empty())
                .unwrap(),
            Request::builder()
                .method(Method::POST)
                .uri("/api/session/slack%3AC123%3A123.456")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"harness_type":"codex"}"#))
                .unwrap(),
            Request::builder()
                .method(Method::POST)
                .uri("/api/session/slack%3AC123%3A123.456/messages")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"messages":[]}"#))
                .unwrap(),
            Request::builder()
                .method(Method::POST)
                .uri("/api/session/slack%3AC123%3A123.456/execute")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"input_lines":[]}"#))
                .unwrap(),
            Request::builder()
                .method(Method::GET)
                .uri("/api/session/slack%3AC123%3A123.456/events")
                .body(Body::empty())
                .unwrap(),
            Request::builder()
                .method(Method::POST)
                .uri("/api/sandboxes/drain")
                .body(Body::empty())
                .unwrap(),
            Request::builder()
                .method(Method::GET)
                .uri("/api/workflows/schedules")
                .body(Body::empty())
                .unwrap(),
            Request::builder()
                .method(Method::GET)
                .uri("/api/workflows/runs")
                .body(Body::empty())
                .unwrap(),
            Request::builder()
                .method(Method::POST)
                .uri("/api/workflows/runs")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"workflow_name":"agent_turn","input":{}}"#))
                .unwrap(),
            Request::builder()
                .method(Method::GET)
                .uri("/api/workflows/runs/run-1")
                .body(Body::empty())
                .unwrap(),
            Request::builder()
                .method(Method::POST)
                .uri("/api/workflows/runs/run-1/cancel")
                .body(Body::empty())
                .unwrap(),
            Request::builder()
                .method(Method::POST)
                .uri("/api/workflows/events")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"event_name":"test.event","payload":{}}"#))
                .unwrap(),
            Request::builder()
                .method(Method::POST)
                .uri("/api/webhooks/test")
                .body(Body::empty())
                .unwrap(),
        ] {
            request.headers_mut().insert(
                header::AUTHORIZATION,
                format!("Bearer {}", console_token()).parse().unwrap(),
            );
            let app = build_router_with_app_state(AppState::unready(test_auth()));
            let response = app.oneshot(request).await.unwrap();
            assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        }
    }

    #[tokio::test]
    async fn mcp_requires_bearer_before_runtime_is_ready() {
        let app = build_router_with_app_state(AppState::unready(test_auth()));

        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/mcp")
                    .header(header::HOST, "centaur.local")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let challenge = response
            .headers()
            .get(header::WWW_AUTHENTICATE)
            .and_then(|value| value.to_str().ok())
            .unwrap();
        assert!(challenge.contains("Bearer"));
        assert!(challenge.contains(
            "resource_metadata=\"http://centaur.local/.well-known/oauth-protected-resource/mcp\""
        ));
    }

    #[tokio::test]
    async fn append_messages_does_not_apply_a_session_body_limit() {
        let pool =
            PgPool::connect_lazy("postgres://postgres:postgres@localhost/centaur_test").unwrap();
        let app = build_router_with_runtime(
            PgSessionStore::new(pool),
            SandboxRuntime::backend(Arc::new(TestBackend::default()), SandboxSpec::new("test")),
        );

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/session/slack%3AC123%3A123.456/messages")
                    .header(header::AUTHORIZATION, format!("Bearer {}", console_token()))
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::CONTENT_LENGTH, (256 * 1024 * 1024 + 1).to_string())
                    .body(Body::from(r#"{"messages":"not-an-array"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_ne!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[tokio::test]
    async fn execute_does_not_apply_a_session_body_limit() {
        let pool =
            PgPool::connect_lazy("postgres://postgres:postgres@localhost/centaur_test").unwrap();
        let app = build_router_with_runtime(
            PgSessionStore::new(pool),
            SandboxRuntime::backend(Arc::new(TestBackend::default()), SandboxSpec::new("test")),
        );

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/session/slack%3AC123%3A123.456/execute")
                    .header(header::AUTHORIZATION, format!("Bearer {}", console_token()))
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::CONTENT_LENGTH, (256 * 1024 * 1024 + 1).to_string())
                    .body(Body::from(r#"{"input_lines":"not-an-array"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_ne!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[tokio::test]
    async fn session_context_exposes_slack_channel_and_thread_ts() {
        let pool =
            PgPool::connect_lazy("postgres://postgres:postgres@localhost/centaur_test").unwrap();
        let app = build_router_with_runtime(
            PgSessionStore::new(pool),
            SandboxRuntime::backend(Arc::new(TestBackend::default()), SandboxSpec::new("test")),
        );

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/session/slack%3AC123%3A123.456")
                    .header(header::AUTHORIZATION, format!("Bearer {}", console_token()))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["thread_key"], "slack:C123:123.456");
        assert_eq!(body["platform"], "slack");
        assert_eq!(body["slack"]["channel_id"], "C123");
        assert_eq!(body["slack"]["thread_ts"], "123.456");
        assert!(body.get("discord").is_none());
        assert!(body.get("linear").is_none());
    }

    #[tokio::test]
    async fn session_context_exposes_discord_guild_channel_and_thread() {
        let pool =
            PgPool::connect_lazy("postgres://postgres:postgres@localhost/centaur_test").unwrap();
        let app = build_router_with_runtime(
            PgSessionStore::new(pool),
            SandboxRuntime::backend(Arc::new(TestBackend::default()), SandboxSpec::new("test")),
        );

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/session/discord%3A111%3A222%3A333")
                    .header(header::AUTHORIZATION, format!("Bearer {}", console_token()))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["thread_key"], "discord:111:222:333");
        assert_eq!(body["platform"], "discord");
        assert_eq!(body["discord"]["guild_id"], "111");
        assert_eq!(body["discord"]["channel_id"], "222");
        assert_eq!(body["discord"]["thread_id"], "333");
        assert!(body.get("slack").is_none());
        assert!(body.get("linear").is_none());
    }

    #[tokio::test]
    async fn session_context_exposes_linear_issue_comment_and_session() {
        let pool =
            PgPool::connect_lazy("postgres://postgres:postgres@localhost/centaur_test").unwrap();
        let app = build_router_with_runtime(
            PgSessionStore::new(pool),
            SandboxRuntime::backend(Arc::new(TestBackend::default()), SandboxSpec::new("test")),
        );

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/session/linear%3AISSUE%3Ac%3ACMT%3As%3ASESS")
                    .header(header::AUTHORIZATION, format!("Bearer {}", console_token()))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["thread_key"], "linear:ISSUE:c:CMT:s:SESS");
        assert_eq!(body["platform"], "linear");
        assert_eq!(body["linear"]["issue_id"], "ISSUE");
        assert_eq!(body["linear"]["comment_id"], "CMT");
        assert_eq!(body["linear"]["agent_session_id"], "SESS");
        assert!(body.get("slack").is_none());
        assert!(body.get("discord").is_none());
    }

    #[tokio::test]
    async fn session_context_exposes_github_repo_number_and_review_comment() {
        let pool =
            PgPool::connect_lazy("postgres://postgres:postgres@localhost/centaur_test").unwrap();
        let app = build_router_with_runtime(
            PgSessionStore::new(pool),
            SandboxRuntime::backend(Arc::new(TestBackend::default()), SandboxSpec::new("test")),
        );

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/session/github%3A0xSplits%2Fcentaur%3A704%3Arc%3A99")
                    .header(header::AUTHORIZATION, format!("Bearer {}", console_token()))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["thread_key"], "github:0xSplits/centaur:704:rc:99");
        assert_eq!(body["platform"], "github");
        assert_eq!(body["github"]["owner"], "0xSplits");
        assert_eq!(body["github"]["repo"], "centaur");
        assert_eq!(body["github"]["number"], 704);
        assert_eq!(body["github"]["kind"], "pr");
        assert_eq!(body["github"]["review_comment_id"], 99);
        assert!(body.get("slack").is_none());
        assert!(body.get("discord").is_none());
        assert!(body.get("linear").is_none());
    }

    #[tokio::test]
    async fn session_context_omits_slack_for_non_slack_thread_key() {
        let pool =
            PgPool::connect_lazy("postgres://postgres:postgres@localhost/centaur_test").unwrap();
        let app = build_router_with_runtime(
            PgSessionStore::new(pool),
            SandboxRuntime::backend(Arc::new(TestBackend::default()), SandboxSpec::new("test")),
        );

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/session/cli%3Atest")
                    .header(header::AUTHORIZATION, format!("Bearer {}", console_token()))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["thread_key"], "cli:test");
        assert_eq!(body["platform"], "unknown");
        assert!(body.get("slack").is_none());
        assert!(body.get("discord").is_none());
        assert!(body.get("linear").is_none());
    }

    #[derive(Default)]
    struct TestBackend {
        next_id: AtomicU64,
    }

    #[async_trait]
    impl SandboxBackend for TestBackend {
        fn name(&self) -> &'static str {
            "test"
        }

        async fn create(&self, _spec: SandboxSpec) -> SandboxResult<SandboxHandle> {
            let id = self.next_id.fetch_add(1, Ordering::Relaxed) + 1;
            Ok(SandboxHandle::new(
                SandboxId::new(format!("test-{id}")),
                self.name(),
            ))
        }

        async fn open_io(&self, _id: &SandboxId) -> SandboxResult<SandboxIo> {
            unreachable!("router construction should not open sandbox I/O")
        }

        async fn status(&self, _id: &SandboxId) -> SandboxResult<SandboxStatus> {
            Ok(SandboxStatus::Running)
        }

        async fn observe(&self, id: &SandboxId) -> SandboxResult<ObservedSandbox> {
            Ok(ObservedSandbox::new(
                id.clone(),
                self.name(),
                SandboxStatus::Running,
            ))
        }

        async fn list_observed(&self) -> SandboxResult<Vec<ObservedSandbox>> {
            Ok(Vec::new())
        }

        async fn stop(&self, _id: &SandboxId) -> SandboxResult<()> {
            Ok(())
        }

        async fn pause(&self, _id: &SandboxId) -> SandboxResult<()> {
            Err(SandboxError::Unsupported {
                backend: self.name(),
                operation: "pause",
            })
        }

        async fn resume(&self, _id: &SandboxId) -> SandboxResult<()> {
            Err(SandboxError::Unsupported {
                backend: self.name(),
                operation: "resume",
            })
        }
    }
}
