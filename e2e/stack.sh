#!/usr/bin/env bash
# Brings up the e2e stack on a dedicated kind cluster and runs the scenarios.
#
#   e2e/stack.sh up     create/update the cluster and deploy everything
#   e2e/stack.sh test   run `bun test e2e/` against the deployed stack
#   e2e/stack.sh down   delete the cluster
#
# The stack is the chart's Slack path (Postgres, console, proxy-sync, api-rs,
# slackbotv2, sandboxes, per-sandbox iron-proxies) plus two fakes for the
# outside world: Slack, and the model providers, which CoreDNS resolves to an
# in-cluster scripted model server.
#
# Images are built from the working tree and loaded into kind by default. Set
# CENTAUR_E2E_IMAGE_TAG (e.g. main or sha-abc1234) to pull published images.
# Every command uses a private kubeconfig, never the ambient context.
set -euo pipefail

cd "$(dirname "$0")/.."

CLUSTER="${CENTAUR_E2E_CLUSTER:-centaur-e2e}"
RELEASE=centaur # contrib/scripts/bootstrap-k8s-secrets.sh assumes this name
NAMESPACE=centaur
SLACK_NAMESPACE=centaur-e2e-slack
MODEL_NAMESPACE=centaur-e2e-model
# Fixture values, not credentials: the fakes and the stack must agree on them.
SLACK_BOT_TOKEN=xoxb-centaur-e2e
SLACK_SIGNING_SECRET=centaur-e2e-signing-secret
OPENAI_TEST_KEY=sk-e2e-openai-test-key
ANTHROPIC_TEST_KEY=sk-ant-e2e-anthropic-test-key
IMAGES=(api-rs:apiRs proxy-sync:proxySync iron-proxy:ironProxy slackbotv2:slackbotv2 agent:sandbox console:console)

export KUBECONFIG
KUBECONFIG="$(mktemp)"
WORK="$(mktemp -d)"
FORWARDS=()
trap 'kill ${FORWARDS[@]+"${FORWARDS[@]}"} 2> /dev/null; rm -rf "$KUBECONFIG" "$WORK"' EXIT

use_cluster() {
  kind get kubeconfig --name "$CLUSTER" > "$KUBECONFIG"
}

apply_stdin() {
  kubectl apply -f - > /dev/null
}

up() {
  if kind get clusters | grep -qx "$CLUSTER"; then
    use_cluster
  else
    kind create cluster --name "$CLUSTER" --kubeconfig "$KUBECONFIG" --wait 120s
  fi

  kubectl -n kube-system create configmap coredns --from-file=Corefile=e2e/Corefile \
    --dry-run=client -o yaml | apply_stdin
  kubectl -n kube-system rollout restart deploy/coredns > /dev/null

  local image_args=() entry service key
  for entry in "${IMAGES[@]}"; do
    service="${entry%%:*}" key="${entry#*:}"
    if [[ -n "${CENTAUR_E2E_IMAGE_TAG:-}" ]]; then
      image_args+=(
        --set "${key}.image.repository=ghcr.io/paradigmxyz/centaur/centaur-${service}"
        --set "${key}.image.tag=${CENTAUR_E2E_IMAGE_TAG}"
      )
      # Pull the multi-GB agent image now, not inside a sandbox's ready timeout.
      if [[ "$service" == agent ]]; then
        docker exec "${CLUSTER}-control-plane" crictl pull \
          "ghcr.io/paradigmxyz/centaur/centaur-agent:${CENTAUR_E2E_IMAGE_TAG}"
      fi
    else
      just build-one "$service"
      kind load docker-image --name "$CLUSTER" "centaur-${service}:latest"
    fi
  done

  SLACK_BOT_TOKEN="$SLACK_BOT_TOKEN" \
  SLACK_SIGNING_SECRET="$SLACK_SIGNING_SECRET" \
  SLACKBOT_API_KEY="$(openssl rand -hex 32)" \
  OP_SERVICE_ACCOUNT_TOKEN=unused OP_VAULT=unused \
    contrib/scripts/bootstrap-k8s-secrets.sh --namespace "$NAMESPACE"
  # iron-proxy's env secret source resolves the provider keys from here.
  kubectl -n "$NAMESPACE" patch secret centaur-infra-env --type merge -p \
    "{\"stringData\":{\"OPENAI_API_KEY\":\"$OPENAI_TEST_KEY\",\"ANTHROPIC_API_KEY\":\"$ANTHROPIC_TEST_KEY\"}}" > /dev/null
  # iron-proxy pods mount the release CA at this path; trusting it lets them
  # verify the model server, whose certificate the same CA signs below.
  kubectl -n "$NAMESPACE" create secret generic centaur-e2e-iron-proxy-env \
    --from-literal=SSL_CERT_FILE=/etc/iron-proxy-ca/ca-cert.pem \
    --dry-run=client -o yaml | apply_stdin

  deploy_model_server
  deploy_fake_slack

  # An interrupted install leaves the release locked as pending; start it over.
  if helm status "$RELEASE" -n "$NAMESPACE" -o json 2> /dev/null | grep -q '"status":"pending-'; then
    helm uninstall "$RELEASE" -n "$NAMESPACE" --wait > /dev/null
  fi
  helm dependency build contrib/chart > /dev/null
  helm upgrade --install "$RELEASE" contrib/chart -n "$NAMESPACE" \
    -f contrib/chart/values.dev.yaml -f e2e/values.yaml \
    ${image_args[@]+"${image_args[@]}"} --wait --timeout 20m
}

