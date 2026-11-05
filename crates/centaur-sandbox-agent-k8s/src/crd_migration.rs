//! Installs the bundled agent-sandbox CRDs, first migrating a cluster still on
//! the `v1alpha1` API (agent-sandbox v0.4.x) to `v1beta1` (v1.x).
//!
//! Upstream requires a stop at a v0.5.x release, whose conversion webhook
//! rewrites stored objects, before v1.x drops `v1alpha1`. Chart users install
//! the latest chart directly, so the chart's pre-upgrade hook runs this one
//! step instead, without a webhook. For the kinds Centaur uses, the only field
//! that changes is the Sandbox's `spec.replicas`, which becomes
//! `spec.operatingMode`; it is carried over explicitly. SandboxClaims, whose
//! conversion is not mechanical, must not exist.
//!
//! The bundled CRDs list `v1alpha1` as an unserved version, so they stay valid
//! while objects are still stored in it: a GitOps server-side dry run of them
//! succeeds before this hook runs, and nothing breaks if they were applied by
//! hand first. During the migration that version is served, without
//! conversion, so every object still stored in it can be read in its old form.
//!
//! Every step is idempotent, so a failed run is repaired by running it again.
//! Writers that only understand `v1alpha1` (the old controller and api-rs) are
//! deleted before any CRD changes; the release recreates them afterwards.

use std::collections::BTreeSet;
use std::time::Duration;

use k8s_openapi::api::apps::v1::Deployment;
use k8s_openapi::api::core::v1::{ConfigMap, PersistentVolumeClaim, Pod, Service};
use k8s_openapi::api::networking::v1::NetworkPolicy;
use k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinition;
use kube::api::{
    ApiResource, DeleteParams, DynamicObject, ListParams, Patch, PatchParams, PostParams,
};
use kube::{Api, Client};
use serde_json::{Value, json};
use tokio::time::{Instant, sleep};

/// The chart's CRDs, the single source for both Helm and this migration.
const BUNDLED_CRDS: [&str; 4] = [
    include_str!("../../../contrib/chart/charts/agent-sandbox/crds/agents.x-k8s.io_sandboxes.yaml"),
    include_str!(
        "../../../contrib/chart/charts/agent-sandbox/crds/extensions.agents.x-k8s.io_sandboxclaims.yaml"
    ),
    include_str!(
        "../../../contrib/chart/charts/agent-sandbox/crds/extensions.agents.x-k8s.io_sandboxtemplates.yaml"
    ),
    include_str!(
        "../../../contrib/chart/charts/agent-sandbox/crds/extensions.agents.x-k8s.io_sandboxwarmpools.yaml"
    ),
];

const OLD_VERSION: &str = "v1alpha1";
const NEW_VERSION: &str = "v1beta1";
const SANDBOX_KIND: &str = "Sandbox";
const SANDBOX_CLAIM_KIND: &str = "SandboxClaim";
/// Records a Sandbox's `spec.replicas` before `v1beta1` storage prunes it.
const REPLICAS_ANNOTATION: &str = "centaur.ai/agent-sandbox-v1alpha1-replicas";
/// Upstream's marker for an object rewritten in `v1beta1` storage form. The
/// value changes on every run, so the write is never a no-op the API server
/// would skip.
const MIGRATED_AT_ANNOTATION: &str = "agents.x-k8s.io/storage-migrated-at";
const FIELD_MANAGER: &str = "centaur-agent-sandbox-migration";
const STOP_TIMEOUT: Duration = Duration::from_secs(300);
const SERVE_TIMEOUT: Duration = Duration::from_secs(60);
const OWNER_GROUPS: [&str; 2] = ["agents.x-k8s.io", "extensions.agents.x-k8s.io"];

#[derive(Debug, thiserror::Error)]
pub enum CrdMigrationError {
    #[error("{context}: {source}")]
    Kube {
        context: String,
        #[source]
        source: Box<kube::Error>,
    },
    #[error("bundled CRD is invalid: {0}")]
    BundledCrd(#[from] serde_yaml::Error),
    #[error("{0}")]
    Refused(String),
}

type Result<T> = std::result::Result<T, CrdMigrationError>;

fn kube_error(context: impl Into<String>) -> impl FnOnce(kube::Error) -> CrdMigrationError {
    let context = context.into();
    move |source| CrdMigrationError::Kube {
        context,
        source: Box::new(source),
    }
}

/// A Deployment whose pods only understand `v1alpha1` and must be gone before
/// the CRDs change.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeploymentRef {
    pub namespace: String,
    pub name: String,
}

