#!/usr/bin/env bash
# Read-only readiness check for the chart's agent-sandbox v1alpha1 -> v1beta1
# migration. See contrib/MIGRATING_AGENT_SANDBOX.md.
set -euo pipefail

CONTEXT=""
NAMESPACE="centaur"
CONTROLLER_NAMESPACE="agent-sandbox-system"

usage() {
  cat <<'EOF'
Usage: contrib/scripts/agent-sandbox-preflight.sh --context CONTEXT
         [--namespace NAMESPACE] [--controller-namespace NAMESPACE]

Checks, without changing anything, whether a cluster is ready for the chart's
agent-sandbox upgrade and whether the upgrade will migrate it.

  --context               kubeconfig context to check (required)
  --namespace             namespace of the Centaur release (default: centaur)
  --controller-namespace  agentSandbox.namespace.name (default: agent-sandbox-system)

Requires kubectl and jq. Exits 0 when ready (review any warnings), 1 when the
upgrade would be refused, and 2 on usage or access errors.
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --context) CONTEXT="${2:?--context needs a value}"; shift 2 ;;
    --namespace) NAMESPACE="${2:?--namespace needs a value}"; shift 2 ;;
    --controller-namespace) CONTROLLER_NAMESPACE="${2:?--controller-namespace needs a value}"; shift 2 ;;
    -h | --help) usage; exit 0 ;;
    *) echo "unknown argument: $1" >&2; usage >&2; exit 2 ;;
  esac
done
if [[ -z "$CONTEXT" ]]; then
  echo "--context is required; this script never uses the ambient context" >&2
  usage >&2
  exit 2
fi
for tool in kubectl jq; do
  command -v "$tool" > /dev/null || { echo "$tool is required" >&2; exit 2; }
done

k() { kubectl --context "$CONTEXT" "$@"; }

FAILURES=0
WARNINGS=0
pass() { printf 'PASS  %s\n' "$*"; }
info() { printf 'INFO  %s\n' "$*"; }
warn() { printf 'WARN  %s\n' "$*"; WARNINGS=$((WARNINGS + 1)); }
fail() { printf 'FAIL  %s\n' "$*"; FAILURES=$((FAILURES + 1)); }

CRDS=(
  sandboxes.agents.x-k8s.io
  sandboxclaims.extensions.agents.x-k8s.io
  sandboxtemplates.extensions.agents.x-k8s.io
  sandboxwarmpools.extensions.agents.x-k8s.io
)

if ! crd_list="$(k get crd -o json 2> /dev/null)"; then
  echo "cannot list CustomResourceDefinitions with context $CONTEXT" >&2
  exit 2
fi
info "context $CONTEXT, release namespace $NAMESPACE, controller namespace $CONTROLLER_NAMESPACE"

