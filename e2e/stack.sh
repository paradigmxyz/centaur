#!/usr/bin/env bash
# Brings up the e2e stack on a dedicated kind cluster and runs the scenarios.
#
#   e2e/stack.sh up            create/update the cluster and deploy everything
#   e2e/stack.sh test [files]  run the e2e scenarios (or the given files)
#   e2e/stack.sh logs          print pods, events, and component logs
#   e2e/stack.sh down          delete the cluster
#
# The stack is the chart's Slack path (Postgres, console, proxy-sync, api-rs,
# slackbotv2, sandboxes, per-sandbox iron-proxies) plus two fakes for the
# outside world: Slack, and the model providers, which CoreDNS resolves to an
# in-cluster scripted model server. The test runner reaches the fakes,
# Postgres, and api-rs on localhost through NodePorts mapped by e2e/infra/kind.yaml.
#
# Images are built from the working tree and loaded into kind by default. Set
# CENTAUR_E2E_IMAGE_TAG (e.g. main or sha-abc1234) to pull published images.
#
# CENTAUR_E2E_AUTH_MODE picks how the harnesses authenticate, for both Codex
# and Claude Code: api_key (default) or access_token, the ChatGPT and Claude.ai
# subscription path, where the console brokers the tokens iron-proxy injects.
# `test` runs against whichever mode the stack was last brought up in; CI runs
# only e2e/tests/harnesses.test.ts in access_token mode. Going
# from access_token back to api_key needs `down` first: api-rs does not remove
# the subscription credentials it registered, so they would still be injected.
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
# Admin bearer for api-rs operator routes the tests call.
API_ADMIN_KEY=centaur-e2e-api-admin-key
OPENAI_TEST_KEY=sk-e2e-openai-test-key
ANTHROPIC_TEST_KEY=sk-ant-e2e-anthropic-test-key
# Subscription access tokens as the console's broker would mint them, and the
# ChatGPT workspace the e2e subscription belongs to.
CODEX_ACCESS_TOKEN=e2e-chatgpt-access-token
CLAUDE_ACCESS_TOKEN=e2e-claude-access-token
CHATGPT_ACCOUNT_ID=e2e00000-0000-4000-8000-00000000c0de
AUTH_MODE="${CENTAUR_E2E_AUTH_MODE:-api_key}"
case "$AUTH_MODE" in
  api_key | access_token) ;;
  *) echo "CENTAUR_E2E_AUTH_MODE must be api_key or access_token, not $AUTH_MODE" >&2; exit 2 ;;
esac
IMAGES=(api-rs:apiRs proxy-sync:proxySync iron-proxy:ironProxy slackbotv2:slackbotv2 agent:sandbox console:console)

export KUBECONFIG
KUBECONFIG="$(mktemp)"
WORK="$(mktemp -d)"
trap 'rm -rf "$KUBECONFIG" "$WORK"' EXIT

use_cluster() {
  kind get kubeconfig --name "$CLUSTER" > "$KUBECONFIG"
}

apply_stdin() {
  kubectl apply -f - > /dev/null
}

