use std::{collections::HashSet, sync::Arc, time::Duration as StdDuration};

use anyhow::Context as _;
use reqwest::{StatusCode, Url, header};
use time::{Duration, OffsetDateTime};
use tokio::sync::Mutex;

use crate::{
    model::{Catalog, Endpoint, RegisteredRoute, RegistrySnapshot},
    store::SignerStore,
};

pub struct Registry {
    store: Arc<dyn SignerStore>,
    client: reqwest::Client,
    url: Url,
    cache_ttl: Duration,
    max_stale: Duration,
    state: Mutex<RegistryState>,
}

#[derive(Default)]
struct RegistryState {
    snapshot: Option<RegistrySnapshot>,
    retry_after: Option<OffsetDateTime>,
}

const REGISTRY_REQUEST_TIMEOUT: StdDuration = StdDuration::from_secs(3);
const REGISTRY_CONNECT_TIMEOUT: StdDuration = StdDuration::from_secs(2);
const REGISTRY_RETRY_BACKOFF: Duration = Duration::seconds(30);

impl Registry {
    pub fn new(
        store: Arc<dyn SignerStore>,
        url: &str,
        cache_ttl: Duration,
        max_stale: Duration,
    ) -> anyhow::Result<Self> {
        let raw_url = url;
        let url = Url::parse(raw_url).context("parse MPP registry URL")?;
        anyhow::ensure!(
            url.scheme() == "https"
                && url.host_str().is_some()
                && url.username().is_empty()
                && url.password().is_none()
                && url.query().is_none()
                && url.fragment().is_none()
                && !has_dot_segments(raw_url),
            "MPP registry URL must be absolute HTTPS without credentials"
        );
        anyhow::ensure!(
            cache_ttl.is_positive() && max_stale >= cache_ttl,
            "MPP registry cache durations are invalid"
        );
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(REGISTRY_CONNECT_TIMEOUT)
            .timeout(REGISTRY_REQUEST_TIMEOUT)
            .build()?;
        Ok(Self {
            store,
            client,
            url,
            cache_ttl,
            max_stale,
            state: Mutex::new(RegistryState::default()),
        })
    }

    pub async fn warm(&self) -> anyhow::Result<()> {
        self.snapshot().await.map(|_| ())
    }

    pub async fn route(
        &self,
        scheme: &str,
        host: &str,
        method: &str,
        path_and_query: &str,
    ) -> anyhow::Result<RegisteredRoute> {
        let snapshot = self.snapshot().await?;
        anyhow::ensure!(
            scheme.eq_ignore_ascii_case("https"),
            "invalid MPP request scheme"
        );
        let authority =
            Url::parse(&format!("{scheme}://{host}")).context("invalid MPP request authority")?;
        let hostname = authority
            .host_str()
            .context("MPP request authority has no hostname")?;
        let port = authority
            .port_or_known_default()
            .context("MPP request authority has no port")?;
        let concrete_path = path_and_query
            .split_once('?')
            .map_or(path_and_query, |(path, _)| path);
        anyhow::ensure!(
            concrete_path.starts_with('/') && !has_dot_segments(concrete_path),
            "invalid MPP request path"
        );

        let mut matches = Vec::new();
        for service in snapshot.catalog.services {
            if service
                .status
                .as_deref()
                .is_some_and(|status| status != "active")
            {
                continue;
            }
            let Some(base_url) = service.base_url() else {
                continue;
            };
            let service_url = Url::parse(base_url)?;
            if service_url
                .host_str()
                .is_none_or(|candidate| !candidate.eq_ignore_ascii_case(hostname))
                || service_url.port_or_known_default() != Some(port)
            {
                continue;
            }
            for endpoint in &service.endpoints {
                let registered_path = registered_path(&service_url, &endpoint.path);
                if endpoint.method.eq_ignore_ascii_case(method)
                    && path_matches(&registered_path, concrete_path)
                {
                    matches.push((
                        path_specificity(&registered_path),
                        RegisteredRoute {
                            service: service.clone(),
                            endpoint: endpoint.clone(),
                        },
                    ));
                }
            }
        }
        anyhow::ensure!(
            !matches.is_empty(),
            "request route is not in the MPP registry"
        );
        let highest_specificity = matches
            .iter()
            .map(|(specificity, _)| *specificity)
            .max()
            .expect("non-empty matches");
        matches.retain(|(specificity, _)| *specificity == highest_specificity);
        anyhow::ensure!(
            matches.len() == 1,
            "request route matches multiple equally specific MPP registry entries"
        );
        Ok(matches.remove(0).1)
    }

