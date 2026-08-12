#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/../../.." && pwd)"
chart="$repo_root/contrib/chart"
tmp_dir="$(mktemp -d)"
trap 'rm -rf "$tmp_dir"' EXIT

cat >"$tmp_dir/values.yaml" <<'YAML'
toolServer:
  enabled: false
repoCache:
  enabled: true
  repositories:
    - repo: group/project
      cloneUrl: http://git.example.test:82/group/project.git
      ref: main
      visibility: private
  gitCredentials:
    username: oauth2
    existingSecretName: gitlab-repo-token
    secretKey: token
  egressPorts: [82, 443]
overlays:
  sources:
    - repo: group/project
      cloneUrl: http://git.example.test:82/group/project.git
      ref: main
      visibility: private
      toolsSubdir: tools
      workflowsSubdir: ""
      skillsSubdir: ""
YAML

helm template centaur "$chart" -f "$tmp_dir/values.yaml" >"$tmp_dir/rendered.yaml"

ruby - "$tmp_dir/rendered.yaml" <<'RUBY'
require "json"
require "yaml"

documents = YAML.load_stream(File.read(ARGV.fetch(0))).compact
repo_cache = documents.find do |doc|
  %w[DaemonSet Deployment].include?(doc["kind"]) &&
    doc.dig("metadata", "name") == "centaur-centaur-repo-cache"
end or abort "repo-cache workload missing"
container = repo_cache.dig("spec", "template", "spec", "containers").find { |item| item["name"] == "repo-cache" }
env = container.fetch("env").to_h { |item| [item["name"], item["value"]] }
clone_urls = JSON.parse(env.fetch("REPOSITORY_CLONE_URLS"))
abort "custom clone URL missing" unless clone_urls == {
  "group/project" => "http://git.example.test:82/group/project.git",
}
abort "generic username missing" unless env["GIT_USERNAME"] == "oauth2"
abort "generic token path missing" unless env["GIT_TOKEN_FILE"] == "/git-credentials/token"

mount = container.fetch("volumeMounts").find { |item| item["mountPath"] == "/git-credentials" }
abort "credential mount missing" unless mount && mount["readOnly"] == true
volume = repo_cache.dig("spec", "template", "spec", "volumes").find { |item| item["name"] == mount["name"] }
abort "credential secret missing" unless volume.dig("secret", "secretName") == "gitlab-repo-token"

api = documents.find do |doc|
  doc["kind"] == "Deployment" && doc.dig("metadata", "name") == "centaur-centaur-api-rs"
end or abort "api-rs deployment missing"
api_container = api.dig("spec", "template", "spec", "containers").find { |item| item["name"] == "api-rs" }
api_env = api_container.fetch("env").to_h { |item| [item["name"], item["value"]] }
abort "api-rs clone URL missing" unless api_env["KUBERNETES_TOOLS_CLONE_URL"] == "http://git.example.test:82/group/project.git"

policy = documents.find do |doc|
  doc["kind"] == "NetworkPolicy" && doc.dig("metadata", "name") == "centaur-centaur-repo-cache"
end or abort "repo-cache network policy missing"
ports = policy.fetch("spec").fetch("egress").flat_map { |rule| rule.fetch("ports", []) }.map { |entry| entry["port"] }
abort "custom egress port missing" unless ports.include?(82)
RUBY

cat >"$tmp_dir/direct.yaml" <<'YAML'
repoCache:
  enabled: false
toolServer:
  enabled: true
  repo: group/tools
  cloneUrl: http://git.example.test:82/group/tools.git
  gitCredentials:
    username: deploy-user
    existingSecretName: direct-git-token
    secretKey: password
YAML

helm template centaur "$chart" -f "$tmp_dir/direct.yaml" >"$tmp_dir/direct-rendered.yaml"
ruby - "$tmp_dir/direct-rendered.yaml" <<'RUBY'
require "yaml"

documents = YAML.load_stream(File.read(ARGV.fetch(0))).compact
api = documents.find do |doc|
  doc["kind"] == "Deployment" && doc.dig("metadata", "name") == "centaur-centaur-api-rs"
end or abort "api-rs deployment missing"
container = api.dig("spec", "template", "spec", "containers").find { |item| item["name"] == "api-rs" }
env = container.fetch("env").to_h { |item| [item["name"], item["value"]] }
abort "direct clone URL missing" unless env["KUBERNETES_TOOLS_CLONE_URL"] == "http://git.example.test:82/group/tools.git"
abort "direct credential secret missing" unless env["KUBERNETES_TOOLS_GIT_CREDENTIALS_SECRET"] == "direct-git-token"
abort "direct credential key missing" unless env["KUBERNETES_TOOLS_GIT_CREDENTIALS_SECRET_KEY"] == "password"
abort "direct credential username missing" unless env["KUBERNETES_TOOLS_GIT_USERNAME"] == "deploy-user"
mount = container.fetch("volumeMounts").find { |item| item["mountPath"] == "/tools-git-credentials" }
abort "direct credential mount missing" unless mount && mount["readOnly"] == true
volume = api.dig("spec", "template", "spec", "volumes").find { |item| item["name"] == mount["name"] }
abort "direct credential volume missing" unless volume.dig("secret", "secretName") == "direct-git-token"
RUBY

cat >"$tmp_dir/legacy.yaml" <<'YAML'
repoCache:
  enabled: true
  repositories: [example/legacy]
  githubToken:
    existingSecretName: legacy-github-token
    secretKey: github-token
toolServer:
  enabled: false
YAML

helm template centaur "$chart" -f "$tmp_dir/legacy.yaml" >"$tmp_dir/legacy-rendered.yaml"
ruby - "$tmp_dir/legacy-rendered.yaml" <<'RUBY'
require "yaml"

documents = YAML.load_stream(File.read(ARGV.fetch(0))).compact
workload = documents.find do |doc|
  %w[DaemonSet Deployment].include?(doc["kind"]) && doc.dig("metadata", "name") == "centaur-centaur-repo-cache"
end or abort "legacy repo-cache workload missing"
container = workload.dig("spec", "template", "spec", "containers").find { |item| item["name"] == "repo-cache" }
env = container.fetch("env").to_h { |item| [item["name"], item["value"]] }
abort "legacy username changed" unless env["GIT_USERNAME"] == "x-access-token"
volume = workload.dig("spec", "template", "spec", "volumes").find { |item| item["name"] == "git-credentials" }
abort "legacy secret name changed" unless volume.dig("secret", "secretName") == "legacy-github-token"
abort "legacy secret key changed" unless volume.dig("secret", "items", 0, "key") == "github-token"
RUBY

cat >"$tmp_dir/conflict.yaml" <<'YAML'
toolServer:
  enabled: false
repoCache:
  enabled: true
  repositories:
    - repo: group/project
      cloneUrl: http://git-a.example.test/group/project.git
overlays:
  sources:
    - repo: group/project
      cloneUrl: http://git-b.example.test/group/project.git
YAML

if helm template centaur "$chart" -f "$tmp_dir/conflict.yaml" >"$tmp_dir/conflict.out" 2>"$tmp_dir/conflict.err"; then
  echo "expected conflicting clone URLs to fail Helm rendering" >&2
  exit 1
fi
grep -q "conflicting cloneUrl values for repo group/project" "$tmp_dir/conflict.err"