# --- CRD state ----------------------------------------------------------------
# A CRD needs migrating while it serves or stores v1alpha1, exactly the test
# the hook applies.
present=0
pending=0
for crd in "${CRDS[@]}"; do
  state="$(jq -r --arg name "$crd" '
    .items[] | select(.metadata.name == $name) |
    [
      ([.spec.versions[] | select(.served) | .name] | join(",")),
      ([.spec.versions[] | select(.storage) | .name] | join(",")),
      ((.status.storedVersions // []) | join(",")),
      (.spec.conversion.strategy // "None")
    ] | @tsv' <<< "$crd_list")"
  if [[ -z "$state" ]]; then
    info "$crd: not installed"
    continue
  fi
  present=$((present + 1))
  IFS=$'\t' read -r served storage stored conversion <<< "$state"
  detail="serves [$served], stores [$storage], storedVersions [$stored], conversion $conversion"
  if [[ ",$served," == *,v1alpha1,* || ",$stored," == *,v1alpha1,* ]]; then
    pending=$((pending + 1))
    info "$crd: needs migration ($detail)"
    if [[ "$conversion" == Webhook && ",$stored," == *,v1alpha1,* ]]; then
      fail "$crd still stores v1alpha1 behind a conversion webhook: finish upstream's v0.5 storage migration first; the hook refuses this state"
    fi
  else
    info "$crd: migrated ($detail)"
  fi
done

if [[ $present -eq 0 ]]; then
  pass "no agent-sandbox CRDs: a fresh install; the hook installs them and migrates nothing"
elif [[ $pending -eq 0 ]]; then
  pass "all agent-sandbox CRDs already serve and store only v1beta1; the upgrade migrates nothing"
else
  info "the upgrade will migrate this cluster (one-way; see contrib/MIGRATING_AGENT_SANDBOX.md)"
fi

crd_present() { jq -e --arg name "$1" 'any(.items[]; .metadata.name == $name)' <<< "$crd_list" > /dev/null; }

# --- Objects --------------------------------------------------------------------
if crd_present sandboxclaims.extensions.agents.x-k8s.io; then
  claims="$(k get sandboxclaims.extensions.agents.x-k8s.io -A -o json | jq '.items | length')"
  if [[ "$claims" -gt 0 && $pending -gt 0 ]]; then
    fail "$claims SandboxClaims exist; the hook refuses to migrate them (delete unused claims, or migrate them with upstream's v0.5 release first)"
  elif [[ "$claims" -gt 0 ]]; then
    info "$claims SandboxClaims exist"
  else
    pass "no SandboxClaims"
  fi
fi

for crd in sandboxtemplates.extensions.agents.x-k8s.io sandboxwarmpools.extensions.agents.x-k8s.io; do
  if crd_present "$crd"; then
    count="$(k get "$crd" -A -o json | jq '.items | length')"
    [[ "$count" -gt 0 ]] && info "$count ${crd%%.*}: rewritten unchanged (their fields are identical in both versions)"
  fi
done

if crd_present sandboxes.agents.x-k8s.io; then
  # Through the preferred version: replicas in v1alpha1, operatingMode in v1beta1.
  k get sandboxes.agents.x-k8s.io -A -o json | jq -r '
    .items | group_by(.metadata.namespace)[] |
    "\(.[0].metadata.namespace)\t\(length)\t\([.[] | select(.spec.replicas == 0 or .spec.operatingMode == "Suspended")] | length)"' |
    while IFS=$'\t' read -r ns total paused; do
      info "$total Sandboxes in $ns, $paused paused"
    done
  if [[ $pending -gt 0 ]]; then
    owned="$(k get pods,services,configmaps,persistentvolumeclaims,networkpolicies -A -o json | jq '
      [.items[].metadata.ownerReferences[]? | select(.kind == "Sandbox" and (.apiVersion | endswith("/v1alpha1")))] | length')"
    info "$owned owner references point at v1alpha1 Sandboxes; the hook rewrites them"
  fi
fi

# --- Writers the hook does and does not stop ------------------------------------
deployments="$(k get deployments -A -o json)"
controllers="$(jq -r '.items[] |
  select(any(.spec.template.spec.containers[]; .image | test("agent-sandbox-controller"))) |
  "\(.metadata.namespace)/\(.metadata.name)\t\([.spec.template.spec.containers[].image] | join(","))"' <<< "$deployments")"
if [[ -z "$controllers" ]]; then
  info "no agent-sandbox controller Deployment found"
else
  while IFS=$'\t' read -r name images; do
    if [[ "$name" == "$CONTROLLER_NAMESPACE/agent-sandbox-controller" ]]; then
      info "agent-sandbox controller $name runs $images; the hook replaces it"
    elif [[ $pending -gt 0 ]]; then
      warn "another agent-sandbox controller, $name ($images), also reconciles Sandboxes; the hook does not stop it, so scale it down or upgrade it to v1.x with this release"
    else
      warn "another agent-sandbox controller, $name ($images), also reconciles Sandboxes"
    fi
  done <<< "$controllers"
fi

api_rs="$(jq -r '.items[] |
  select(.metadata.labels["app.kubernetes.io/component"] == "api-rs") |
  "\(.metadata.namespace)/\(.metadata.name)\t\(.spec.template.spec.containers[0].image)"' <<< "$deployments")"
if [[ $present -gt 0 ]] && ! grep -q "^$NAMESPACE/" <<< "$api_rs"; then
  warn "no api-rs Deployment in $NAMESPACE; check --namespace"
fi
while IFS=$'\t' read -r name image; do
  [[ -z "$name" ]] && continue
  if [[ "$name" == "$NAMESPACE/"* ]]; then
    info "api-rs $name runs $image; the hook stops it and the release recreates it"
  elif [[ $pending -gt 0 ]]; then
    warn "api-rs $name ($image) belongs to another release; the CRDs are cluster-wide, so it fails once v1alpha1 is gone unless it is upgraded too"
  fi
done <<< "$api_rs"

# --- What the hook needs ----------------------------------------------------------
# The hook's ClusterRole and ClusterRoleBinding are created by whoever applies
# the release, which may be a GitOps controller rather than this identity.
for check in "create clusterroles" "create clusterrolebindings" "create jobs -n $NAMESPACE"; do
  # shellcheck disable=SC2086 # the check is a verb, a resource, and flags
  if [[ "$(k auth can-i $check 2> /dev/null || true)" == yes ]]; then
    pass "this identity can $check"
  else
    warn "this identity cannot $check; whatever applies the release (for example a GitOps controller) must be able to"
  fi
done

api_port="$(k get endpoints kubernetes -n default -o jsonpath='{.subsets[0].ports[0].port}' 2> /dev/null || true)"
# The hook's egress policy, like api-rs's, allows 443 and networkPolicy.apiServerPort.
if [[ -n "$api_port" && "$api_port" != 443 ]]; then
  info "the API server endpoint listens on $api_port; with networkPolicy.enabled, set networkPolicy.apiServerPort to $api_port (default 6443)"
fi

# --- Verdict ------------------------------------------------------------------------
echo
if [[ $FAILURES -gt 0 ]]; then
  echo "NOT READY: $FAILURES blocking problem(s), $WARNINGS warning(s)"
  exit 1
elif [[ $WARNINGS -gt 0 ]]; then
  echo "READY WITH WARNINGS: review the $WARNINGS warning(s) above"
  exit 0
fi
echo "READY"
