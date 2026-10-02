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
e2e_cluster := env_var_or_default("CENTAUR_E2E_CLUSTER", "centaur-e2e")
# bootstrap-k8s-secrets.sh assumes the default release name.
e2e_release := "centaur"
e2e_namespace := "centaur"
e2e_slack_namespace := "centaur-e2e-slack"
e2e_slack_port := env_var_or_default("CENTAUR_E2E_SLACK_PORT", "18443")
# Fixture values shared by the fake Slack and slackbotv2; not real credentials.
e2e_slack_bot_token := "xoxb-centaur-e2e"
e2e_slack_signing_secret := "centaur-e2e-signing-secret"

default:
    just --list

build:
    #!/usr/bin/env bash
    set -euo pipefail
    if [[ "${JUST_BUILD_SEQUENTIAL:-0}" =~ ^(1|true|yes)$ ]]; then
      just _build-all-sequential
    else
      pids=()
      for recipe in _build-api-rs _build-company-context _build-proxy-sync _build-iron-proxy _build-slackbotv2 _build-linearbot _build-discordbot _build-githubbot _build-teamsbot _build-agent _build-console; do
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
    just _build-company-context
    just _build-proxy-sync
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
      company-context) just _build-company-context ;;
      proxy-sync) just _build-proxy-sync ;;
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

_build-company-context:
    docker build -t centaur-company-context:latest -f services/company-context/Dockerfile .

_build-proxy-sync:
    docker build -t centaur-proxy-sync:latest -f services/proxy-sync/Dockerfile .

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
    for img in centaur-api-rs centaur-company-context centaur-proxy-sync centaur-iron-proxy centaur-slackbotv2 centaur-linearbot centaur-discordbot centaur-githubbot centaur-teamsbot centaur-agent centaur-console; do
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
    for img in centaur-api-rs centaur-company-context centaur-proxy-sync centaur-iron-proxy centaur-slackbotv2 centaur-linearbot centaur-discordbot centaur-githubbot centaur-teamsbot centaur-agent centaur-console; do
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
          --set experimentalCompanyContext.image.repository=ghcr.io/paradigmxyz/centaur/centaur-company-context
          --set proxySync.image.repository=ghcr.io/paradigmxyz/centaur/centaur-proxy-sync
          --set ironProxy.image.repository=ghcr.io/paradigmxyz/centaur/centaur-iron-proxy
          --set slackbotv2.image.repository=ghcr.io/paradigmxyz/centaur/centaur-slackbotv2
          --set linearbot.image.repository=ghcr.io/paradigmxyz/centaur/centaur-linearbot
          --set discordbot.image.repository=ghcr.io/paradigmxyz/centaur/centaur-discordbot
          --set githubbot.image.repository=ghcr.io/paradigmxyz/centaur/centaur-githubbot
          --set teamsbot.image.repository=ghcr.io/paradigmxyz/centaur/centaur-teamsbot
          --set sandbox.image.repository=ghcr.io/paradigmxyz/centaur/centaur-agent
          --set console.image.repository=ghcr.io/paradigmxyz/centaur/centaur-console
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

# The e2e stack is a dedicated kind cluster running the chart's minimal Slack
# path (Postgres, console, proxy-sync, api-rs, slackbotv2, agent sandboxes)
# against an in-cluster fake Slack, with real harnesses and real model APIs.
# Model keys (OPENAI_API_KEY, ANTHROPIC_API_KEY, ...) come from the shell. By
# default local images are built and loaded; set CENTAUR_E2E_IMAGE_TAG (e.g.
# sha-abc1234) to have the cluster pull published ghcr images instead.
# E2E_HARNESSES (default codex,claudecode) picks the harnesses to exercise.
#
# Deploy the e2e stack and mention each harness through the fake Slack.
e2e: e2e-up e2e-test

