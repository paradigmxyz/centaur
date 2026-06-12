set dotenv-load := true

namespace := env_var_or_default("CENTAUR_NAMESPACE", "centaur")
release := env_var_or_default("CENTAUR_RELEASE", "centaur")
source := env_var_or_default("CENTAUR_IMAGE_SOURCE", "local")
chart := "contrib/chart"
dev_values := "contrib/chart/values.dev.yaml"
# Command used to import images into k3s's containerd. Override for rootless or
# remote setups, e.g. CENTAUR_K3S_CTR="k3s ctr" or "ssh host sudo k3s ctr".
k3s_ctr := env_var_or_default("CENTAUR_K3S_CTR", "sudo k3s ctr")
# Local image registry `just up k3s` pushes to. Images are pushed under the
# `library/` namespace so k3s resolves the chart's bare `:latest` tags through a
# docker.io registry mirror — configure that on the node with:
#   /etc/rancher/k3s/registries.yaml
#     mirrors:
#       docker.io:
#         endpoint: ["http://localhost:5000"]
registry := env_var_or_default("CENTAUR_LOCAL_REGISTRY", "localhost:5000")
agent_dockerfile := env_var_or_default("CENTAUR_AGENT_DOCKERFILE", "services/sandbox/Dockerfile")
agent_build_target := env_var_or_default("CENTAUR_AGENT_BUILD_TARGET", "sandbox")
agent_image := env_var_or_default("CENTAUR_AGENT_IMAGE", "centaur-agent:latest")
thin_agent_image := env_var_or_default("CENTAUR_THIN_AGENT_IMAGE", "centaur-agent:thin")

default:
    just --list

build:
    #!/usr/bin/env bash
    set -euo pipefail
    if [[ "${JUST_BUILD_SEQUENTIAL:-0}" =~ ^(1|true|yes)$ ]]; then
      just _build-all-sequential
    else
      pids=()
      for recipe in _build-api-rs _build-iron-proxy _build-slackbotv2 _build-agent; do
        just "$recipe" &
        pids+=("$!")
      done
      status=0
      for pid in "${pids[@]}"; do
        wait "$pid" || status=1
      done
      exit "$status"
    fi

_build-all-sequential:
    just _build-api-rs
    just _build-iron-proxy
    just _build-slackbotv2
    just _build-agent

build-one service:
    #!/usr/bin/env bash
    set -euo pipefail
    case "{{service}}" in
      api-rs) just _build-api-rs ;;
      iron-proxy) just _build-iron-proxy ;;
      slackbotv2) just _build-slackbotv2 ;;
      agent|sandbox) just _build-agent ;;
      agent-thin|sandbox-thin) just _build-agent-thin ;;
      *) echo "unknown service: {{service}}" >&2; exit 2 ;;
    esac

_build-api-rs:
    docker build -t centaur-api-rs:latest -f services/api-rs/Dockerfile .

_build-iron-proxy:
    docker build -t centaur-iron-proxy:latest -f services/iron-proxy/Dockerfile .

_build-slackbotv2:
    docker build -t centaur-slackbotv2:latest -f services/slackbotv2/Dockerfile .

_build-agent:
    docker build --target "{{agent_build_target}}" -t "{{agent_image}}" -f "{{agent_dockerfile}}" .

_build-agent-thin:
    docker build --target sandbox -t "{{thin_agent_image}}" -f services/sandbox/Dockerfile.thin .

# Push locally-built images to the local registry under library/ so k3s pulls
# them via its docker.io mirror. Used by `just up k3s`. Only changed layers are
# pushed, so this is much faster than `_import-k3s` on repeat runs.
_push-registry:
    #!/usr/bin/env bash
    set -euo pipefail
    for img in centaur-api-rs centaur-iron-proxy centaur-slackbotv2 centaur-agent; do
      target="{{registry}}/library/${img}:latest"
      echo "pushing ${img}:latest -> ${target}..."
      docker tag "${img}:latest" "${target}"
      docker push "${target}"
    done

