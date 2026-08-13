#!/usr/bin/env bash
set -euo pipefail

chart=$(cd "$(dirname "$0")/.." && pwd)
tmp_dir=$(mktemp -d)
trap 'rm -rf "$tmp_dir"' EXIT

helm template centaur "$chart" >"$tmp_dir/default.yaml"
if grep -Eq 'centaur-(feishubot|workspace-provisioner|gitlab-publisher)' "$tmp_dir/default.yaml"; then
  echo "disabled development integrations rendered workloads or identities" >&2
  exit 1
fi

cat >"$tmp_dir/enabled.yaml" <<'YAML'
workspaceRepositories:
  enabled: true
  gitlab:
    baseUrl: http://git.example.test:82
    allowInsecureHttp: true
    egressCidrs: [192.0.2.44/32]
    secretRef: centaur-gitlab
    secretKey: token
    pageSize: 20
  storage:
    className: example-storage
    size: 20Gi
    accessMode: ReadWriteMany
gitlabPublishing:
  enabled: true
  secretRef: centaur-gitlab
  secretKey: token
feishubot:
  enabled: true
  secretRef: centaur-feishu
  appIdKey: app-id
  appSecretKey: app-secret
  botOpenIdKey: bot-open-id
  apiKeyKey: api-key
  allowedTenantKeys: [tenant-example]
console:
  publicUrl: https://console.example.test
  feishuLogin:
    enabled: true
    secretRef: centaur-feishu-login
    clientIdKey: app-id
    clientSecretKey: app-secret
    allowedTenantKeys: [tenant-example]
YAML

helm template centaur "$chart" -f "$tmp_dir/enabled.yaml" >"$tmp_dir/enabled-rendered.yaml"
ruby - "$tmp_dir/enabled-rendered.yaml" <<'RUBY'
require "yaml"

documents = YAML.load_stream(File.read(ARGV.fetch(0))).compact
find = ->(kind, name) do
  documents.find { |doc| doc["kind"] == kind && doc.dig("metadata", "name") == name }
end
container = ->(deployment, name) do
  deployment.dig("spec", "template", "spec", "containers").find { |item| item["name"] == name }
end
env_by_name = ->(item) { item.fetch("env").to_h { |entry| [entry.fetch("name"), entry] } }

feishu = find.call("Deployment", "centaur-centaur-feishubot") or abort "feishubot deployment missing"
abort "feishubot must use one replica" unless feishu.dig("spec", "replicas") == 1
abort "feishubot must use Recreate" unless feishu.dig("spec", "strategy", "type") == "Recreate"
feishu_container = container.call(feishu, "feishubot")
feishu_env = env_by_name.call(feishu_container)
%w[FEISHU_APP_ID FEISHU_APP_SECRET FEISHU_BOT_OPEN_ID FEISHUBOT_API_KEY].each do |name|
  abort "#{name} missing" unless feishu_env.dig(name, "valueFrom", "secretKeyRef", "name") == "centaur-feishu"
end
abort "China Feishu tenants missing" unless feishu_env.dig("FEISHU_TENANT_ALLOWLIST", "value") == "tenant-example"
abort "Console link missing" unless feishu_env.dig("CENTAUR_CONSOLE_PUBLIC_URL", "value") == "https://console.example.test"
abort "GitLab secret leaked to feishubot" if feishu.to_s.include?("centaur-gitlab")
%w[/health /ready /metrics].each do |path|
  abort "feishubot #{path} endpoint is not wired" unless feishu.to_s.include?(path)
end

console = find.call("Deployment", "centaur-centaur-console") or abort "Console deployment missing"
console_env = env_by_name.call(container.call(console, "console"))
abort "Feishu login client ID missing" unless console_env.dig("CENTAUR_CONSOLE_FEISHU_CLIENT_ID", "valueFrom", "secretKeyRef", "name") == "centaur-feishu-login"
abort "Feishu login secret missing" unless console_env.dig("CENTAUR_CONSOLE_FEISHU_CLIENT_SECRET", "valueFrom", "secretKeyRef", "name") == "centaur-feishu-login"
abort "Feishu login tenants missing" unless console_env.dig("CENTAUR_CONSOLE_FEISHU_ALLOWED_TENANT_KEYS", "value") == "tenant-example"
abort "GitLab token leaked to Console" if console.to_s.include?("centaur-gitlab")