impl std::str::FromStr for DeploymentRef {
    type Err = String;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        match value.split_once('/') {
            Some((namespace, name)) if !namespace.is_empty() && !name.is_empty() => Ok(Self {
                namespace: namespace.to_owned(),
                name: name.to_owned(),
            }),
            _ => Err(format!("expected <namespace>/<name>, got {value:?}")),
        }
    }
}

/// Migrate any `v1alpha1` agent-sandbox state, then install the bundled CRDs.
/// `stop` lists the Deployments to delete when a migration is needed.
pub async fn install_agent_sandbox_crds(client: &Client, stop: &[DeploymentRef]) -> Result<()> {
    let crds: Api<CustomResourceDefinition> = Api::all(client.clone());
    let bundled = bundled_crds()?;
    let mut pending = Vec::new();
    for crd in &bundled {
        let name = crd_name(crd);
        if let Some(live) = crds
            .get_opt(name)
            .await
            .map_err(kube_error(format!("get CRD {name}")))?
            && needs_migration(&live)
        {
            pending.push(live);
        }
    }

    if !pending.is_empty() {
        tracing::info!(
            crds = ?pending.iter().map(crd_name).collect::<Vec<_>>(),
            "migrating agent-sandbox CRDs from {OLD_VERSION} to {NEW_VERSION}"
        );
        migrate(client, &crds, &bundled, &pending, stop).await?;
    }

    for crd in &bundled {
        install_crd(&crds, crd).await?;
    }
    tracing::info!("agent-sandbox CRDs are current");
    Ok(())
}

async fn migrate(
    client: &Client,
    crds: &Api<CustomResourceDefinition>,
    bundled: &[CustomResourceDefinition],
    pending: &[CustomResourceDefinition],
    stop: &[DeploymentRef],
) -> Result<()> {
    // Refuse before changing anything.
    for live in pending {
        if stored_versions_include(live, OLD_VERSION) && conversion_strategy(live) == "Webhook" {
            return Err(CrdMigrationError::Refused(format!(
                "CRD {} uses a conversion webhook and still stores {OLD_VERSION} objects; finish \
                 the upstream agent-sandbox v0.5 storage migration first",
                crd_name(live)
            )));
        }
        if live.spec.names.kind == SANDBOX_CLAIM_KIND {
            let claims = list_all(client, &served_resource(live)?).await?;
            if !claims.is_empty() {
                return Err(CrdMigrationError::Refused(format!(
                    "{} SandboxClaims exist; their {OLD_VERSION} fields need upstream's \
                     conversion webhook, so migrate them with the agent-sandbox v0.5 release first",
                    claims.len()
                )));
            }
        }
    }

    for deployment in stop {
        delete_deployment(client, deployment).await?;
    }

    for live in pending {
        if !stored_versions_include(live, OLD_VERSION) {
            continue;
        }
        let bundled = bundled_for(bundled, live);
        install_crd(crds, &serving_old_version(bundled)).await?;
        for version in [OLD_VERSION, NEW_VERSION] {
            wait_until_served(client, &resource(bundled, version)).await?;
        }
        tracing::info!(crd = crd_name(live), "CRD now stores {NEW_VERSION}");
        if bundled.spec.names.kind == SANDBOX_KIND {
            record_replicas(client, bundled).await?;
        }
    }

    let now = jiff::Timestamp::now().to_string();
    let mut sandbox_namespaces = BTreeSet::new();
    for live in pending {
        let resource = resource(bundled_for(bundled, live), NEW_VERSION);
        let objects = list_all(client, &resource).await?;
        for object in &objects {
            let (Some(namespace), Some(name)) = (
                object.metadata.namespace.as_deref(),
                object.metadata.name.as_deref(),
            ) else {
                continue;
            };
            if resource.kind == SANDBOX_KIND {
                sandbox_namespaces.insert(namespace.to_owned());
            }
            Api::<DynamicObject>::namespaced_with(client.clone(), namespace, &resource)
                .patch(
                    name,
                    &PatchParams::default(),
                    &Patch::Merge(storage_rewrite_patch(object, &now)),
                )
                .await
                .map_err(kube_error(format!(
                    "rewrite {} {namespace}/{name}",
                    resource.kind
                )))?;
        }
        tracing::info!(
            kind = resource.kind,
            count = objects.len(),
            "rewrote objects in {NEW_VERSION} storage"
        );
    }

    for namespace in &sandbox_namespaces {
        rewrite_owner_references(client, namespace).await?;
    }

    for live in pending {
        crds.patch_status(
            crd_name(live),
            &PatchParams::default(),
            &Patch::Merge(json!({ "status": { "storedVersions": [NEW_VERSION] } })),
        )
        .await
        .map_err(kube_error(format!(
            "drop {OLD_VERSION} from stored versions of CRD {}",
            crd_name(live)
        )))?;
    }
    Ok(())
}