up() {
  if kind get clusters | grep -qx "$CLUSTER"; then
    use_cluster
    if ! docker port "${CLUSTER}-control-plane" | grep -q '^30081/tcp'; then
      echo "cluster $CLUSTER predates e2e/infra/kind.yaml; run e2e/stack.sh down first" >&2
      exit 1
    fi
  else
    kind create cluster --name "$CLUSTER" --config e2e/infra/kind.yaml --kubeconfig "$KUBECONFIG" --wait 120s
  fi

  # Pods inherit the host's DNS search domains, so every external lookup first
  # tries names like api.openai.com.<host-domain> through the host's resolver,
  # which is slow under load. resolv.conf is bind-mounted, so write in place.
  docker exec "${CLUSTER}-control-plane" sh -c \
    "grep -v '^search ' /etc/resolv.conf > /tmp/resolv.conf && cat /tmp/resolv.conf > /etc/resolv.conf"

  kubectl -n kube-system create configmap coredns --from-file=Corefile=e2e/infra/Corefile \
    --dry-run=client -o yaml | apply_stdin
  kubectl -n kube-system rollout restart deploy/coredns > /dev/null

  local image_args=() entry service key tag
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
      # Tag by content so a rebuilt image changes the release values and Helm
      # rolls exactly the pods whose image changed.
      tag="e2e-$(docker image inspect -f '{{.Id}}' "centaur-${service}:latest" | cut -c8-19)"
      docker tag "centaur-${service}:latest" "centaur-${service}:${tag}"
      kind load docker-image --name "$CLUSTER" "centaur-${service}:${tag}"
      image_args+=(
        --set "${key}.image.repository=centaur-${service}"
        --set "${key}.image.tag=${tag}"
      )
    fi
  done

  SLACK_BOT_TOKEN="$SLACK_BOT_TOKEN" \
  SLACK_SIGNING_SECRET="$SLACK_SIGNING_SECRET" \
  SLACKBOT_API_KEY="$(openssl rand -hex 32)" \
  OP_SERVICE_ACCOUNT_TOKEN=unused OP_VAULT=unused \
    contrib/scripts/bootstrap-k8s-secrets.sh --namespace "$NAMESPACE"
  # iron-proxy's env secret source resolves the provider keys from here.
  kubectl -n "$NAMESPACE" patch secret centaur-infra-env --type merge -p \
    "{\"stringData\":{\"OPENAI_API_KEY\":\"$OPENAI_TEST_KEY\",\"ANTHROPIC_API_KEY\":\"$ANTHROPIC_TEST_KEY\",\"OPENAI_CODEX_ACCOUNT_ID\":\"$CHATGPT_ACCOUNT_ID\",\"CENTAUR_APIRS_ADMIN_API_KEY\":\"$API_ADMIN_KEY\"}}" > /dev/null
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
  # The chart's 1Password Connect dependency resolves only from a registered repo.
  helm repo add onepassword https://1password.github.io/connect-helm-charts --force-update > /dev/null
  helm dependency build contrib/chart > /dev/null
  helm upgrade --install "$RELEASE" contrib/chart -n "$NAMESPACE" \
    -f contrib/chart/values.dev.yaml -f e2e/infra/values.yaml \
    --set "sandbox.codexAuthMode=${AUTH_MODE}" --set "sandbox.claudeCodeAuthMode=${AUTH_MODE}" \
    ${image_args[@]+"${image_args[@]}"} --wait --timeout 20m &
  local helm_pid=$!
  # In access_token mode api-rs refuses to start until the console holds the
  # broker credentials, and the console comes up in the same release.
  if [[ "$AUTH_MODE" == access_token ]]; then
    seed_broker_credentials
  fi
  wait "$helm_pid"
  kubectl apply -f e2e/infra/test-access.yaml > /dev/null
  wait_for_test_access
}

# The NodePorts and the policy admitting them take a moment to start routing;
# until then connections are reset, so wait before declaring the stack up.
wait_for_test_access() {
  for _ in $(seq 60); do
    if curl -fsS -o /dev/null --max-time 2 http://127.0.0.1:30081/readyz 2> /dev/null &&
      timeout 2 bash -c '< /dev/tcp/127.0.0.1/30432' 2> /dev/null; then
      return 0
    fi
    sleep 1
  done
  echo "api-rs and Postgres NodePorts did not become reachable" >&2
  return 1
}

# Stores the subscription access tokens as the console's broker would after a
# refresh. Without a refresh token the console never tries to refresh them.
seed_broker_credentials() {
  local console="deploy/${RELEASE}-centaur-console"
  until kubectl -n "$NAMESPACE" get "$console" > /dev/null 2>&1; do sleep 2; done
  kubectl -n "$NAMESPACE" rollout status "$console" --timeout=15m
  kubectl -n "$NAMESPACE" exec "$console" -- bin/rails runner "
    { 'openai-codex' => '$CODEX_ACCESS_TOKEN', 'anthropic-claude' => '$CLAUDE_ACCESS_TOKEN' }.each do |id, token|
      BrokerCredential.find_or_initialize_by(foreign_id: id).update!(
        token_endpoint: 'https://oauth.invalid/token', client_id: 'centaur-e2e',
        access_token: token, last_refresh: Time.current, expires_at: 1.year.from_now)
    end"
  # Skip the crash-loop backoff api-rs built up while it waited.
  kubectl -n "$NAMESPACE" delete pod -l app.kubernetes.io/component=api-rs --wait=false > /dev/null
}