    pub async fn ready(&self) -> bool {
        let Ok(state) = self.state.try_lock() else {
            return false;
        };
        state.snapshot.as_ref().is_some_and(|snapshot| {
            OffsetDateTime::now_utc() - snapshot.fetched_at <= self.max_stale
        })
    }

    async fn snapshot(&self) -> anyhow::Result<RegistrySnapshot> {
        let mut state = self.state.lock().await;
        if state.snapshot.is_none()
            && let Some(cached) = self.store.load_registry_cache().await?
        {
            match validate_catalog(&cached.catalog) {
                Ok(()) => state.snapshot = Some(cached),
                Err(error) => {
                    tracing::warn!(error = %error, "discarding invalid cached MPP registry");
                }
            }
        }
        let now = OffsetDateTime::now_utc();
        if let Some(snapshot) = state.snapshot.as_ref()
            && now - snapshot.fetched_at <= self.cache_ttl
        {
            record_snapshot_metrics(snapshot, now);
            return Ok(snapshot.clone());
        }

        if state
            .retry_after
            .is_some_and(|retry_after| now < retry_after)
        {
            let snapshot = state
                .snapshot
                .as_ref()
                .filter(|snapshot| now - snapshot.fetched_at <= self.max_stale)
                .context("MPP registry cache expired while refresh is backed off")?;
            metrics::counter!("centaur_mpp_registry_stale_uses_total").increment(1);
            record_snapshot_metrics(snapshot, now);
            return Ok(snapshot.clone());
        }

        match self.refresh(state.snapshot.as_ref(), now).await {
            Ok(snapshot) => {
                self.store.save_registry_cache(&snapshot).await?;
                metrics::counter!("centaur_mpp_registry_refresh_total", "outcome" => "success")
                    .increment(1);
                record_snapshot_metrics(&snapshot, now);
                state.snapshot = Some(snapshot.clone());
                state.retry_after = None;
                Ok(snapshot)
            }
            Err(error) => {
                metrics::counter!("centaur_mpp_registry_refresh_total", "outcome" => "failure")
                    .increment(1);
                state.retry_after =
                    Some(now + std::cmp::min(self.cache_ttl, REGISTRY_RETRY_BACKOFF));
                if let Some(snapshot) = state.snapshot.as_ref()
                    && now - snapshot.fetched_at <= self.max_stale
                {
                    metrics::counter!("centaur_mpp_registry_stale_uses_total").increment(1);
                    record_snapshot_metrics(snapshot, now);
                    tracing::warn!(
                        error = %error,
                        cache_age_seconds = (now - snapshot.fetched_at).whole_seconds(),
                        "using stale MPP registry cache"
                    );
                    return Ok(snapshot.clone());
                }
                Err(error)
            }
        }
    }

    async fn refresh(
        &self,
        cached: Option<&RegistrySnapshot>,
        now: OffsetDateTime,
    ) -> anyhow::Result<RegistrySnapshot> {
        let mut request = self.client.get(self.url.clone());
        if let Some(etag) = cached.and_then(|snapshot| snapshot.etag.as_deref()) {
            request = request.header(header::IF_NONE_MATCH, etag);
        }
        if let Some(last_modified) = cached.and_then(|snapshot| snapshot.last_modified.as_deref()) {
            request = request.header(header::IF_MODIFIED_SINCE, last_modified);
        }
        let response = request.send().await.context("fetch MPP registry")?;
        if response.status() == StatusCode::NOT_MODIFIED {
            let cached = cached.context("MPP registry returned 304 without a cached catalog")?;
            return Ok(RegistrySnapshot {
                catalog: cached.catalog.clone(),
                fetched_at: now,
                etag: cached.etag.clone(),
                last_modified: cached.last_modified.clone(),
            });
        }
        anyhow::ensure!(
            response.status().is_success(),
            "MPP registry returned HTTP {}",
            response.status().as_u16()
        );
        let headers = response.headers().clone();
        let catalog = response
            .json::<Catalog>()
            .await
            .context("decode MPP registry JSON")?;
        validate_catalog(&catalog)?;
        Ok(RegistrySnapshot {
            catalog,
            fetched_at: now,
            etag: header_string(&headers, header::ETAG),
            last_modified: header_string(&headers, header::LAST_MODIFIED),
        })
    }
}