api = find.call("Deployment", "centaur-centaur-api-rs") or abort "api-rs deployment missing"
api_container = container.call(api, "api-rs")
api_env = env_by_name.call(api_container)
expected = {
  "GITLAB_BASE_URL" => "http://git.example.test:82",
  "GITLAB_ALLOW_INSECURE_HTTP" => "true",
  "GITLAB_TOKEN_FILE" => "/var/run/secrets/centaur-gitlab/token",
  "GITLAB_CATALOG_PAGE_SIZE" => "20",
  "DEVELOPMENT_WORKSPACE_ENABLED" => "true",
  "DEVELOPMENT_WORKSPACE_STORAGE_ACCESS_MODE" => "ReadWriteMany",
  "DEVELOPMENT_WORKSPACE_PROVISIONER_IMAGE_PULL_POLICY" => "Always",
  "DEVELOPMENT_WORKSPACE_SERVICE_ACCOUNT" => "centaur-centaur-workspace-provisioner",
  "DEVELOPMENT_WORKSPACE_PUBLISHER_SERVICE_ACCOUNT" => "centaur-centaur-gitlab-publisher",
  "GITLAB_PUBLISHING_ENABLED" => "true",
}
expected.each { |name, value| abort "#{name} mismatch" unless api_env.dig(name, "value") == value }
abort "Feishu ingress key missing from api-rs" unless api_env.dig("FEISHUBOT_API_KEY", "valueFrom", "secretKeyRef", "name") == "centaur-feishu"
mount = api_container.fetch("volumeMounts").find { |item| item["mountPath"] == "/var/run/secrets/centaur-gitlab" }
abort "GitLab catalog token mount missing" unless mount && mount["readOnly"] == true
volume = api.dig("spec", "template", "spec", "volumes").find { |item| item["name"] == mount["name"] }
abort "GitLab token Secret volume missing" unless volume.dig("secret", "secretName") == "centaur-gitlab"
abort "GitLab token leaked through sandbox passthrough" if api_env.fetch("SESSION_SANDBOX_PASSTHROUGH_ENV").fetch("value").include?("GITLAB")

provisioner = find.call("ServiceAccount", "centaur-centaur-workspace-provisioner") or abort "provisioner ServiceAccount missing"
publisher = find.call("ServiceAccount", "centaur-centaur-gitlab-publisher") or abort "publisher ServiceAccount missing"
abort "provisioner and publisher identities must differ" if provisioner.dig("metadata", "name") == publisher.dig("metadata", "name")

role = find.call("Role", "centaur-centaur-api-rs-sandbox-manager") or abort "api-rs Role missing"
jobs = role.fetch("rules").find { |rule| rule.fetch("resources", []).include?("jobs") }
pvcs = role.fetch("rules").find { |rule| rule.fetch("resources", []).include?("persistentvolumeclaims") }
abort "api-rs cannot reconcile Jobs" unless jobs && %w[create delete get list watch].all? { |verb| jobs.fetch("verbs").include?(verb) }
abort "api-rs cannot reconcile workspace PVCs" unless pvcs && %w[create delete get list patch watch].all? { |verb| pvcs.fetch("verbs").include?(verb) }

api_policy = find.call("NetworkPolicy", "centaur-centaur-api-rs-egress") or abort "api-rs egress policy missing"
workspace_policy = find.call("NetworkPolicy", "centaur-centaur-development-gitlab-egress") or abort "workspace GitLab egress policy missing"
[api_policy, workspace_policy].each do |policy|
  rule = policy.dig("spec", "egress").find do |candidate|
    candidate.dig("to", 0, "ipBlock", "cidr") == "192.0.2.44/32" &&
      candidate.fetch("ports", []).any? { |entry| entry["port"] == 82 }
  end
  abort "exact GitLab IP:port egress missing" unless rule
end
feishu_policy = find.call("NetworkPolicy", "centaur-centaur-feishubot") or abort "feishubot NetworkPolicy missing"
abort "feishubot cannot reach China Feishu HTTPS" unless feishu_policy.dig("spec", "egress").any? { |rule| rule.fetch("ports", []).any? { |entry| entry["port"] == 443 } }
RUBY

cat >"$tmp_dir/review-only.yaml" <<'YAML'
workspaceRepositories:
  enabled: true
  gitlab:
    baseUrl: http://git.example.test:82
    allowInsecureHttp: true
    egressCidrs: [192.0.2.44/32]
    secretRef: centaur-gitlab
YAML
helm template centaur "$chart" -f "$tmp_dir/review-only.yaml" >"$tmp_dir/review-only-rendered.yaml"
ruby - "$tmp_dir/review-only-rendered.yaml" <<'RUBY'
require "yaml"

documents = YAML.load_stream(File.read(ARGV.fetch(0))).compact
abort "publisher identity rendered while publication is disabled" if documents.any? do |doc|
  doc["kind"] == "ServiceAccount" && doc.dig("metadata", "name") == "centaur-centaur-gitlab-publisher"
end
api = documents.find do |doc|
  doc["kind"] == "Deployment" && doc.dig("metadata", "name") == "centaur-centaur-api-rs"
end or abort "api-rs deployment missing"
container = api.dig("spec", "template", "spec", "containers").find { |item| item["name"] == "api-rs" }
env = container.fetch("env").to_h { |item| [item["name"], item["value"]] }
abort "publication was not explicitly disabled" unless env["GITLAB_PUBLISHING_ENABLED"] == "false"
abort "publisher ServiceAccount leaked into review-only config" if env.key?("DEVELOPMENT_WORKSPACE_PUBLISHER_SERVICE_ACCOUNT")
RUBY

