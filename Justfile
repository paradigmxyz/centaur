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

default:
    just --list

build:
    #!/usr/bin/env bash
    set -euo pipefail
    if [[ "${JUST_BUILD_SEQUENTIAL:-0}" =~ ^(1|true|yes)$ ]]; then
      just _build-all-sequential
    else
      pids=()
      for recipe in _build-api-rs _build-iron-proxy _build-slackbotv2 _build-linearbot _build-discordbot _build-githubbot _build-teamsbot _build-agent _build-console; do
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
    just _build-linearbot
    just _build-discordbot
    just _build-githubbot
    just _build-teamsbot
    just _build-agent
    just _build-console

build-one service:
    #!/usr/bin/env bash
    set -euo pipefail
    case "{{service}}" in
      api-rs) just _build-api-rs ;;
      iron-proxy) just _build-iron-proxy ;;
      slackbotv2) just _build-slackbotv2 ;;
      linearbot) just _build-linearbot ;;
      discordbot) just _build-discordbot ;;
      githubbot) just _build-githubbot ;;
      teamsbot) just _build-teamsbot ;;
      agent|sandbox) just _build-agent ;;
      workflow-python) just _build-workflow-python ;;
      console) just _build-console ;;
      *) echo "unknown service: {{service}}" >&2; exit 2 ;;
    esac

_build-api-rs:
    docker build -t centaur-api-rs:latest -f services/api-rs/Dockerfile .

_build-iron-proxy:
    docker build -t centaur-iron-proxy:latest -f services/iron-proxy/Dockerfile .

_build-slackbotv2:
    docker build -t centaur-slackbotv2:latest -f services/slackbotv2/Dockerfile .

_build-linearbot:
    docker build -t centaur-linearbot:latest -f services/linearbot/Dockerfile .

_build-discordbot:
    docker build -t centaur-discordbot:latest -f services/discordbot/Dockerfile .

_build-githubbot:
    docker build -t centaur-githubbot:latest -f services/githubbot/Dockerfile .

_build-teamsbot:
    docker build -t centaur-teamsbot:latest -f services/teamsbot/Dockerfile .

_build-agent:
    docker build --target "{{agent_build_target}}" -t "{{agent_image}}" -f "{{agent_dockerfile}}" .

# The Python workflow host is embedded in both consumer images.
_build-workflow-python:
    just _build-api-rs
    just _build-agent

# The console builds from its own subdirectory context (services/console), unlike
# the other services which build from the repo root.
_build-console:
    docker build -t centaur-console:latest -f services/console/Dockerfile services/console

# Push locally-built images to the local registry under library/ so k3s pulls
# them via its docker.io mirror. Used by `just up k3s`. Only changed layers are
# pushed, so this is much faster than `_import-k3s` on repeat runs.
_push-registry:
    #!/usr/bin/env bash
    set -euo pipefail
    for img in centaur-api-rs centaur-iron-proxy centaur-slackbotv2 centaur-linearbot centaur-discordbot centaur-githubbot centaur-teamsbot centaur-agent centaur-console; do
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
    for img in centaur-api-rs centaur-iron-proxy centaur-slackbotv2 centaur-linearbot centaur-discordbot centaur-githubbot centaur-teamsbot centaur-agent centaur-console; do
      echo "importing ${img}:latest into k3s containerd..."
      docker save "${img}:latest" | {{k3s_ctr}} images import -
    done

bootstrap-secrets *args:
    contrib/scripts/bootstrap-k8s-secrets.sh --namespace {{namespace}} {{args}}

