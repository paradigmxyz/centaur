use std::{collections::HashSet, sync::Arc};

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
    snapshot: Mutex<Option<RegistrySnapshot>>,
}

impl Registry {
    pub fn new(
        store: Arc<dyn SignerStore>,
        url: &str,
        cache_ttl: Duration,
        max_stale: Duration,
    ) -> anyhow::Result<Self> {
        let url = Url::parse(url).context("parse MPP registry URL")?;
        anyhow::ensure!(
            url.scheme() == "https" && url.host_str().is_some() && url.username().is_empty(),
            "MPP registry URL must be absolute HTTPS without credentials"
        );
        anyhow::ensure!(
            cache_ttl.is_positive() && max_stale >= cache_ttl,
            "MPP registry cache durations are invalid"
        );
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        Ok(Self {
            store,
            client,
            url,
            cache_ttl,
            max_stale,
            snapshot: Mutex::new(None),
        })
    }

    pub async fn route(
        &self,
        host: &str,
        method: &str,
        path_and_query: &str,
    ) -> anyhow::Result<RegisteredRoute> {
        let snapshot = self.snapshot().await?;
        let authority =
            Url::parse(&format!("https://{host}")).context("invalid MPP request authority")?;
        let hostname = authority
            .host_str()
            .context("MPP request authority has no hostname")?;
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
            if service_url.host_str() != Some(hostname) {
                continue;
            }
            if let Some(realm) = &service.realm
                && !realm.eq_ignore_ascii_case(hostname)
            {
                continue;
            }
            for endpoint in &service.endpoints {
                if endpoint.method.eq_ignore_ascii_case(method)
                    && path_matches(&endpoint.path, concrete_path)
                {
                    matches.push(RegisteredRoute {
                        service: service.clone(),
                        endpoint: endpoint.clone(),
                    });
                }
            }
        }
        anyhow::ensure!(
            !matches.is_empty(),
            "request route is not in the MPP registry"
        );
        anyhow::ensure!(
            matches.len() == 1,
            "request route matches multiple MPP registry entries"
        );
        Ok(matches.remove(0))
    }

    pub async fn ready(&self) -> bool {
        self.snapshot()
            .await
            .is_ok_and(|snapshot| OffsetDateTime::now_utc() - snapshot.fetched_at <= self.max_stale)
    }

    async fn snapshot(&self) -> anyhow::Result<RegistrySnapshot> {
        let mut guard = self.snapshot.lock().await;
        if guard.is_none() {
            *guard = self.store.load_registry_cache().await?;
        }
        let now = OffsetDateTime::now_utc();
        if let Some(snapshot) = guard.as_ref()
            && now - snapshot.fetched_at <= self.cache_ttl
        {
            record_snapshot_metrics(snapshot, now);
            return Ok(snapshot.clone());
        }

        match self.refresh(guard.as_ref(), now).await {
            Ok(snapshot) => {
                self.store.save_registry_cache(&snapshot).await?;
                metrics::counter!("centaur_mpp_registry_refresh_total", "outcome" => "success")
                    .increment(1);
                record_snapshot_metrics(&snapshot, now);
                *guard = Some(snapshot.clone());
                Ok(snapshot)
            }
            Err(error) => {
                metrics::counter!("centaur_mpp_registry_refresh_total", "outcome" => "failure")
                    .increment(1);
                if let Some(snapshot) = guard.as_ref()
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
            url.scheme() == "https" && url.host_str().is_some() && url.username().is_empty(),
            "MPP registry service URL must be absolute HTTPS without credentials"
        );
        if let Some(realm) = &service.realm {
            anyhow::ensure!(
                url.host_str()
                    .is_some_and(|host| realm.eq_ignore_ascii_case(host)),
                "MPP registry service realm does not match its URL"
            );
        }
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
                let parameter = expected.starts_with(':')
                    || (expected.starts_with('{') && expected.ends_with('}'));
                (parameter && !actual.is_empty()) || expected == &actual
            })
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
    use super::{has_dot_segments, path_matches};

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
}
