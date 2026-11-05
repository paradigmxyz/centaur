# Migrating agent-sandbox from v1alpha1 to v1beta1

The chart bundles the upstream agent-sandbox controller and its CRDs. Chart
releases up to and including the v0.4.x controller use the `v1alpha1` API;
newer releases ship the v1.x controller, which serves only `v1beta1`, and
api-rs now speaks only `v1beta1` (a Sandbox's `spec.replicas` became
`spec.operatingMode: Running | Suspended`).

Upstream only supports this upgrade through an intermediate v0.5.x release and
its conversion webhook. The chart does it in one step instead: a
pre-install/pre-upgrade hook migrates the cluster before anything else in the
release changes. A fresh install needs no migration; the same hook just
installs the CRDs.

## What the hook does

The hook is a Job named `<release-fullname>-agent-sandbox-crds` (label
`app.kubernetes.io/component=agent-sandbox-crds`) that runs
`centaur-api-server install-agent-sandbox-crds` from the api-rs image. Argo CD
runs it as a PreSync hook.

When the cluster still serves or stores `v1alpha1`, it:

1. refuses to start if any SandboxClaims exist, or if a CRD still stores
   `v1alpha1` behind upstream's conversion webhook;
2. deletes the api-rs and agent-sandbox controller Deployments, which only
   understand `v1alpha1`, and waits for their pods to exit (the release
   recreates both);
3. installs the bundled CRDs with `v1alpha1` temporarily served, switching
   storage to `v1beta1`, and records each Sandbox's `spec.replicas` in the
   `centaur.ai/agent-sandbox-v1alpha1-replicas` annotation;
4. rewrites every object in `v1beta1` form, setting `operatingMode` from the
   recorded value (`0` becomes `Suspended`, anything else `Running`) and
   stamping `agents.x-k8s.io/storage-migrated-at`;
5. rewrites owner references on Sandbox children (Pods, Services,
   ConfigMaps, PersistentVolumeClaims, NetworkPolicies) from `v1alpha1` to
   `v1beta1`, so garbage collection keeps working;
6. sets each CRD's `status.storedVersions` to `["v1beta1"]` and installs the
   bundled CRDs, which list `v1alpha1` only as an unserved version.

Every step is idempotent: a run that fails or is interrupted is repaired by
running the upgrade again. On an already-migrated cluster the hook only
refreshes the CRDs, which is also how CRD updates reach Helm installs (Helm
never re-applies `crds/` on upgrade).

Running sandbox pods are not restarted; the new controller adopts them.
Sandbox operations are unavailable from step 2 until the release recreates
api-rs, usually well under a minute.

## Before you start

Run the read-only preflight check against the cluster first. It reports
whether the upgrade will migrate it, finds anything that would make the hook
refuse (SandboxClaims, an unfinished upstream webhook migration), and warns
about writers the hook does not stop, such as another agent-sandbox controller
or another release's api-rs sharing the cluster-wide CRDs:

```bash
contrib/scripts/agent-sandbox-preflight.sh --context <ctx> \
  [--namespace <namespace>] [--controller-namespace <controller-namespace>]
```

It exits 0 when the cluster is ready (review any warnings) and 1 when the
upgrade would be refused. The points below are what it checks, plus the ones
only you can decide.