assert_render_fails() {
  local name=$1
  local expected=$2
  if helm template centaur "$chart" -f "$tmp_dir/$name.yaml" >"$tmp_dir/$name.out" 2>"$tmp_dir/$name.err"; then
    echo "expected $name to fail Helm rendering" >&2
    exit 1
  fi
  grep -q "$expected" "$tmp_dir/$name.err"
}

cat >"$tmp_dir/missing-tenant.yaml" <<'YAML'
feishubot:
  enabled: true
  secretRef: centaur-feishu
YAML
assert_render_fails missing-tenant 'feishubot.allowedTenantKeys must contain at least one tenant key'

cat >"$tmp_dir/publishing-without-workspace.yaml" <<'YAML'
gitlabPublishing:
  enabled: true
  secretRef: centaur-gitlab
YAML
assert_render_fails publishing-without-workspace 'gitlabPublishing.enabled requires workspaceRepositories.enabled'

cat >"$tmp_dir/mismatched-token.yaml" <<'YAML'
workspaceRepositories:
  enabled: true
  gitlab:
    baseUrl: http://192.0.2.44:82
    allowInsecureHttp: true
    secretRef: centaur-gitlab-read
gitlabPublishing:
  enabled: true
  secretRef: centaur-gitlab-write
YAML
assert_render_fails mismatched-token 'must reference the same GitLab Secret'

cat >"$tmp_dir/invalid-gitlab.yaml" <<'YAML'
workspaceRepositories:
  enabled: true
  gitlab:
    baseUrl: ssh://git.example.test/group/project
    secretRef: centaur-gitlab
YAML
assert_render_fails invalid-gitlab 'workspaceRepositories.gitlab.baseUrl must be an HTTP(S) origin'

cat >"$tmp_dir/missing-egress-cidr.yaml" <<'YAML'
workspaceRepositories:
  enabled: true
  gitlab:
    baseUrl: https://git.example.test
    secretRef: centaur-gitlab
YAML
assert_render_fails missing-egress-cidr 'workspaceRepositories.gitlab.egressCidrs must contain at least one CIDR'

cat >"$tmp_dir/invalid-egress-cidr.yaml" <<'YAML'
workspaceRepositories:
  enabled: true
  gitlab:
    baseUrl: https://git.example.test
    egressCidrs: [192.0.2.999/32]
    secretRef: centaur-gitlab
YAML
assert_render_fails invalid-egress-cidr 'workspaceRepositories.gitlab.egressCidrs contains invalid CIDR'

cat >"$tmp_dir/unsupported-prefix.yaml" <<'YAML'
workspaceRepositories:
  enabled: true
  gitlab:
    baseUrl: http://192.0.2.44:82
    allowInsecureHttp: true
    egressCidrs: [192.0.2.44/32]
    secretRef: centaur-gitlab
gitlabPublishing:
  enabled: true
  secretRef: centaur-gitlab
  branchPrefix: automation
YAML
assert_render_fails unsupported-prefix 'gitlabPublishing.branchPrefix currently supports only centaur'

cat >"$tmp_dir/workspace-without-api.yaml" <<'YAML'
apiRs:
  enabled: false
workspaceRepositories:
  enabled: true
  gitlab:
    baseUrl: http://192.0.2.44:82
    allowInsecureHttp: true
    egressCidrs: [192.0.2.44/32]
    secretRef: centaur-gitlab
YAML
assert_render_fails workspace-without-api 'workspaceRepositories.enabled requires apiRs.enabled'

cat >"$tmp_dir/workspace-wrong-backend.yaml" <<'YAML'
apiRs:
  sandboxBackend: local
workspaceRepositories:
  enabled: true
  gitlab:
    baseUrl: http://192.0.2.44:82
    allowInsecureHttp: true
    secretRef: centaur-gitlab
YAML
assert_render_fails workspace-wrong-backend 'workspaceRepositories.enabled requires apiRs.sandboxBackend=agent-k8s'

cat >"$tmp_dir/invalid-gitlab-port.yaml" <<'YAML'
workspaceRepositories:
  enabled: true
  gitlab:
    baseUrl: http://192.0.2.44:70000
    allowInsecureHttp: true
    secretRef: centaur-gitlab
YAML
assert_render_fails invalid-gitlab-port 'workspaceRepositories.gitlab.baseUrl port must be between 1 and 65535'

cat >"$tmp_dir/insecure-http.yaml" <<'YAML'
workspaceRepositories:
  enabled: true
  gitlab:
    baseUrl: http://git.example.test:82
    egressCidrs: [192.0.2.44/32]
    secretRef: centaur-gitlab
YAML
assert_render_fails insecure-http 'workspaceRepositories.gitlab.allowInsecureHttp must be true'

echo "development workflow Helm assertions passed"