fn bundled_crds() -> Result<Vec<CustomResourceDefinition>> {
    BUNDLED_CRDS
        .iter()
        .map(|yaml| Ok(serde_yaml::from_str(yaml)?))
        .collect()
}

fn crd_name(crd: &CustomResourceDefinition) -> &str {
    crd.metadata.name.as_deref().unwrap_or_default()
}

fn bundled_for<'a>(
    bundled: &'a [CustomResourceDefinition],
    live: &CustomResourceDefinition,
) -> &'a CustomResourceDefinition {
    bundled
        .iter()
        .find(|crd| crd_name(crd) == crd_name(live))
        .expect("pending CRDs are looked up by bundled name")
}

fn needs_migration(crd: &CustomResourceDefinition) -> bool {
    crd.spec
        .versions
        .iter()
        .any(|v| v.name == OLD_VERSION && v.served)
        || stored_versions_include(crd, OLD_VERSION)
}

fn stored_versions_include(crd: &CustomResourceDefinition, version: &str) -> bool {
    crd.status
        .as_ref()
        .and_then(|status| status.stored_versions.as_ref())
        .is_some_and(|stored| stored.iter().any(|v| v == version))
}

/// The bundled CRD with its `v1alpha1` stub served. The stub's schema keeps
/// unknown fields, so an object still stored in `v1alpha1` reads back intact.
fn serving_old_version(bundled: &CustomResourceDefinition) -> CustomResourceDefinition {
    let mut crd = bundled.clone();
    for version in &mut crd.spec.versions {
        if version.name == OLD_VERSION {
            version.served = true;
        }
    }
    crd
}

fn conversion_strategy(crd: &CustomResourceDefinition) -> &str {
    crd.spec
        .conversion
        .as_ref()
        .map_or("None", |conversion| conversion.strategy.as_str())
}

fn resource(crd: &CustomResourceDefinition, version: &str) -> ApiResource {
    let group = crd.spec.group.clone();
    ApiResource {
        api_version: format!("{group}/{version}"),
        group,
        version: version.to_owned(),
        kind: crd.spec.names.kind.clone(),
        plural: crd.spec.names.plural.clone(),
    }
}

fn served_resource(crd: &CustomResourceDefinition) -> Result<ApiResource> {
    let version = crd.spec.versions.iter().find(|v| v.served).ok_or_else(|| {
        CrdMigrationError::Refused(format!("CRD {} serves no version", crd_name(crd)))
    })?;
    Ok(resource(crd, &version.name))
}

async fn list_all(client: &Client, resource: &ApiResource) -> Result<Vec<DynamicObject>> {
    Api::<DynamicObject>::all_with(client.clone(), resource)
        .list(&ListParams::default())
        .await
        .map(|list| list.items)
        .map_err(kube_error(format!("list {}", resource.plural)))
}