deploy_model_server() {
  kubectl -n "$NAMESPACE" get secret centaur-firewall-ca-key -o jsonpath='{.data.ca-cert\.pem}' \
    | base64 -d > "$WORK/ca.pem"
  kubectl -n "$NAMESPACE" get secret centaur-firewall-ca-key -o jsonpath='{.data.ca-key\.pem}' \
    | base64 -d > "$WORK/ca-key.pem"
  openssl req -new -newkey rsa:2048 -nodes -subj /CN=api.openai.com \
    -keyout "$WORK/tls.key" -out "$WORK/tls.csr" 2> /dev/null
  printf 'subjectAltName=DNS:api.openai.com,DNS:api.anthropic.com\nextendedKeyUsage=serverAuth\n' \
    > "$WORK/ext.cnf"
  openssl x509 -req -in "$WORK/tls.csr" -CA "$WORK/ca.pem" -CAkey "$WORK/ca-key.pem" \
    -CAcreateserial -days 30 -sha256 -extfile "$WORK/ext.cnf" -out "$WORK/tls.crt" 2> /dev/null

  kubectl create namespace "$MODEL_NAMESPACE" --dry-run=client -o yaml | apply_stdin
  kubectl -n "$MODEL_NAMESPACE" create secret tls model-server-tls \
    --cert="$WORK/tls.crt" --key="$WORK/tls.key" --dry-run=client -o yaml | apply_stdin
  kubectl -n "$MODEL_NAMESPACE" create configmap model-server \
    --from-file=e2e/model-server.ts --dry-run=client -o yaml | apply_stdin
  kubectl apply -f e2e/model-server.yaml > /dev/null
  kubectl -n "$MODEL_NAMESPACE" rollout restart deploy/model-server > /dev/null
  kubectl -n "$MODEL_NAMESPACE" rollout status deploy/model-server --timeout=180s
}

deploy_fake_slack() {
  kubectl create namespace "$SLACK_NAMESPACE" --dry-run=client -o yaml | apply_stdin
  kubectl -n "$SLACK_NAMESPACE" create configmap fake-slack \
    --from-file=e2e/fake-slack.ts --from-file=e2e/fixture.ts --dry-run=client -o yaml | apply_stdin
  kubectl -n "$SLACK_NAMESPACE" create secret generic fake-slack \
    --from-literal=SLACK_BOT_TOKEN="$SLACK_BOT_TOKEN" \
    --from-literal=SLACK_SIGNING_SECRET="$SLACK_SIGNING_SECRET" \
    --from-literal=SLACK_EVENTS_URL="http://${RELEASE}-centaur-slackbotv2.${NAMESPACE}.svc.cluster.local:3001/api/webhooks/slack" \
    --dry-run=client -o yaml | apply_stdin
  kubectl -n "$SLACK_NAMESPACE" apply -f e2e/fake-slack.yaml > /dev/null
  kubectl -n "$SLACK_NAMESPACE" rollout restart deploy/fake-slack > /dev/null
  kubectl -n "$SLACK_NAMESPACE" rollout status deploy/fake-slack --timeout=180s
}

run_tests() {
  use_cluster
  forward E2E_SLACK_URL "$SLACK_NAMESPACE" svc/fake-slack 443
  forward E2E_MODEL_URL "$MODEL_NAMESPACE" svc/model-server 8080
  export E2E_SLACK_URL E2E_MODEL_URL
  export E2E_NAMESPACE="$NAMESPACE" E2E_RELEASE="$RELEASE" E2E_OPENAI_KEY="$OPENAI_TEST_KEY"
  if ! bun test ./e2e "$@"; then
    kubectl get pods -A -o wide
    kubectl -n "$NAMESPACE" logs "deploy/${RELEASE}-centaur-slackbotv2" --tail=100 || true
    kubectl -n "$NAMESPACE" logs "deploy/${RELEASE}-centaur-api-rs" --tail=-1 \
      | grep -E '"level":"(WARN|ERROR)"' | tail -50 || true
    kubectl -n "$MODEL_NAMESPACE" logs deploy/model-server --tail=100 || true
    kubectl -n "$SLACK_NAMESPACE" logs deploy/fake-slack --tail=100 || true
    return 1
  fi
}

# Port-forwards to a free local port and stores the base URL in variable $1.
forward() {
  local var="$1" namespace="$2" target="$3" port="$4" out="$WORK/forward-${3//\//-}" local_port="" _
  kubectl -n "$namespace" port-forward "$target" ":$port" > "$out" 2>&1 &
  FORWARDS+=("$!")
  for _ in {1..30}; do
    local_port="$(sed -n 's/^Forwarding from 127.0.0.1:\([0-9]*\) .*/\1/p' "$out")"
    if [[ -n "$local_port" ]] && curl -fsS "http://127.0.0.1:$local_port/healthz" > /dev/null 2>&1; then
      printf -v "$var" 'http://127.0.0.1:%s' "$local_port"
      return 0
    fi
    sleep 1
  done
  echo "port-forward to $namespace/$target failed: $(cat "$out")" >&2
  return 1
}

case "${1:-}" in
  up) up ;;
  test) shift; run_tests "$@" ;;
  down) kind delete cluster --name "$CLUSTER" ;;
  *) echo "usage: $0 up|test|down" >&2; exit 2 ;;
esac
