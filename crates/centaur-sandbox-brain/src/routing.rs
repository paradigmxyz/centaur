use std::{collections::BTreeMap, sync::Arc};

use async_trait::async_trait;
use centaur_sandbox_core::{
    ObservedSandbox, SandboxBackend, SandboxError, SandboxHandle, SandboxId, SandboxIo,
    SandboxResult, SandboxSpec, SandboxStatus,
};

use crate::{BrainSandboxBackend, HARNESS_LABEL, NANOCODE_HARNESS, backend::is_brain_sandbox_id};

/// Sends `nanocode` sandboxes to the brain and everything else to the real
/// (Kubernetes) backend.
///
/// Every id-based call routes by id prefix, so the rest of api-rs can treat
/// both kinds of sandbox identically.
pub struct RoutingSandboxBackend {
    primary: Arc<dyn SandboxBackend>,
    brain: Option<Arc<dyn SandboxBackend>>,
}

impl RoutingSandboxBackend {
    pub fn new(primary: Arc<dyn SandboxBackend>, brain: Option<Arc<dyn SandboxBackend>>) -> Self {
        Self { primary, brain }
    }

    /// Wrap `primary` and point `nanocode` sandboxes at the brain at `url`.
    pub fn with_brain_url(
        primary: Arc<dyn SandboxBackend>,
        url: impl AsRef<str>,
    ) -> SandboxResult<Self> {
        let brain = Arc::new(BrainSandboxBackend::new(url)?);
        Ok(Self::new(primary, Some(brain)))
    }

    pub fn brain_enabled(&self) -> bool {
        self.brain.is_some()
    }

    fn backend_for(&self, id: &str) -> &Arc<dyn SandboxBackend> {
        match &self.brain {
            Some(brain) if is_brain_sandbox_id(id) => brain,
            _ => &self.primary,
        }
    }
}