deploy:
    #!/usr/bin/env bash
    set -euo pipefail
    helm dependency update {{chart}} >/dev/null
    # Prefer -f values over --set for image repos: Helm --set always beats -f,
    # which prevented CENTAUR_EXTRA_VALUES from pinning a local console image.
    values_args=("-f" "{{dev_values}}")
    case "{{source}}" in
      local) ;;
      ghcr) values_args+=("-f" "contrib/chart/values.ghcr.yaml") ;;
      *) echo "unknown source: {{source}} (expected local or ghcr)" >&2; exit 2 ;;
    esac
    extra_args=()
    if [[ -n "${OP_CONNECT_CREDENTIALS_FILE:-}" ]]; then
      extra_args+=("--set" "ironProxy.secretSource=onepassword-connect" "--set" "onepasswordConnect.connect.create=true")
    fi
    if [[ -n "${CODEX_AUTH_MODE:-}" ]]; then
      extra_args+=("--set" "sandbox.codexAuthMode=${CODEX_AUTH_MODE}")
    fi
    if [[ -n "${CLAUDE_CODE_AUTH_MODE:-}" ]]; then
      extra_args+=("--set" "sandbox.claudeCodeAuthMode=${CLAUDE_CODE_AUTH_MODE}")
    fi
    if [[ -n "${CENTAUR_EXTRA_VALUES:-}" ]]; then
      IFS=',' read -r -a extra_values <<< "${CENTAUR_EXTRA_VALUES}"
      for values_file in "${extra_values[@]}"; do
        values_file="${values_file#"${values_file%%[![:space:]]*}"}"
        values_file="${values_file%"${values_file##*[![:space:]]}"}"
        [[ -n "${values_file}" ]] || continue
        values_args+=("-f" "${values_file}")
      done
    fi
    helm upgrade --install {{release}} {{chart}} -n {{namespace}} --create-namespace \
      "${values_args[@]}" ${extra_args[@]+"${extra_args[@]}"}

smoke:
    #!/usr/bin/env bash
    set -euo pipefail
    namespace="${NAMESPACE:-{{namespace}}}"
    release="${RELEASE:-{{release}}}"
    api_deployment="${release}-centaur-api-rs"
    prompt="Reply exactly PONG"

    context="$(kubectl config current-context)"
    [[ -n "${context}" ]] || { echo "SMOKE_ERROR: kubectl has no current context" >&2; exit 1; }
    echo "Using kubectl context: ${context}"

    # Slackbot authenticates to api-rs with this key. The smoke path executes
    # curl inside api-rs, where localhost intentionally bypasses API-key auth.
    # Read only the encoded Secret field so no credential is printed or passed
    # to a process command line.
    slackbot_api_key_b64="$(kubectl -n "${namespace}" get secret centaur-infra-env \
      -o jsonpath='{.data.SLACKBOT_API_KEY}')"
    [[ -n "${slackbot_api_key_b64}" ]] || {
      echo "SMOKE_ERROR: secret centaur-infra-env is missing SLACKBOT_API_KEY" >&2
      exit 1
    }

    kubectl -n "${namespace}" get deployment "${api_deployment}" >/dev/null
    kubectl -n "${namespace}" exec "deploy/${api_deployment}" -- \
      curl -fsS http://localhost:8080/healthz >/dev/null

    thread_key="smoke:claudecode-$(date +%s)-${RANDOM}"
    thread_path="$(jq -rn --arg value "${thread_key}" '$value | @uri')"
    session_payload='{"harness_type":"claudecode","on_harness_conflict":"restart"}'
    kubectl -n "${namespace}" exec "deploy/${api_deployment}" -- \
      curl -fsS -X POST "http://localhost:8080/api/session/${thread_path}" \
      -H "Content-Type: application/json" -d "${session_payload}" >/dev/null

    message_payload="$(jq -nc --arg text "${prompt}" \
      '{messages:[{role:"user",parts:[{type:"text",text:$text}]}]}')"
    kubectl -n "${namespace}" exec "deploy/${api_deployment}" -- \
      curl -fsS -X POST "http://localhost:8080/api/session/${thread_path}/messages" \
      -H "Content-Type: application/json" -d "${message_payload}" >/dev/null

    input_line="$(jq -nc --arg text "${prompt}" \
      '{type:"user",message:{content:[{type:"text",text:$text}]}}')"
    execute_payload="$(jq -nc --arg input "${input_line}" \
      '{input_lines:[$input],idle_timeout_ms:60000,max_duration_ms:300000}')"
    execute="$(kubectl -n "${namespace}" exec "deploy/${api_deployment}" -- \
      curl -fsS -X POST "http://localhost:8080/api/session/${thread_path}/execute" \
      -H "Content-Type: application/json" -d "${execute_payload}")"
    execution_id="$(jq -er '.execution_id' <<< "${execute}")"

    for _ in $(seq 1 90); do
      events="$(kubectl -n "${namespace}" exec "deploy/${api_deployment}" -- \
        curl -sS -N --max-time 3 \
        "http://localhost:8080/api/session/${thread_path}/events?execution_id=${execution_id}&after_event_id=0" \
        || true)"
      if [[ "${events}" == *PONG* ]]; then
        echo "SMOKE_OK"
        exit 0
      fi
      sleep 2
    done

    echo "SMOKE_ERROR: timed out waiting for PONG (thread=${thread_key}, execution=${execution_id})" >&2
    exit 1

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
