set dotenv-load := true

namespace := env_var_or_default("CENTAUR_NAMESPACE", "centaur")
release := env_var_or_default("CENTAUR_RELEASE", "centaur")
source := env_var_or_default("CENTAUR_IMAGE_SOURCE", "local")
chart := "contrib/chart"
dev_values := "contrib/chart/values.dev.yaml"
# Command used to import images into k3s's containerd. Override for rootless or
# remote setups, e.g. CENTAUR_K3S_CTR="k3s ctr" or "ssh host sudo k3s ctr".
k3s_ctr := env_var_or_default("CENTAUR_K3S_CTR", "sudo k3s ctr")

default:
    just --list

build:
    #!/usr/bin/env bash
    set -euo pipefail
    if [[ "${JUST_BUILD_SEQUENTIAL:-0}" =~ ^(1|true|yes)$ ]]; then
      just _build-all-sequential
    else
      pids=()
      for recipe in _build-api _build-iron-proxy _build-slackbot _build-agent; do
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
    just _build-api
    just _build-iron-proxy
    just _build-slackbot
    just _build-agent

build-one service:
    #!/usr/bin/env bash
    set -euo pipefail
    case "{{service}}" in
      api) just _build-api ;;
      iron-proxy) just _build-iron-proxy ;;
      slackbot) just _build-slackbot ;;
      agent|sandbox) just _build-agent ;;
      *) echo "unknown service: {{service}}" >&2; exit 2 ;;
    esac

_build-api:
    docker build -t centaur-api:latest -f services/api/Dockerfile .

_build-iron-proxy:
    docker build -t centaur-iron-proxy:latest -f services/iron-proxy/Dockerfile .

_build-slackbot:
    docker build -t centaur-slackbot:latest -f services/slackbot/Dockerfile .

_build-agent:
    docker build --target sandbox -t centaur-agent:latest -f services/sandbox/Dockerfile .

# Import locally-built images into k3s's containerd. k3s uses containerd, not
# the Docker daemon, so `docker build` images are otherwise invisible to it
# (pods ImagePullBackOff on the :latest tags). Used by `just up k3s`.
_import-k3s:
    #!/usr/bin/env bash
    set -euo pipefail
    for img in centaur-api centaur-iron-proxy centaur-slackbot centaur-agent; do
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
          --set api.image.repository=ghcr.io/paradigmxyz/centaur/centaur-api
          --set ironProxy.image.repository=ghcr.io/paradigmxyz/centaur/centaur-iron-proxy
          --set slackbot.image.repository=ghcr.io/paradigmxyz/centaur/centaur-slackbot
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
        --set sandbox.extraEnv.CODEX_AUTH_MODE=${CODEX_AUTH_MODE}
      )
    fi
    if [[ -n "${CLAUDE_CODE_AUTH_MODE:-}" ]]; then
      extra_args+=(
        --set sandbox.extraEnv.CLAUDE_CODE_AUTH_MODE=${CLAUDE_CODE_AUTH_MODE}
      )
    fi
    helm upgrade --install {{release}} {{chart}} -n {{namespace}} --create-namespace -f {{dev_values}} ${extra_args[@]+"${extra_args[@]}"}

# Bring up the dev stack; pass `k3s` (just up k3s) to import local images into k3s's containerd.
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
          just _import-k3s
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

slack-thread-logs slack_link since="24h":
    CENTAUR_NAMESPACE={{namespace}} CENTAUR_RELEASE={{release}} bash services/slackbot/scripts/slack-thread-logs.sh "{{slack_link}}" "{{since}}"

slack-thread-report slack_link:
    CENTAUR_NAMESPACE={{namespace}} CENTAUR_RELEASE={{release}} bash services/slackbot/scripts/slack-thread-report.sh "{{slack_link}}"