# Create or update the e2e cluster and deploy the stack.
e2e-up:
    #!/usr/bin/env bash
    set -euo pipefail
    cluster="{{e2e_cluster}}"
    # A private kubeconfig keeps every command here off the ambient context.
    export KUBECONFIG="$(mktemp)"
    trap 'rm -f "$KUBECONFIG"' EXIT
    if kind get clusters | grep -qx "$cluster"; then
      kind get kubeconfig --name "$cluster" > "$KUBECONFIG"
    else
      kind create cluster --name "$cluster" --kubeconfig "$KUBECONFIG" --wait 120s
    fi

    image_args=()
    for entry in api-rs:apiRs proxy-sync:proxySync iron-proxy:ironProxy slackbotv2:slackbotv2 agent:sandbox console:console; do
      service="${entry%%:*}" key="${entry#*:}"
      if [[ -n "${CENTAUR_E2E_IMAGE_TAG:-}" ]]; then
        image_args+=(
          --set "${key}.image.repository=ghcr.io/paradigmxyz/centaur/centaur-${service}"
          --set "${key}.image.tag=${CENTAUR_E2E_IMAGE_TAG}"
        )
        # Pull the multi-GB agent image up front, not inside a sandbox's ready timeout.
        if [[ "$service" == agent ]]; then
          docker exec "${cluster}-control-plane" crictl pull "ghcr.io/paradigmxyz/centaur/centaur-agent:${CENTAUR_E2E_IMAGE_TAG}"
        fi
      else
        just build-one "$service"
        kind load docker-image --name "$cluster" "centaur-${service}:latest"
      fi
    done

    SLACK_BOT_TOKEN="{{e2e_slack_bot_token}}" \
    SLACK_SIGNING_SECRET="{{e2e_slack_signing_secret}}" \
    SLACKBOT_API_KEY="$(openssl rand -hex 32)" \
    OP_SERVICE_ACCOUNT_TOKEN=unused OP_VAULT=unused \
      contrib/scripts/bootstrap-k8s-secrets.sh --namespace "{{e2e_namespace}}"
    # iron-proxy's env secret source resolves model keys from centaur-infra-env.
    data=()
    for name in OPENAI_API_KEY ANTHROPIC_API_KEY AMP_API_KEY OPENROUTER_API_KEY NOUS_API_KEY; do
      if [[ -n "${!name:-}" ]]; then
        data+=("\"${name}\":\"$(printf '%s' "${!name}" | base64 | tr -d '\n')\"")
      fi
    done
    if [[ "${#data[@]}" -gt 0 ]]; then
      kubectl -n "{{e2e_namespace}}" patch secret centaur-infra-env --type merge --patch-file /dev/stdin \
        <<< "{\"data\":{$(IFS=,; echo "${data[*]}")}}" >/dev/null
    fi

    slack_ns="{{e2e_slack_namespace}}"
    kubectl create namespace "$slack_ns" --dry-run=client -o yaml | kubectl apply -f -
    kubectl -n "$slack_ns" create configmap fake-slack \
      --from-file=e2e/fake-slack.ts --from-file=e2e/fixture.ts --dry-run=client -o yaml | kubectl apply -f -
    kubectl -n "$slack_ns" create secret generic fake-slack \
      --from-literal=SLACK_BOT_TOKEN="{{e2e_slack_bot_token}}" \
      --from-literal=SLACK_SIGNING_SECRET="{{e2e_slack_signing_secret}}" \
      --from-literal=SLACK_EVENTS_URL="http://{{e2e_release}}-centaur-slackbotv2.{{e2e_namespace}}.svc.cluster.local:3001/api/webhooks/slack" \
      --dry-run=client -o yaml | kubectl apply -f -
    kubectl -n "$slack_ns" apply -f e2e/fake-slack.yaml
    kubectl -n "$slack_ns" rollout restart deploy/fake-slack
    kubectl -n "$slack_ns" rollout status deploy/fake-slack --timeout=180s

    helm dependency build {{chart}} >/dev/null
    helm upgrade --install {{e2e_release}} {{chart}} -n "{{e2e_namespace}}" \
      -f {{dev_values}} -f e2e/values.yaml ${image_args[@]+"${image_args[@]}"} --wait --timeout 20m

# Run the Slack harness test against the deployed e2e stack.
e2e-test:
    #!/usr/bin/env bash
    set -euo pipefail
    export KUBECONFIG="$(mktemp)"
    kind get kubeconfig --name "{{e2e_cluster}}" > "$KUBECONFIG"
    kubectl -n "{{e2e_slack_namespace}}" port-forward svc/fake-slack "{{e2e_slack_port}}:443" >/dev/null &
    port_forward=$!
    trap 'kill "$port_forward"; rm -f "$KUBECONFIG"' EXIT
    for _ in {1..30}; do
      curl -fsS "http://127.0.0.1:{{e2e_slack_port}}/healthz" >/dev/null 2>&1 && break
      sleep 1
    done
    if ! E2E_SLACK_URL="http://127.0.0.1:{{e2e_slack_port}}" bun test ./e2e/slack-harness.test.ts; then
      kubectl get pods -A -o wide
      kubectl -n "{{e2e_namespace}}" logs "deploy/{{e2e_release}}-centaur-slackbotv2" --tail=100 || true
      kubectl -n "{{e2e_namespace}}" logs "deploy/{{e2e_release}}-centaur-api-rs" --tail=-1 \
        | grep -E '"level":"(WARN|ERROR)"' | tail -50 || true
      kubectl -n "{{e2e_slack_namespace}}" logs deploy/fake-slack --tail=100 || true
      exit 1
    fi

# Delete the e2e cluster.
e2e-down:
    kind delete cluster --name "{{e2e_cluster}}"