fn record_snapshot_metrics(snapshot: &RegistrySnapshot, now: OffsetDateTime) {
    metrics::gauge!("centaur_mpp_registry_cache_age_seconds")
        .set((now - snapshot.fetched_at).whole_seconds().max(0) as f64);
    metrics::gauge!("centaur_mpp_registry_services").set(snapshot.catalog.services.len() as f64);
}

fn header_string(
    headers: &reqwest::header::HeaderMap,
    name: reqwest::header::HeaderName,
) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
}

fn validate_catalog(catalog: &Catalog) -> anyhow::Result<()> {
    let mut ids = HashSet::new();
    for service in &catalog.services {
        anyhow::ensure!(
            !service.id.is_empty() && ids.insert(service.id.as_str()),
            "MPP registry contains an invalid or duplicate service id"
        );
        let base_url = service
            .base_url()
            .context("MPP registry service has no URL")?;
        let url = Url::parse(base_url).context("parse MPP service URL")?;
        anyhow::ensure!(
            url.scheme() == "https"
                && url.host_str().is_some()
                && url.username().is_empty()
                && url.password().is_none()
                && url.query().is_none()
                && url.fragment().is_none()
                && !has_dot_segments(base_url),
            "MPP registry service URL must be absolute HTTPS without credentials"
        );
        anyhow::ensure!(
            service
                .realm
                .as_deref()
                .is_none_or(|realm| !realm.is_empty()),
            "MPP registry service realm cannot be empty"
        );
        for endpoint in &service.endpoints {
            validate_endpoint(endpoint)?;
        }
    }
    Ok(())
}

fn validate_endpoint(endpoint: &Endpoint) -> anyhow::Result<()> {
    anyhow::ensure!(
        !endpoint.method.is_empty()
            && endpoint
                .method
                .chars()
                .all(|character| character.is_ascii_alphabetic()),
        "MPP registry endpoint has an invalid method"
    );
    anyhow::ensure!(
        endpoint.path.starts_with('/') && !has_dot_segments(&endpoint.path),
        "MPP registry endpoint has an invalid path"
    );
    Ok(())
}

fn path_matches(template: &str, concrete: &str) -> bool {
    let template_segments = template.split('/').collect::<Vec<_>>();
    let concrete_segments = concrete.split('/').collect::<Vec<_>>();
    template_segments.len() == concrete_segments.len()
        && template_segments
            .iter()
            .zip(concrete_segments)
            .all(|(expected, actual)| {
                let parameter = is_path_parameter(expected);
                (parameter && !actual.is_empty()) || expected == &actual
            })
}

fn path_specificity(path: &str) -> usize {
    path.split('/')
        .filter(|segment| !segment.is_empty() && !is_path_parameter(segment))
        .count()
}

fn is_path_parameter(segment: &str) -> bool {
    segment.starts_with(':') || (segment.starts_with('{') && segment.ends_with('}'))
}

fn registered_path(service_url: &Url, endpoint_path: &str) -> String {
    format!(
        "{}{}",
        service_url.path().trim_end_matches('/'),
        endpoint_path
    )
}