deploy_model_server() {
  kubectl -n "$NAMESPACE" get secret centaur-firewall-ca-key -o jsonpath='{.data.ca-cert\.pem}' \
    | base64 -d > "$WORK/ca.pem"
  kubectl -n "$NAMESPACE" get secret centaur-firewall-ca-key -o jsonpath='{.data.ca-key\.pem}' \
    | base64 -d > "$WORK/ca-key.pem"
  openssl req -new -newkey rsa:2048 -nodes -subj /CN=api.openai.com \
    -keyout "$WORK/tls.key" -out "$WORK/tls.csr" 2> /dev/null
  printf 'subjectAltName=DNS:api.openai.com,DNS:chatgpt.com,DNS:api.anthropic.com\nextendedKeyUsage=serverAuth\n' \
    > "$WORK/ext.cnf"
  openssl x509 -req -in "$WORK/tls.csr" -CA "$WORK/ca.pem" -CAkey "$WORK/ca-key.pem" \
    -CAcreateserial -days 30 -sha256 -extfile "$WORK/ext.cnf" -out "$WORK/tls.crt" 2> /dev/null

  kubectl create namespace "$MODEL_NAMESPACE" --dry-run=client -o yaml | apply_stdin
  kubectl -n "$MODEL_NAMESPACE" create secret tls model-server-tls \
    --cert="$WORK/tls.crt" --key="$WORK/tls.key" --dry-run=client -o yaml | apply_stdin
  kubectl -n "$MODEL_NAMESPACE" create secret generic model-server \
    --from-literal=CHATGPT_ACCOUNT_ID="$CHATGPT_ACCOUNT_ID" --dry-run=client -o yaml | apply_stdin
  kubectl -n "$MODEL_NAMESPACE" create configmap model-server \
    --from-file=e2e/fakes/model-server.ts --dry-run=client -o yaml | apply_stdin
  kubectl apply -f e2e/infra/model-server.yaml > /dev/null
  kubectl -n "$MODEL_NAMESPACE" rollout restart deploy/model-server > /dev/null
  kubectl -n "$MODEL_NAMESPACE" rollout status deploy/model-server --timeout=180s
}

deploy_fake_slack() {
  kubectl create namespace "$SLACK_NAMESPACE" --dry-run=client -o yaml | apply_stdin
  kubectl -n "$SLACK_NAMESPACE" create configmap fake-slack \
    --from-file=e2e/fakes/fake-slack.ts --from-file=e2e/fakes/slack-fixture.ts --dry-run=client -o yaml | apply_stdin
  kubectl -n "$SLACK_NAMESPACE" create secret generic fake-slack \
    --from-literal=SLACK_BOT_TOKEN="$SLACK_BOT_TOKEN" \
    --from-literal=SLACK_SIGNING_SECRET="$SLACK_SIGNING_SECRET" \
    --from-literal=SLACK_EVENTS_URL="http://${RELEASE}-centaur-slackbotv2.${NAMESPACE}.svc.cluster.local:3001/api/webhooks/slack" \
    --dry-run=client -o yaml | apply_stdin
  kubectl -n "$SLACK_NAMESPACE" apply -f e2e/infra/fake-slack.yaml > /dev/null
  kubectl -n "$SLACK_NAMESPACE" rollout restart deploy/fake-slack > /dev/null
  kubectl -n "$SLACK_NAMESPACE" rollout status deploy/fake-slack --timeout=180s
}