- **The migration is one-way.** Once the hook has run, the previous chart
  cannot be restored: its CRDs are rejected by the API server, and its
  `v1alpha1`-only api-rs cannot find any Sandbox. Fix problems forward by
  re-running the upgrade. See [Rollback](#rollback).
- **`agentSandbox.enabled: false`** means you manage agent-sandbox yourself.
  No hook runs; upgrade your own install to v1.x before upgrading the chart.
- **No SandboxClaims may exist.** The chart does not use them; the hook refuses
  to run if it finds any:

  ```bash
  kubectl --context <ctx> get sandboxclaims.extensions.agents.x-k8s.io -A --no-headers | wc -l
  ```

- **Take a backup and a baseline** so you can compare afterwards:

  ```bash
  kubectl --context <ctx> get sandboxes.agents.x-k8s.io -A -o yaml > sandboxes-before.yaml
  kubectl --context <ctx> get sandboxes.agents.x-k8s.io -A \
    -o custom-columns=NS:.metadata.namespace,NAME:.metadata.name,REPLICAS:.spec.replicas \
    --no-headers > before.txt
  ```

## Running the migration

### Helm

Upgrade as usual, with a timeout that covers the hook, and without automatic
rollback:

```bash
helm --kube-context <ctx> upgrade <release> <chart> -n <namespace> \
  -f <values>.yaml --wait --timeout 15m
```

- Do not pass `--atomic` (Helm 3) or `--rollback-on-failure` (Helm 4), and do
  not run `helm rollback` afterwards.
- If the upgrade fails, read the hook log (see
  [Troubleshooting](#troubleshooting)), fix the cause, and run the same
  `helm upgrade` again. A failed hook Job is kept for inspection.

### Argo CD

Sync the new chart revision as usual. Server-side and client-side apply both
work.

- The hook's logs disappear when it succeeds (its delete policy is
  `hook-succeeded`), so follow them during the sync if you want them:

  ```bash
  kubectl --context <ctx> -n <namespace> logs -f \
    -l app.kubernetes.io/component=agent-sandbox-crds --tail=-1
  ```

- **Move the chart and the image tags together.** The hook runs from the api-rs
  image, and the new api-rs needs the migrated CRDs:
  - New images with the old chart: api-rs crash-loops (see
    [api-rs crash-loops with a 404](#api-rs-crash-loops-with-a-404)) until the
    new chart arrives and its hook runs. Nothing is changed in the meantime.
  - New chart with the old images: the hook fails immediately because the old
    binary does not know the command, and nothing is changed. The migration
    runs when the images follow.

  If the chart revision and the image tags reach the Application through
  different paths (for example, image tags from a values file on a branch and
  the chart revision from a parent app-of-apps), they can arrive minutes
  apart. Either temporarily disable automated sync on the Application and sync
  it once both have landed, or accept a short crash loop. See also
  [A parent app-of-apps sync stalls](#a-parent-app-of-apps-sync-stalls).

- Automated sync does not retry a failed sync of the same revision unless the
  Application has a `syncPolicy.retry`. Without one, a failed hook needs a
  manual sync once the cause is fixed.

### Flux and other Helm-based controllers

Hooks run as with Helm. Disable rollback remediation for this upgrade (in Flux,
`spec.upgrade.remediation` on the `HelmRelease`).

### Tools that do not run Helm hooks

`helm template | kubectl apply` and similar flows are not supported: the hook
becomes an ordinary Job with no ordering guarantee. If you must, apply the
rendered hook Job (and its ServiceAccount, ClusterRole, ClusterRoleBinding, and
NetworkPolicy) first, wait for it to complete, then apply the rest.

## Verifying the migration

```bash
k() { kubectl --context <ctx> "$@"; }

# v1beta1 served and stored; v1alpha1 listed but unserved. Every row should
# read: v1beta1,v1alpha1   true,false   [v1beta1]
k get crd -o custom-columns=NAME:.metadata.name,VERSIONS:.spec.versions[*].name,SERVED:.spec.versions[*].served,STORED:.status.storedVersions \
  | grep agents.x-k8s.io

# Controller on v1.x and api-rs ready
k -n <controller-namespace> get deploy agent-sandbox-controller \
  -o jsonpath='{.spec.template.spec.containers[0].image} ready={.status.readyReplicas}{"\n"}'
k -n <namespace> get deploy -l app.kubernetes.io/component=api-rs \
  -o custom-columns=NAME:.metadata.name,IMAGE:.spec.template.spec.containers[0].image,READY:.status.readyReplicas

# Each Sandbox paused before (replicas 0) is Suspended; every other is Running
k get sandboxes.v1beta1.agents.x-k8s.io -A \
  -o custom-columns=NS:.metadata.namespace,NAME:.metadata.name,MODE:.spec.operatingMode \
  --no-headers > after.txt
join <(awk '{print $1"/"$2, $3}' before.txt | sort) <(awk '{print $1"/"$2, $3}' after.txt | sort) -a1 \
  | awk '{ok = ($2=="0" && $3=="Suspended") || ($2!="0" && $3=="Running"); if (!ok) print "MISMATCH", $0; n++} END {print n, "checked"}'

# No leftover replica annotations and no v1alpha1 owner references (both 0)
k get sandboxes.v1beta1.agents.x-k8s.io -A -o json \
  | jq '[.items[] | select(.metadata.annotations["centaur.ai/agent-sandbox-v1alpha1-replicas"])] | length'
k -n <namespace> get pods,svc,netpol,cm,pvc -o json \
  | jq '[.items[].metadata.ownerReferences[]? | select(.kind=="Sandbox" and (.apiVersion|endswith("/v1alpha1")))] | length'

# The hook's cluster-scoped RBAC was removed (no output)
k get clusterrole,clusterrolebinding -o name | grep agent-sandbox-crds
```

`<controller-namespace>` is `agentSandbox.namespace.name` (default
`agent-sandbox-system`).

Finally, send a message to a conversation whose sandbox was paused before the
upgrade. It should answer, and its `session.sandbox_ready` event should report
`sandbox_ready_source` `resumed` for the same sandbox ID:

```sql
select payload->>'sandbox_id', payload->>'sandbox_ready_source'
from session_events
where thread_key = '<thread-key>' and event_type = 'session.sandbox_ready'
order by event_id desc limit 1;
```

## Troubleshooting

Start with the hook's log. While the Job exists (it is kept when it fails):

```bash
kubectl --context <ctx> -n <namespace> logs -l app.kubernetes.io/component=agent-sandbox-crds --tail=-1
kubectl --context <ctx> -n <namespace> describe job -l app.kubernetes.io/component=agent-sandbox-crds
```

Under Argo CD, the hook's result is also recorded on the Application:

```bash
kubectl --context <ctx> -n argocd get app <app> -o jsonpath='{.status.operationState.message}{"\n"}{range .status.operationState.syncResult.resources[?(@.hookType=="PreSync")]}{.kind}/{.name} {.hookPhase} {.message}{"\n"}{end}'
```

A successful migration logs, in order: `deleting until the release recreates
it`, `CRD now stores v1beta1` and `recorded Sandbox replicas`, `rewrote objects
in v1beta1 storage`, `rewrote owner references to v1beta1`, and `agent-sandbox
CRDs are current`.

### `Unsuccessful data error parse: 404 page not found` in the hook log

Expected. A version a CRD has just started serving answers 404 for a moment;
the hook waits and retries. Only a following `Error:` line indicates a
failure.

### `N SandboxClaims exist`

The hook refused before changing anything. SandboxClaim's `v1alpha1` fields do
not convert mechanically. Delete the claims if they are unused, or migrate them
with upstream's v0.5.x release and migration guide first, then re-run the
upgrade.

### `uses a conversion webhook and still stores v1alpha1 objects`

The cluster is partway through upstream's v0.5.x migration. Finish it (rewrite
the objects and prune `status.storedVersions`, per upstream's guide), then
re-run the upgrade. Nothing was changed.

### `still exists 300s after deletion`

A Deployment the hook deleted did not go away. Look for its pods stuck
`Terminating` and for finalizers on the Deployment or its ReplicaSets:

```bash
kubectl --context <ctx> -n <namespace> get deploy,rs,pods -l app.kubernetes.io/component=api-rs
kubectl --context <ctx> -n <controller-namespace> get deploy,rs,pods
```

Resolve the blocker and re-run the upgrade.

### `Forbidden` or connection errors talking to the Kubernetes API

The hook runs with its own ServiceAccount and egress NetworkPolicy, both
created as hooks before the Job. With `networkPolicy.enabled`, the policy
allows TCP 443 and `networkPolicy.apiServerPort` (default 6443). If your API
server's endpoint uses a different port, set `networkPolicy.apiServerPort` to
it; api-rs needs the same value.

### api-rs crash-loops with a 404

The log ends with:

```text
Error: Sandbox(NotFound("list sandboxes for pod configuration reconciliation"))
```

api-rs asked for a Sandbox API version the cluster does not serve. Compare the
api-rs image with the CRD:

```bash
kubectl --context <ctx> -n <namespace> get pods -l app.kubernetes.io/component=api-rs \
  -o custom-columns=NAME:.metadata.name,IMAGE:.spec.containers[0].image
kubectl --context <ctx> get crd sandboxes.agents.x-k8s.io \
  -o jsonpath='{range .spec.versions[*]}{.name} served={.served}{"\n"}{end}stored={.status.storedVersions}{"\n"}'
```

- **New api-rs, CRD serves only `v1alpha1`:** the hook has not run. Usually the
  new image arrived before the new chart (see the Argo CD notes above). Get the
  new chart synced; its hook replaces the crashing Deployment. Nothing has
  been changed while api-rs is crash-looping.
- **Old api-rs, CRD stores `[v1beta1]`:** the migration finished, but an old
  api-rs image was started afterwards (for example by a rollback or a stale
  image tag). Deploy the new image. Do not roll the chart back.

### Argo CD: `ComparisonError` mentioning `status.storedVersions[0]: Invalid value: "v1alpha1"`

The CRDs Argo CD is diffing do not list `v1alpha1` at all, so a server-side
dry run is rejected while the cluster still stores it, and the sync, including
its PreSync hook, never starts. The bundled CRDs carry an unserved `v1alpha1`
entry for exactly this reason; it is missing if the CRDs came from somewhere
else or were re-vendored without it. Restore the entry (it is at the end of
each file in `contrib/chart/charts/agent-sandbox/crds/`). The Application may
show a stale `Synced` in the meantime; check `.status.conditions`.

### A parent app-of-apps sync stalls

If the chart's Application is managed by a parent Application that syncs
children in waves and waits for each to become healthy, a child that is
crash-looping (see above) can hold the parent's sync indefinitely, so the new
chart revision is never applied. Terminate the parent's sync and sync the
chart's Application directly:

```bash
argocd app terminate-op <parent-app>
argocd app sync <parent-app> --resource argoproj.io:Application:argocd/<app>
argocd app sync <app>
```

### A sandbox paused before the upgrade came back `Running`

Its replica count was not recorded before it was rewritten. That can happen
if something other than the hook changed the CRDs or the Sandbox in between
(for example, applying CRDs from another source and pruning
`status.storedVersions` by hand). Suspend it again; its state is intact:

```bash
kubectl --context <ctx> -n <namespace> patch sandboxes.v1beta1.agents.x-k8s.io <name> \
  --type merge -p '{"spec":{"operatingMode":"Suspended"}}'
```

`sandboxes-before.yaml` lists which sandboxes had `spec.replicas: 0`.

### Children of a deleted Sandbox are not garbage-collected

Their owner references still name `v1alpha1`, which garbage collection can no
longer resolve. The hook rewrites owner references in namespaces that have
Sandboxes, for Pods, Services, ConfigMaps, PersistentVolumeClaims, and
NetworkPolicies, and only while a migration is pending. Find what it missed:

```bash
kubectl --context <ctx> get pods,svc,cm,pvc,netpol -A -o json \
  | jq -r '.items[] | select(any(.metadata.ownerReferences[]?; .kind=="Sandbox" and (.apiVersion|endswith("/v1alpha1")))) | "\(.kind) \(.metadata.namespace)/\(.metadata.name)"'
```

If the owning Sandbox still exists, point the reference at `v1beta1`;
otherwise delete the object (Centaur's carry a `centaur.ai/sandbox-id`
label).

### `kubectl get sandboxes` fails with `the server could not find the requested resource`

kubectl's discovery cache still prefers `v1alpha1`. Name the version
(`sandboxes.v1beta1.agents.x-k8s.io`) or clear the cache under
`~/.kube/cache/discovery`.

## Rollback

Rolling back after the hook has run is not supported. The previous chart's
CRDs omit `v1beta1`, which the API server now holds data in, so they are
rejected; its api-rs only speaks `v1alpha1`, which is no longer served, and
treats every Sandbox as missing. Returning to `v1alpha1` storage would also
lose `operatingMode`, which `v1alpha1` cannot represent. Upstream's documented
rollback deletes every Sandbox.

If something goes wrong, fix the cause and re-run the upgrade of the new
chart: every step of the hook is safe to repeat.