# Legacy: import locally-built images straight into k3s's containerd (no registry
# needed). Slower than `_push-registry`; kept as a fallback. Run manually with
# `just _import-k3s`.
_import-k3s:
    #!/usr/bin/env bash
    set -euo pipefail
    for img in centaur-api-rs centaur-iron-proxy centaur-slackbotv2 centaur-agent; do
      echo "importing ${img}:latest into k3s containerd..."
      docker save "${img}:latest" | {{k3s_ctr}} images import -
    done

bootstrap-secrets *args:
    contrib/scripts/bootstrap-k8s-secrets.sh --namespace {{namespace}} {{args}}

deploy:
    #!/usr/bin/env bash
    set -euo pipefail
    helm dependency update {{chart}} >/dev/null
    extra_args=()
    case "{{source}}" in
      local) ;;
      ghcr)
        extra_args+=(
          --set apiRs.image.repository=ghcr.io/paradigmxyz/centaur/centaur-api-rs
          --set ironProxy.image.repository=ghcr.io/paradigmxyz/centaur/centaur-iron-proxy
          --set slackbotv2.image.repository=ghcr.io/paradigmxyz/centaur/centaur-slackbotv2
          --set sandbox.image.repository=ghcr.io/paradigmxyz/centaur/centaur-agent
        )
        ;;
      *) echo "unknown source: {{source}} (expected local or ghcr)" >&2; exit 2 ;;
    esac
    if [[ -n "${OP_CONNECT_CREDENTIALS_FILE:-}" ]]; then
      extra_args+=(
        --set ironProxy.secretSource=onepassword-connect
        --set onepasswordConnect.connect.create=true
      )
    fi
    if [[ -n "${CODEX_AUTH_MODE:-}" ]]; then
      extra_args+=(
        --set sandbox.codexAuthMode=${CODEX_AUTH_MODE}
      )
    fi
    if [[ -n "${CLAUDE_CODE_AUTH_MODE:-}" ]]; then
      extra_args+=(
        --set sandbox.claudeCodeAuthMode=${CLAUDE_CODE_AUTH_MODE}
      )
    fi
    # Layer an optional local-only values file (e.g. Tailscale Funnel ingress) on
    # top of values.dev.yaml. Kept out of the shared dev values so teammates'
    # `just up` is unaffected. Appended after -f {{dev_values}} so it wins
    # (helm applies -f files left-to-right).
    if [[ -n "${CENTAUR_EXTRA_VALUES:-}" ]]; then
      extra_args+=(-f "${CENTAUR_EXTRA_VALUES}")
    fi
    helm upgrade --install {{release}} {{chart}} -n {{namespace}} --create-namespace -f {{dev_values}} ${extra_args[@]+"${extra_args[@]}"}

# Bring up the dev stack; pass `k3s` (just up k3s) to push local images to the
# local registry (CENTAUR_LOCAL_REGISTRY, default localhost:5000) for k3s to pull.
up import="":
    #!/usr/bin/env bash
    set -euo pipefail
    if [[ -n "{{import}}" && "{{import}}" != "k3s" ]]; then
      echo "unknown argument: {{import}} (expected nothing or 'k3s')" >&2; exit 2
    fi
    just bootstrap-secrets
    case "{{source}}" in
      local)
        just build
        if [[ "{{import}}" == "k3s" ]]; then
          just _push-registry
        fi
        ;;
      ghcr) ;;
      *) echo "unknown source: {{source}} (expected local or ghcr)" >&2; exit 2 ;;
    esac
    just source={{source}} deploy

down:
    kubectl delete namespace {{namespace}} --ignore-not-found --wait

reinstall:
    just down
    just up

status:
    kubectl get all -n {{namespace}}

logs component:
    kubectl logs -n {{namespace}} deploy/{{release}}-centaur-{{component}} --tail=200 -f

shell component:
    kubectl exec -it -n {{namespace}} deploy/{{release}}-centaur-{{component}} -- sh