/// A version a CRD update just started serving answers 404 until the API
/// server's handler for it is up, which takes a moment.
async fn wait_until_served(client: &Client, resource: &ApiResource) -> Result<()> {
    let api = Api::<DynamicObject>::all_with(client.clone(), resource);
    let deadline = Instant::now() + SERVE_TIMEOUT;
    loop {
        match api.list(&ListParams::default().limit(1)).await {
            Ok(_) => return Ok(()),
            Err(kube::Error::Api(status)) if status.code == 404 && Instant::now() < deadline => {
                sleep(Duration::from_millis(500)).await;
            }
            Err(error) => {
                return Err(kube_error(format!(
                    "wait for {} to be served",
                    resource.api_version
                ))(error));
            }
        }
    }
}

async fn delete_deployment(client: &Client, deployment: &DeploymentRef) -> Result<()> {
    let deployments: Api<Deployment> = Api::namespaced(client.clone(), &deployment.namespace);
    let label = format!("deployment {}/{}", deployment.namespace, deployment.name);
    // Foreground deletion keeps the Deployment until its pods are gone, so
    // its absence means no old pod is still writing.
    match deployments
        .delete(&deployment.name, &DeleteParams::foreground())
        .await
    {
        Ok(_) => {}
        Err(kube::Error::Api(status)) if status.is_not_found() => return Ok(()),
        Err(error) => return Err(kube_error(format!("delete {label}"))(error)),
    }
    tracing::info!(
        deployment = label,
        "deleting until the release recreates it"
    );
    let deadline = Instant::now() + STOP_TIMEOUT;
    while deployments
        .get_opt(&deployment.name)
        .await
        .map_err(kube_error(format!("get {label}")))?
        .is_some()
    {
        if Instant::now() >= deadline {
            return Err(CrdMigrationError::Refused(format!(
                "{label} still exists {}s after deletion",
                STOP_TIMEOUT.as_secs()
            )));
        }
        sleep(Duration::from_secs(2)).await;
    }
    Ok(())
}

async fn record_replicas(client: &Client, crd: &CustomResourceDefinition) -> Result<()> {
    let resource = resource(crd, OLD_VERSION);
    let mut recorded = 0;
    for sandbox in &list_all(client, &resource).await? {
        let (Some(namespace), Some(name), Some(replicas)) = (
            sandbox.metadata.namespace.as_deref(),
            sandbox.metadata.name.as_deref(),
            replicas_to_record(sandbox),
        ) else {
            continue;
        };
        Api::<DynamicObject>::namespaced_with(client.clone(), namespace, &resource)
            .patch(
                name,
                &PatchParams::default(),
                &Patch::Merge(json!({
                    "metadata": { "annotations": { REPLICAS_ANNOTATION: replicas.to_string() } },
                })),
            )
            .await
            .map_err(kube_error(format!(
                "record replicas of Sandbox {namespace}/{name}"
            )))?;
        recorded += 1;
    }
    tracing::info!(count = recorded, "recorded Sandbox replicas");
    Ok(())
}

/// The `spec.replicas` of a Sandbox read through `v1alpha1`, unless an earlier
/// run already recorded it (the recording write may have moved the object to
/// `v1beta1` storage, dropping the field) or already rewrote the object.
fn replicas_to_record(sandbox: &DynamicObject) -> Option<i64> {
    let annotations = sandbox.metadata.annotations.as_ref();
    if annotations.is_some_and(|a| {
        a.contains_key(REPLICAS_ANNOTATION) || a.contains_key(MIGRATED_AT_ANNOTATION)
    }) {
        return None;
    }
    Some(sandbox.data["spec"]["replicas"].as_i64().unwrap_or(1))
}

/// A write that changes the object, so the API server stores it again in the
/// current storage version, carrying a recorded replica count over to
/// `spec.operatingMode`.
fn storage_rewrite_patch(object: &DynamicObject, now: &str) -> Value {
    let mut patch = json!({ "metadata": { "annotations": { MIGRATED_AT_ANNOTATION: now } } });
    if let Some(replicas) = object
        .metadata
        .annotations
        .as_ref()
        .and_then(|annotations| annotations.get(REPLICAS_ANNOTATION))
    {
        let mode = if replicas.trim() == "0" {
            "Suspended"
        } else {
            "Running"
        };
        patch["metadata"]["annotations"][REPLICAS_ANNOTATION] = Value::Null;
        patch["spec"] = json!({ "operatingMode": mode });
    }
    patch
}