#[async_trait]
impl SandboxBackend for RoutingSandboxBackend {
    fn name(&self) -> &'static str {
        "routing"
    }

    async fn create(&self, spec: SandboxSpec) -> SandboxResult<SandboxHandle> {
        let is_nanocode =
            spec.labels.get(HARNESS_LABEL).map(String::as_str) == Some(NANOCODE_HARNESS);
        if is_nanocode {
            let Some(brain) = &self.brain else {
                return Err(SandboxError::InvalidSpec(format!(
                    "harness {NANOCODE_HARNESS} is not enabled"
                )));
            };
            return brain.create(spec).await;
        }
        self.primary.create(spec).await
    }

    async fn open_io(&self, id: &SandboxId) -> SandboxResult<SandboxIo> {
        self.backend_for(id.as_str()).open_io(id).await
    }

    async fn read_output_since(
        &self,
        id: &SandboxId,
        since: Option<std::time::SystemTime>,
    ) -> SandboxResult<Vec<String>> {
        self.backend_for(id.as_str())
            .read_output_since(id, since)
            .await
    }

    async fn workload_address(&self, id: &SandboxId) -> SandboxResult<String> {
        self.backend_for(id.as_str()).workload_address(id).await
    }

    async fn status(&self, id: &SandboxId) -> SandboxResult<SandboxStatus> {
        self.backend_for(id.as_str()).status(id).await
    }

    async fn observe(&self, id: &SandboxId) -> SandboxResult<ObservedSandbox> {
        self.backend_for(id.as_str()).observe(id).await
    }

    async fn list_observed(&self) -> SandboxResult<Vec<ObservedSandbox>> {
        // The brain's cheap logical handles are not reaped, so only the real
        // backend enumerates its sandboxes.
        self.primary.list_observed().await
    }

    async fn stop(&self, id: &SandboxId) -> SandboxResult<()> {
        self.backend_for(id.as_str()).stop(id).await
    }

    async fn assign_iron_control_proxy_principal(
        &self,
        id: &SandboxId,
        principal_id: &str,
        requester_principal_id: Option<&str>,
        labels: &BTreeMap<String, String>,
    ) -> SandboxResult<()> {
        self.backend_for(id.as_str())
            .assign_iron_control_proxy_principal(id, principal_id, requester_principal_id, labels)
            .await
    }

    async fn ensure_iron_control_proxy_resources(
        &self,
        id: &SandboxId,
        principal_id: &str,
        requester_principal_id: Option<&str>,
        labels: &BTreeMap<String, String>,
    ) -> SandboxResult<()> {
        self.backend_for(id.as_str())
            .ensure_iron_control_proxy_resources(id, principal_id, requester_principal_id, labels)
            .await
    }

    async fn enable_observability(&self, id: &SandboxId) -> SandboxResult<()> {
        self.backend_for(id.as_str()).enable_observability(id).await
    }

    async fn pause(&self, id: &SandboxId) -> SandboxResult<()> {
        self.backend_for(id.as_str()).pause(id).await
    }

    async fn resume(&self, id: &SandboxId) -> SandboxResult<()> {
        self.backend_for(id.as_str()).resume(id).await
    }

    async fn reap_orphan_iron_proxy_resources(
        &self,
        grace: std::time::Duration,
    ) -> SandboxResult<BTreeMap<String, u32>> {
        self.primary.reap_orphan_iron_proxy_resources(grace).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use centaur_sandbox_core::{SandboxIo, SandboxSpec};
    use std::sync::Mutex;

    /// Records which backend received each call and returns prefixed ids.
    struct RecordingBackend {
        prefix: &'static str,
        calls: Mutex<Vec<String>>,
    }

    impl RecordingBackend {
        fn new(prefix: &'static str) -> Arc<Self> {
            Arc::new(Self {
                prefix,
                calls: Mutex::new(Vec::new()),
            })
        }

        fn record(&self, call: &str) {
            self.calls.lock().unwrap().push(call.to_owned());
        }

        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl SandboxBackend for RecordingBackend {
        fn name(&self) -> &'static str {
            self.prefix
        }

        async fn create(&self, _spec: SandboxSpec) -> SandboxResult<SandboxHandle> {
            self.record("create");
            Ok(SandboxHandle::new(
                format!("{}-new", self.prefix),
                self.prefix,
            ))
        }

        async fn open_io(&self, _id: &SandboxId) -> SandboxResult<SandboxIo> {
            self.record("open_io");
            Err(SandboxError::backend("not needed"))
        }

        async fn status(&self, _id: &SandboxId) -> SandboxResult<SandboxStatus> {
            self.record("status");
            Ok(SandboxStatus::Running)
        }

        async fn observe(&self, id: &SandboxId) -> SandboxResult<ObservedSandbox> {
            self.record("observe");
            Ok(ObservedSandbox::new(
                id.as_str(),
                self.prefix,
                SandboxStatus::Running,
            ))
        }

        async fn list_observed(&self) -> SandboxResult<Vec<ObservedSandbox>> {
            self.record("list_observed");
            Ok(Vec::new())
        }

        async fn stop(&self, _id: &SandboxId) -> SandboxResult<()> {
            self.record("stop");
            Ok(())
        }

        async fn pause(&self, _id: &SandboxId) -> SandboxResult<()> {
            self.record("pause");
            Ok(())
        }

        async fn resume(&self, _id: &SandboxId) -> SandboxResult<()> {
            self.record("resume");
            Ok(())
        }

        async fn assign_iron_control_proxy_principal(
            &self,
            _id: &SandboxId,
            _principal_id: &str,
            _requester_principal_id: Option<&str>,
            _labels: &BTreeMap<String, String>,
        ) -> SandboxResult<()> {
            self.record("assign_iron_control_proxy_principal");
            Ok(())
        }

        async fn enable_observability(&self, _id: &SandboxId) -> SandboxResult<()> {
            self.record("enable_observability");
            Ok(())
        }
    }

    fn spec_with_harness(harness: &str) -> SandboxSpec {
        SandboxSpec::new("ignored").label(HARNESS_LABEL, harness)
    }

    #[tokio::test]
    async fn create_routes_nanocode_to_brain_and_others_to_primary() {
        let primary = RecordingBackend::new("primary");
        let brain = RecordingBackend::new("brain");
        let routing = RoutingSandboxBackend::new(
            primary.clone() as Arc<dyn SandboxBackend>,
            Some(brain.clone() as Arc<dyn SandboxBackend>),
        );

        let handle = routing.create(spec_with_harness("nanocode")).await.unwrap();
        assert_eq!(handle.backend, "brain");
        let handle = routing.create(spec_with_harness("codex")).await.unwrap();
        assert_eq!(handle.backend, "primary");

        assert_eq!(brain.calls(), vec!["create"]);
        assert_eq!(primary.calls(), vec!["create"]);
    }

    #[tokio::test]
    async fn id_prefixed_calls_route_to_brain() {
        let primary = RecordingBackend::new("primary");
        let brain = RecordingBackend::new("brain");
        let routing = RoutingSandboxBackend::new(
            primary.clone() as Arc<dyn SandboxBackend>,
            Some(brain.clone() as Arc<dyn SandboxBackend>),
        );

        let brain_id = SandboxId::new("brain-abc");
        let sandbox_id = SandboxId::new("sbx-abc");

        routing.status(&brain_id).await.unwrap();
        routing.stop(&brain_id).await.unwrap();
        routing.observe(&brain_id).await.unwrap();
        routing.status(&sandbox_id).await.unwrap();
        routing.stop(&sandbox_id).await.unwrap();
        routing.observe(&sandbox_id).await.unwrap();
        routing.list_observed().await.unwrap();

        assert_eq!(brain.calls(), vec!["status", "stop", "observe"]);
        assert_eq!(
            primary.calls(),
            vec!["status", "stop", "observe", "list_observed"]
        );
    }

    #[tokio::test]
    async fn nanocode_create_without_brain_is_rejected() {
        let primary = RecordingBackend::new("primary");
        let routing = RoutingSandboxBackend::new(primary.clone() as Arc<dyn SandboxBackend>, None);

        let error = routing
            .create(spec_with_harness("nanocode"))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("nanocode is not enabled"));
        assert!(primary.calls().is_empty());

        let handle = routing.create(spec_with_harness("codex")).await.unwrap();
        assert_eq!(handle.backend, "primary");
    }

    #[tokio::test]
    async fn proxy_methods_route_by_id_prefix() {
        let primary = RecordingBackend::new("primary");
        let brain = RecordingBackend::new("brain");
        let routing = RoutingSandboxBackend::new(
            primary.clone() as Arc<dyn SandboxBackend>,
            Some(brain.clone() as Arc<dyn SandboxBackend>),
        );

        let id = SandboxId::new("brain-abc");
        let labels = BTreeMap::new();
        routing
            .assign_iron_control_proxy_principal(&id, "prn_x", Some("prn_y"), &labels)
            .await
            .unwrap();
        routing
            .ensure_iron_control_proxy_resources(&id, "prn_x", None, &labels)
            .await
            .unwrap();
        routing.enable_observability(&id).await.unwrap();
        routing.pause(&id).await.unwrap();
        routing.resume(&id).await.unwrap();

        // The brain backend receives the id-prefixed calls; the primary never
        // sees them.
        assert_eq!(
            brain.calls(),
            vec![
                "assign_iron_control_proxy_principal",
                "enable_observability",
                "pause",
                "resume"
            ]
        );
        assert!(primary.calls().is_empty());
    }
}