run_tests() {
  use_cluster
  remove_sandboxes
  local password
  password="$(kubectl -n "$NAMESPACE" get secret centaur-infra-env -o jsonpath='{.data.POSTGRES_PASSWORD}' | base64 -d)"
  export E2E_SLACK_URL=http://127.0.0.1:30443
  export E2E_MODEL_URL=http://127.0.0.1:30080
  export E2E_DATABASE_URL="postgres://tempo:${password}@127.0.0.1:30432/ai_v2"
  # The console names its database after the Rails environment (contrib/chart/templates/console.yaml).
  export E2E_IRON_CONTROL_DATABASE_URL="postgres://tempo:${password}@127.0.0.1:30432/iron_control_production"
  export E2E_API_URL=http://127.0.0.1:30081 E2E_API_KEY="$API_ADMIN_KEY"
  # Expect the credentials of the auth mode the stack runs in.
  E2E_AUTH_MODE="$(kubectl -n "$NAMESPACE" get "deploy/${RELEASE}-centaur-api-rs" \
    -o jsonpath='{.spec.template.spec.containers[0].env[?(@.name=="CODEX_AUTH_MODE")].value}')"
  export E2E_AUTH_MODE
  if [[ "$E2E_AUTH_MODE" == access_token ]]; then
    export E2E_OPENAI_CREDENTIAL="Bearer $CODEX_ACCESS_TOKEN" E2E_ANTHROPIC_CREDENTIAL="Bearer $CLAUDE_ACCESS_TOKEN"
    export E2E_CHATGPT_ACCOUNT_ID="$CHATGPT_ACCOUNT_ID"
  else
    export E2E_OPENAI_CREDENTIAL="Bearer $OPENAI_TEST_KEY" E2E_ANTHROPIC_CREDENTIAL="$ANTHROPIC_TEST_KEY"
  fi
  # A wider burst of cold sandboxes overloads the single kind node.
  # Arguments pick test files (e.g. e2e/tests/lifecycle.test.ts); default is all.
  local targets=("$@")
  [[ "${#targets[@]}" -gt 0 ]] || targets=(./e2e/tests)
  if ! bun test --max-concurrency=4 "${targets[@]}"; then
    dump_logs
    return 1
  fi
}

# Prints what is needed to debug a failed deploy or test run.
dump_logs() {
  kubectl get pods -A -o wide || true
  kubectl get events -A --sort-by=.lastTimestamp | tail -50 || true
  kubectl -n "$NAMESPACE" logs "deploy/${RELEASE}-centaur-slackbotv2" --tail=-1 \
    | grep -E '"level":"(warn|error)"' | tail -100 || true
  # A container that restarted took its logs with it; show how it ended.
  kubectl -n "$NAMESPACE" logs "deploy/${RELEASE}-centaur-slackbotv2" --previous --tail=50 2> /dev/null || true
  kubectl -n "$NAMESPACE" logs "deploy/${RELEASE}-centaur-api-rs" --previous --tail=50 2> /dev/null || true
  kubectl -n "$NAMESPACE" logs "deploy/${RELEASE}-centaur-api-rs" --tail=-1 \
    | grep -E '"level":"(WARN|ERROR)"' | tail -50 || true
  kubectl -n "$MODEL_NAMESPACE" logs deploy/model-server --tail=100 || true
  kubectl -n "$SLACK_NAMESPACE" logs deploy/fake-slack --tail=-1 | grep -v '_delivered"' | tail -100 || true
}

# Earlier runs leave a sandbox and iron-proxy per thread (they idle for hours).
# Remove them so each run starts on an unloaded node.
remove_sandboxes() {
  kubectl -n "$NAMESPACE" delete sandboxes.agents.x-k8s.io --all --wait=false > /dev/null
  kubectl -n "$NAMESPACE" delete pods,services,networkpolicies,configmaps,secrets \
    -l centaur.ai/managed-by=api-rs --wait=false > /dev/null
}

case "${1:-}" in
  up) up ;;
  test) shift; run_tests "$@" ;;
  logs) use_cluster; dump_logs ;;
  down) kind delete cluster --name "$CLUSTER" ;;
  *) echo "usage: $0 up|test|logs|down" >&2; exit 2 ;;
esac