/// Garbage collection resolves an owner through the exact `apiVersion` in the
/// reference, so children pointing at `v1alpha1` would outlive their Sandbox
/// once that version is gone.
async fn rewrite_owner_references(client: &Client, namespace: &str) -> Result<()> {
    for resource in [
        ApiResource::erase::<Pod>(&()),
        ApiResource::erase::<Service>(&()),
        ApiResource::erase::<ConfigMap>(&()),
        ApiResource::erase::<PersistentVolumeClaim>(&()),
        ApiResource::erase::<NetworkPolicy>(&()),
    ] {
        let api = Api::<DynamicObject>::namespaced_with(client.clone(), namespace, &resource);
        let objects = api
            .list(&ListParams::default())
            .await
            .map_err(kube_error(format!(
                "list {} in {namespace}",
                resource.plural
            )))?;
        let mut rewritten = 0;
        for object in objects.items {
            let Some(name) = object.metadata.name.as_deref() else {
                continue;
            };
            let Some(patch) = owner_reference_patch(&object) else {
                continue;
            };
            // The test op fails the patch if the references changed since the
            // read, instead of overwriting a concurrent update.
            let patch = serde_json::from_value(patch).expect("owner reference patch is valid");
            api.patch(name, &PatchParams::default(), &Patch::Json::<()>(patch))
                .await
                .map_err(kube_error(format!(
                    "rewrite owner references of {} {namespace}/{name}",
                    resource.kind
                )))?;
            rewritten += 1;
        }
        if rewritten > 0 {
            tracing::info!(
                kind = resource.kind,
                namespace,
                count = rewritten,
                "rewrote owner references to {NEW_VERSION}"
            );
        }
    }
    Ok(())
}

fn owner_reference_patch(object: &DynamicObject) -> Option<Value> {
    let current = object.metadata.owner_references.as_ref()?;
    let mut changed = false;
    let updated: Vec<_> = current
        .iter()
        .cloned()
        .map(|mut owner| {
            if let Some((group, version)) = owner.api_version.split_once('/')
                && version == OLD_VERSION
                && OWNER_GROUPS.contains(&group)
            {
                owner.api_version = format!("{group}/{NEW_VERSION}");
                changed = true;
            }
            owner
        })
        .collect();
    changed.then(|| {
        json!([
            { "op": "test", "path": "/metadata/ownerReferences", "value": current },
            { "op": "replace", "path": "/metadata/ownerReferences", "value": updated },
        ])
    })
}