cleanup-orphan-proxy-services mode="dry-run":
    #!/usr/bin/env bash
    set -euo pipefail
    case "{{mode}}" in
      dry-run|delete) ;;
      *) echo "mode must be dry-run or delete" >&2; exit 2 ;;
    esac

    live_sandboxes="$(mktemp)"
    trap 'rm -f "$live_sandboxes"' EXIT
    kubectl -n {{namespace}} get pod -l centaur.ai/managed=true \
      -o jsonpath='{range .items[*]}{.metadata.labels.centaur\.ai/sandbox-id}{"\n"}{end}' \
      | sort -u > "$live_sandboxes"

    found=0
    while IFS=$'\t' read -r service sandbox_id; do
      [[ -n "$service" && -n "$sandbox_id" ]] || continue
      [[ "$sandbox_id" != "api" ]] || continue
      if grep -qx "$sandbox_id" "$live_sandboxes"; then
        continue
      fi
      found=1
      if [[ "{{mode}}" == "delete" ]]; then
        kubectl -n {{namespace}} delete svc "$service"
      else
        printf 'orphan proxy service: %s sandbox_id=%s\n' "$service" "$sandbox_id"
      fi
    done < <(
      kubectl -n {{namespace}} get svc -l centaur.ai/iron-proxy=true \
        -o jsonpath='{range .items[*]}{.metadata.name}{"\t"}{.metadata.labels.centaur\.ai/sandbox-id}{"\n"}{end}'
    )

    if [[ "$found" -eq 0 ]]; then
      echo "No orphan proxy services found."
    fi

shell component:
    kubectl exec -it -n {{namespace}} deploy/{{release}}-centaur-{{component}} -- sh

smoke:
    #!/usr/bin/env bash
    set -euo pipefail
    THREAD_KEY="smoke-$(date +%s)"
    API_DEPLOY="deploy/{{release}}-centaur-api"

    SPAWN=$(kubectl exec -n {{namespace}} "$API_DEPLOY" -- curl -s -X POST http://localhost:8000/agent/spawn \
      -H "Content-Type: application/json" \
      -d "{\"thread_key\":\"${THREAD_KEY}\"}")
    ASSIGNMENT_GENERATION=$(printf '%s' "$SPAWN" | jq -r '.assignment_generation')

    kubectl exec -n {{namespace}} "$API_DEPLOY" -- curl -s -X POST http://localhost:8000/agent/message \
      -H "Content-Type: application/json" \
      -d "{\"thread_key\":\"${THREAD_KEY}\",\"assignment_generation\":${ASSIGNMENT_GENERATION},\"role\":\"user\",\"parts\":[{\"type\":\"text\",\"text\":\"Reply with exactly PONG and nothing else.\"}]}" >/dev/null

    EXECUTE=$(kubectl exec -n {{namespace}} "$API_DEPLOY" -- curl -s -X POST http://localhost:8000/agent/execute \
      -H "Content-Type: application/json" \
      -d "{\"thread_key\":\"${THREAD_KEY}\",\"assignment_generation\":${ASSIGNMENT_GENERATION},\"delivery\":{\"platform\":\"dev\"}}")
    EXECUTION_ID=$(printf '%s' "$EXECUTE" | jq -r '.execution_id')

    for _ in $(seq 1 60); do
      STATE=$(kubectl exec -n {{namespace}} "$API_DEPLOY" -- curl -s "http://localhost:8000/agent/executions/${EXECUTION_ID}")
      STATUS=$(printf '%s' "$STATE" | jq -r '.status // empty')
      case "$STATUS" in
        completed)
          printf '%s\n' "$STATE" | jq
          printf '%s\n' "$STATE" | jq -e '.result_text | contains("PONG")' >/dev/null
          exit 0
          ;;
        failed|failed_permanent|cancelled)
          printf '%s\n' "$STATE" | jq
          exit 1
          ;;
      esac
      sleep 2
    done

    kubectl exec -n {{namespace}} "$API_DEPLOY" -- curl -s "http://localhost:8000/agent/executions/${EXECUTION_ID}" | jq
    echo "smoke timed out waiting for execution ${EXECUTION_ID}" >&2
    exit 1