fn has_dot_segments(path: &str) -> bool {
    path.split('/').any(|segment| {
        let encoded = segment.to_ascii_lowercase();
        encoded.contains("%2f")
            || encoded.contains("%5c")
            || matches!(encoded.replace("%2e", ".").as_str(), "." | "..")
    })
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashMap,
        sync::{
            Arc, Mutex as StdMutex,
            atomic::{AtomicBool, AtomicUsize, Ordering},
        },
    };

    use async_trait::async_trait;
    use axum::{
        Json, Router,
        extract::State,
        http::{HeaderMap, StatusCode},
        response::{IntoResponse, Response},
        routing::get,
    };
    use reqwest::Url;
    use time::{Duration, OffsetDateTime};
    use tokio::net::TcpListener;
    use uuid::Uuid;

    use super::{RegistryState, has_dot_segments, path_matches, registered_path, validate_catalog};
    use crate::{
        model::{
            ActiveExecution, BeginAttempt, Catalog, CompletedAttempt, CompletionOutcome, Endpoint,
            NewAttempt, RegistrySnapshot, Service,
        },
        store::SignerStore,
    };

    struct MemoryStore {
        cached: StdMutex<Option<RegistrySnapshot>>,
        saves: AtomicUsize,
    }

    impl MemoryStore {
        fn new(cached: Option<RegistrySnapshot>) -> Self {
            Self {
                cached: StdMutex::new(cached),
                saves: AtomicUsize::new(0),
            }
        }
    }

    #[async_trait]
    impl SignerStore for MemoryStore {
        async fn load_registry_cache(&self) -> anyhow::Result<Option<RegistrySnapshot>> {
            Ok(self.cached.lock().expect("cached registry").clone())
        }

        async fn save_registry_cache(&self, snapshot: &RegistrySnapshot) -> anyhow::Result<()> {
            *self.cached.lock().expect("cached registry") = Some(snapshot.clone());
            self.saves.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        async fn active_execution(
            &self,
            _sandbox_id: &str,
        ) -> anyhow::Result<Option<ActiveExecution>> {
            Ok(None)
        }

        async fn active_execution_lease_count(&self) -> anyhow::Result<i64> {
            Ok(0)
        }

        async fn active_reservation_count(&self) -> anyhow::Result<i64> {
            Ok(0)
        }

        async fn reconcile_stale_reservations(&self, _max_age_seconds: i64) -> anyhow::Result<i64> {
            Ok(0)
        }

        async fn begin_attempt(
            &self,
            _attempt: &NewAttempt,
            _max_per_charge_atomic: Option<i64>,
            _max_daily_atomic: Option<i64>,
        ) -> anyhow::Result<BeginAttempt> {
            Ok(BeginAttempt::Created)
        }

        async fn mark_authorized(&self, _attempt_id: Uuid) -> anyhow::Result<()> {
            Ok(())
        }

        async fn mark_sign_failed(
            &self,
            _attempt_id: Uuid,
            _error_code: &str,
        ) -> anyhow::Result<()> {
            Ok(())
        }

        async fn complete_attempt(
            &self,
            _attempt_id: Uuid,
            _outcome: CompletionOutcome,
            _replay_status: Option<u16>,
            _receipt_hash: Option<&str>,
            _error_code: Option<&str>,
        ) -> anyhow::Result<Option<CompletedAttempt>> {
            Ok(None)
        }

        async fn ready(&self) -> bool {
            true
        }
    }

    #[derive(Clone)]
    struct RegistryServerState {
        response: Arc<AtomicUsize>,
        calls: Arc<AtomicUsize>,
        saw_conditions: Arc<AtomicBool>,
    }

    async fn registry_handler(
        State(state): State<RegistryServerState>,
        headers: HeaderMap,
    ) -> Response {
        state.calls.fetch_add(1, Ordering::SeqCst);
        state.saw_conditions.store(
            headers
                .get("if-none-match")
                .and_then(|value| value.to_str().ok())
                == Some("\"catalog-v1\"")
                && headers
                    .get("if-modified-since")
                    .and_then(|value| value.to_str().ok())
                    == Some("Thu, 30 Jul 2026 00:00:00 GMT"),
            Ordering::SeqCst,
        );
        match state.response.load(Ordering::SeqCst) {
            0 => StatusCode::NOT_MODIFIED.into_response(),
            1 => Json(serde_json::json!({"services": "invalid"})).into_response(),
            3 => {
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                Json(catalog()).into_response()
            }
            _ => (StatusCode::SERVICE_UNAVAILABLE, "unavailable").into_response(),
        }
    }

    async fn registry_server(state: RegistryServerState) -> Url {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind registry server");
        let address = listener.local_addr().expect("registry server address");
        tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new()
                    .route("/registry", get(registry_handler))
                    .with_state(state),
            )
            .await
            .expect("serve registry");
        });
        Url::parse(&format!("http://{address}/registry")).expect("registry URL")
    }

    fn catalog() -> Catalog {
        Catalog {
            services: vec![Service {
                id: "catalog".to_owned(),
                name: Some("Catalog".to_owned()),
                description: None,
                service_url: Some("https://api.example".to_owned()),
                url: None,
                realm: Some("api.example".to_owned()),
                categories: vec!["data".to_owned()],
                tags: vec![],
                status: Some("active".to_owned()),
                endpoints: vec![Endpoint {
                    method: "GET".to_owned(),
                    path: "/v1/records".to_owned(),
                    description: None,
                    payment: None,
                    extra: HashMap::new(),
                }],
                extra: HashMap::new(),
            }],
            extra: HashMap::new(),
        }
    }

    fn snapshot(fetched_at: OffsetDateTime) -> RegistrySnapshot {
        snapshot_with_catalog(catalog(), fetched_at)
    }

    fn snapshot_with_catalog(catalog: Catalog, fetched_at: OffsetDateTime) -> RegistrySnapshot {
        RegistrySnapshot {
            catalog,
            fetched_at,
            etag: Some("\"catalog-v1\"".to_owned()),
            last_modified: Some("Thu, 30 Jul 2026 00:00:00 GMT".to_owned()),
        }
    }

    fn registry(store: Arc<MemoryStore>, url: Url) -> super::Registry {
        super::Registry {
            store,
            client: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(std::time::Duration::from_millis(50))
                .build()
                .expect("registry client"),
            url,
            cache_ttl: Duration::minutes(15),
            max_stale: Duration::hours(24),
            state: tokio::sync::Mutex::new(RegistryState::default()),
        }
    }

    #[test]
    fn route_templates_match_whole_segments_only() {
        assert!(path_matches("/v1/records/:id", "/v1/records/abc"));
        assert!(path_matches("/v1/records/{id}", "/v1/records/abc"));
        assert!(!path_matches("/v1/records/:id", "/v1/records"));
        assert!(!path_matches("/v1/records/:id", "/v1/records/abc/extra"));
    }

    #[test]
    fn dot_segments_are_rejected() {
        assert!(has_dot_segments("/v1/../admin"));
        assert!(has_dot_segments("/v1/%2E%2E/admin"));
        assert!(has_dot_segments("/v1/%2e./admin"));
        assert!(has_dot_segments("/v1/record%2Fadmin"));
        assert!(!has_dot_segments("/v1/records"));
    }

    #[test]
    fn registry_url_rejects_credentials_queries_fragments_and_dot_segments() {
        for url in [
            "https://user:password@registry.example/services",
            "https://registry.example/services?target=other",
            "https://registry.example/services#fragment",
            "https://registry.example/%2e%2e/services",
        ] {
            let store = Arc::new(MemoryStore::new(None));
            let result =
                super::Registry::new(store, url, Duration::minutes(15), Duration::hours(24));
            assert!(result.is_err(), "accepted unsafe registry URL {url}");
        }
    }

    #[test]
    fn service_base_paths_are_part_of_registered_routes() {
        let service = Url::parse("https://gateway.example/provider").unwrap();
        assert_eq!(
            registered_path(&service, "/v1/search"),
            "/provider/v1/search"
        );
        assert!(path_matches(
            &registered_path(&service, "/v1/:id"),
            "/provider/v1/record-1"
        ));
    }

    #[test]
    fn catalog_allows_overlapping_route_templates() {
        let mut catalog = catalog();
        let mut duplicate = catalog.services[0].clone();
        duplicate.id = "duplicate".to_owned();
        duplicate.endpoints[0].path = "/v1/:resource".to_owned();
        catalog.services.push(duplicate);

        validate_catalog(&catalog).expect("overlapping templates are resolved per request");
    }

    #[tokio::test]
    async fn route_prefers_the_template_with_more_literal_segments() {
        let mut routes = catalog();
        routes.services[0].id = "specific".to_owned();
        let mut generic = routes.services[0].clone();
        generic.id = "generic".to_owned();
        generic.endpoints[0].path = "/v1/:resource".to_owned();
        routes.services.push(generic);
        let store = Arc::new(MemoryStore::new(Some(snapshot_with_catalog(
            routes,
            OffsetDateTime::now_utc(),
        ))));

        let selected = registry(store, Url::parse("http://unused.example/registry").unwrap())
            .route("https", "api.example", "GET", "/v1/records")
            .await
            .expect("specific route");

        assert_eq!(selected.service.id, "specific");
    }

    #[tokio::test]
    async fn route_rejects_equal_specificity_matches() {
        let mut routes = catalog();
        routes.services[0].id = "first".to_owned();
        routes.services[0].endpoints[0].path = "/v1/:collection/fixed".to_owned();
        let mut second = routes.services[0].clone();
        second.id = "second".to_owned();
        second.endpoints[0].path = "/v1/records/:id".to_owned();
        routes.services.push(second);
        let store = Arc::new(MemoryStore::new(Some(snapshot_with_catalog(
            routes,
            OffsetDateTime::now_utc(),
        ))));

        let error = registry(store, Url::parse("http://unused.example/registry").unwrap())
            .route("https", "api.example", "GET", "/v1/records/fixed")
            .await
            .expect_err("equal-specificity route must fail closed");

        assert!(error.to_string().contains("multiple equally specific"));
    }

    #[tokio::test]
    async fn empty_path_segment_does_not_match_a_parameter() {
        let mut routes = catalog();
        routes.services[0].id = "root".to_owned();
        routes.services[0].endpoints[0].path = "/".to_owned();
        let mut keyed = routes.services[0].clone();
        keyed.id = "keyed".to_owned();
        keyed.endpoints[0].path = "/:key".to_owned();
        routes.services.push(keyed);
        let store = Arc::new(MemoryStore::new(Some(snapshot_with_catalog(
            routes,
            OffsetDateTime::now_utc(),
        ))));

        let selected = registry(store, Url::parse("http://unused.example/registry").unwrap())
            .route("https", "api.example", "GET", "/")
            .await
            .expect("root route");

        assert_eq!(selected.service.id, "root");
    }

    #[tokio::test]
    async fn fresh_cache_skips_refresh_and_conditional_refresh_is_saved() {
        let state = RegistryServerState {
            response: Arc::new(AtomicUsize::new(0)),
            calls: Arc::new(AtomicUsize::new(0)),
            saw_conditions: Arc::new(AtomicBool::new(false)),
        };
        let url = registry_server(state.clone()).await;
        let fresh_store = Arc::new(MemoryStore::new(Some(snapshot(OffsetDateTime::now_utc()))));
        let fresh = registry(fresh_store.clone(), url.clone())
            .snapshot()
            .await
            .expect("fresh cache");
        assert_eq!(fresh.catalog.services[0].id, "catalog");
        assert_eq!(state.calls.load(Ordering::SeqCst), 0);
        assert_eq!(fresh_store.saves.load(Ordering::SeqCst), 0);

        let stale_store = Arc::new(MemoryStore::new(Some(snapshot(
            OffsetDateTime::now_utc() - Duration::hours(1),
        ))));
        let refreshed = registry(stale_store.clone(), url)
            .snapshot()
            .await
            .expect("conditional refresh");
        assert!(OffsetDateTime::now_utc() - refreshed.fetched_at < Duration::seconds(2));
        assert_eq!(state.calls.load(Ordering::SeqCst), 1);
        assert!(state.saw_conditions.load(Ordering::SeqCst));
        assert_eq!(stale_store.saves.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn invalid_refresh_uses_valid_stale_cache_but_expired_cache_fails_closed() {
        let state = RegistryServerState {
            response: Arc::new(AtomicUsize::new(1)),
            calls: Arc::new(AtomicUsize::new(0)),
            saw_conditions: Arc::new(AtomicBool::new(false)),
        };
        let url = registry_server(state.clone()).await;
        let stale_at = OffsetDateTime::now_utc() - Duration::hours(1);
        let stale_store = Arc::new(MemoryStore::new(Some(snapshot(stale_at))));
        let stale = registry(stale_store.clone(), url.clone())
            .snapshot()
            .await
            .expect("stale fallback");
        assert_eq!(stale.fetched_at, stale_at);
        assert_eq!(stale_store.saves.load(Ordering::SeqCst), 0);

        let expired_store = Arc::new(MemoryStore::new(Some(snapshot(
            OffsetDateTime::now_utc() - Duration::hours(25),
        ))));
        let error = registry(expired_store.clone(), url)
            .snapshot()
            .await
            .expect_err("expired cache must fail");
        assert!(error.to_string().contains("decode MPP registry JSON"));
        assert_eq!(expired_store.saves.load(Ordering::SeqCst), 0);
        assert_eq!(state.calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn failed_refresh_is_bounded_and_backed_off_while_stale_cache_is_valid() {
        let state = RegistryServerState {
            response: Arc::new(AtomicUsize::new(3)),
            calls: Arc::new(AtomicUsize::new(0)),
            saw_conditions: Arc::new(AtomicBool::new(false)),
        };
        let url = registry_server(state.clone()).await;
        let stale_at = OffsetDateTime::now_utc() - Duration::hours(1);
        let store = Arc::new(MemoryStore::new(Some(snapshot(stale_at))));
        let registry = registry(store, url);

        let started = std::time::Instant::now();
        let first = registry.snapshot().await.expect("stale fallback");
        let second = registry
            .snapshot()
            .await
            .expect("backed-off stale fallback");

        assert!(started.elapsed() < std::time::Duration::from_millis(180));
        assert_eq!(first.fetched_at, stale_at);
        assert_eq!(second.fetched_at, stale_at);
        assert_eq!(state.calls.load(Ordering::SeqCst), 1);
        assert!(registry.ready().await);
        assert_eq!(state.calls.load(Ordering::SeqCst), 1);
    }
}