/// Create the bundled CRD or replace the live spec with it, keeping the live
/// object's labels and annotations (for example a GitOps tracking label).
async fn install_crd(
    crds: &Api<CustomResourceDefinition>,
    bundled: &CustomResourceDefinition,
) -> Result<()> {
    let name = crd_name(bundled);
    let params = PostParams {
        field_manager: Some(FIELD_MANAGER.to_owned()),
        ..PostParams::default()
    };
    let Some(mut live) = crds
        .get_opt(name)
        .await
        .map_err(kube_error(format!("get CRD {name}")))?
    else {
        crds.create(&params, bundled)
            .await
            .map_err(kube_error(format!("create CRD {name}")))?;
        return Ok(());
    };
    live.spec = bundled.spec.clone();
    live.metadata
        .annotations
        .get_or_insert_default()
        .extend(bundled.metadata.annotations.clone().unwrap_or_default());
    crds.replace(name, &params, &live)
        .await
        .map_err(kube_error(format!("update CRD {name}")))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A Sandbox CRD as agent-sandbox v0.4.x installed it.
    fn live_v1alpha1_sandbox_crd() -> CustomResourceDefinition {
        let mut crd = bundled_crds().unwrap().remove(0);
        crd.spec.versions.retain(|v| v.name == NEW_VERSION);
        crd.spec.versions[0].name = OLD_VERSION.to_owned();
        crd
    }

    fn object(annotations: &[(&str, &str)], owners: Value) -> DynamicObject {
        serde_json::from_value(json!({
            "apiVersion": "v1",
            "kind": "Pod",
            "metadata": {
                "name": "asbx-test",
                "namespace": "centaur",
                "annotations": annotations.iter().copied().collect::<std::collections::BTreeMap<_, _>>(),
                "ownerReferences": owners,
            },
        }))
        .unwrap()
    }

    #[test]
    fn bundled_crds_serve_only_v1beta1_and_keep_v1alpha1_unserved() {
        let crds = bundled_crds().unwrap();
        assert_eq!(crds.len(), 4);
        for crd in &crds {
            let versions: Vec<_> = crd
                .spec
                .versions
                .iter()
                .map(|v| (v.name.as_str(), v.served, v.storage))
                .collect();
            assert_eq!(
                versions,
                [(NEW_VERSION, true, true), (OLD_VERSION, false, false)],
                "{}",
                crd_name(crd)
            );
            assert!(!needs_migration(crd), "{}", crd_name(crd));
        }
        assert!(needs_migration(&live_v1alpha1_sandbox_crd()));
    }

    #[test]
    fn replicas_are_recorded_once_from_the_v1alpha1_form() {
        let mut paused = object(&[], json!([]));
        paused.data = json!({ "spec": { "replicas": 0 } });
        assert_eq!(replicas_to_record(&paused), Some(0));
        // Unset replicas meant one running pod in v1alpha1.
        assert_eq!(replicas_to_record(&object(&[], json!([]))), Some(1));
        // A re-run must not overwrite a recorded value with a pruned form.
        assert_eq!(
            replicas_to_record(&object(&[(REPLICAS_ANNOTATION, "0")], json!([]))),
            None
        );
        assert_eq!(
            replicas_to_record(&object(&[(MIGRATED_AT_ANNOTATION, "t")], json!([]))),
            None
        );
    }

    #[test]
    fn storage_rewrite_carries_recorded_replicas_to_operating_mode() {
        let paused = storage_rewrite_patch(&object(&[(REPLICAS_ANNOTATION, "0")], json!([])), "t");
        assert_eq!(paused["spec"]["operatingMode"], "Suspended");
        assert_eq!(
            paused["metadata"]["annotations"][REPLICAS_ANNOTATION],
            Value::Null
        );
        assert_eq!(
            paused["metadata"]["annotations"][MIGRATED_AT_ANNOTATION],
            "t"
        );

        let running = storage_rewrite_patch(&object(&[(REPLICAS_ANNOTATION, "1")], json!([])), "t");
        assert_eq!(running["spec"]["operatingMode"], "Running");

        // Already rewritten by an earlier run: keep its mode, still rewrite.
        let rewritten = storage_rewrite_patch(&object(&[], json!([])), "t");
        assert!(rewritten.get("spec").is_none());
        assert_eq!(
            rewritten["metadata"]["annotations"][MIGRATED_AT_ANNOTATION],
            "t"
        );
    }

    #[test]
    fn owner_reference_patch_moves_only_agent_sandbox_owners_to_v1beta1() {
        let sandbox = json!({
            "apiVersion": "agents.x-k8s.io/v1alpha1",
            "kind": "Sandbox",
            "name": "asbx-test",
            "uid": "uid-1",
            "controller": true,
        });
        let other = json!({
            "apiVersion": "apps/v1",
            "kind": "ReplicaSet",
            "name": "rs",
            "uid": "uid-2",
        });
        let patch = owner_reference_patch(&object(&[], json!([sandbox, other]))).unwrap();

        assert_eq!(patch[0]["op"], "test");
        assert_eq!(
            patch[0]["value"][0]["apiVersion"],
            "agents.x-k8s.io/v1alpha1"
        );
        let updated = &patch[1]["value"];
        assert_eq!(updated[0]["apiVersion"], "agents.x-k8s.io/v1beta1");
        assert_eq!(updated[0]["uid"], "uid-1");
        assert_eq!(updated[0]["controller"], true);
        assert_eq!(updated[1], other);

        let current = json!([{
            "apiVersion": "agents.x-k8s.io/v1beta1",
            "kind": "Sandbox",
            "name": "asbx-test",
            "uid": "uid-1",
        }]);
        assert!(owner_reference_patch(&object(&[], current)).is_none());
    }
}
