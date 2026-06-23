{{- define "centaur.name" -}}
{{- default .Chart.Name .Values.nameOverride | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{- define "centaur.fullname" -}}
{{- if .Values.fullnameOverride -}}
{{- .Values.fullnameOverride | trunc 63 | trimSuffix "-" -}}
{{- else -}}
{{- printf "%s-%s" .Release.Name (include "centaur.name" .) | trunc 63 | trimSuffix "-" -}}
{{- end -}}
{{- end -}}

{{- define "centaur.chart" -}}
{{- printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" -}}
{{- end -}}

{{- define "centaur.labels" -}}
helm.sh/chart: {{ include "centaur.chart" . }}
{{ include "centaur.selectorLabels" . }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
{{- end -}}

{{- define "centaur.selectorLabels" -}}
app.kubernetes.io/name: {{ include "centaur.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end -}}

{{- define "centaur.componentLabels" -}}
{{ include "centaur.labels" .root }}
app.kubernetes.io/component: {{ .component }}
{{- end -}}

{{- define "centaur.componentSelectorLabels" -}}
{{ include "centaur.selectorLabels" .root }}
app.kubernetes.io/component: {{ .component }}
{{- end -}}

{{- define "centaur.componentName" -}}
{{- printf "%s-%s" (include "centaur.fullname" .root) .component | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{- define "centaur.secretEnvName" -}}
{{- required "secretManager.existingSecretName is required" .Values.secretManager.existingSecretName | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{- define "centaur.trustedCaSecretName" -}}
{{- required "firewall.existingCaSecretName is required" .Values.firewall.existingCaSecretName | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{- define "centaur.trustedCaKeySecretName" -}}
{{- required "firewall.existingCaKeySecretName is required" .Values.firewall.existingCaKeySecretName | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{- define "centaur.repoCacheGithubTokenSecretName" -}}
{{- if .Values.repoCache.githubToken.existingSecretName -}}
{{- .Values.repoCache.githubToken.existingSecretName | trunc 63 | trimSuffix "-" -}}
{{- else -}}
{{- printf "%s-repo-cache-github-token" (include "centaur.fullname" .) | trunc 63 | trimSuffix "-" -}}
{{- end -}}
{{- end -}}

{{- define "centaur.overlaySources" -}}
{{- $sources := list -}}
{{- with .Values.overlays.sources -}}
{{- range . -}}
{{- if .repo -}}
{{- $source := dict "repo" .repo -}}
{{- with .ref }}{{- $_ := set $source "ref" . -}}{{- end -}}
{{- /*
Subdir defaults: an omitted key falls back to the conventional layout
(tools, workflows, .agents/skills); a key explicitly set to "" disables
that surface for the source. Missing directories are skipped at runtime,
so the defaults are safe for repos that only carry some surfaces.
*/ -}}
{{- if hasKey . "toolsSubdir" -}}
{{- with .toolsSubdir }}{{- $_ := set $source "toolsSubdir" . -}}{{- end -}}
{{- else -}}
{{- $_ := set $source "toolsSubdir" "tools" -}}
{{- end -}}
{{- if hasKey . "workflowsSubdir" -}}
{{- with .workflowsSubdir }}{{- $_ := set $source "workflowsSubdir" . -}}{{- end -}}
{{- else -}}
{{- $_ := set $source "workflowsSubdir" "workflows" -}}
{{- end -}}
{{- if hasKey . "skillsSubdir" -}}
{{- with .skillsSubdir }}{{- $_ := set $source "skillsSubdir" . -}}{{- end -}}
{{- else -}}
{{- $_ := set $source "skillsSubdir" ".agents/skills" -}}
{{- end -}}
{{- with .promptPath }}{{- $_ := set $source "promptPath" . -}}{{- end -}}
{{- with .personasSubdir }}{{- $_ := set $source "personasSubdir" . -}}{{- end -}}
{{- $sources = append $sources $source -}}
{{- end -}}
{{- end -}}
{{- else -}}
{{- if and .Values.toolServer.enabled .Values.toolServer.repo -}}
{{- $source := dict "repo" .Values.toolServer.repo "toolsSubdir" (default "tools" .Values.toolServer.subdir) "workflowsSubdir" "workflows" "skillsSubdir" ".agents/skills" -}}
{{- with .Values.toolServer.ref }}{{- $_ := set $source "ref" . -}}{{- end -}}
{{- $sources = append $sources $source -}}
{{- range .Values.toolServer.extraSources -}}
{{- if .repo -}}
{{- $source := dict "repo" .repo "toolsSubdir" (default "tools" .subdir) "workflowsSubdir" (default "workflows" .workflowsSubdir) "skillsSubdir" (default ".agents/skills" .skillsSubdir) -}}
{{- with .ref }}{{- $_ := set $source "ref" . -}}{{- end -}}
{{- $sources = append $sources $source -}}
{{- end -}}
{{- end -}}
{{- end -}}
{{- end -}}
{{- toJson $sources -}}
{{- end -}}

{{- define "centaur.httpRouteName" -}}
{{- $suffix := default (printf "route-%v" .index) .route.name -}}
{{- printf "%s-%s" (include "centaur.fullname" .root) $suffix | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{- define "centaur.secretResourceVersion" -}}
{{- $secret := lookup "v1" "Secret" .root.Release.Namespace .name -}}
{{- if $secret -}}
{{- $secret.metadata.resourceVersion -}}
{{- else -}}
{{- .name -}}
{{- end -}}
{{- end -}}

{{- define "centaur.infraSecretsChecksum" -}}
{{- $envName := include "centaur.secretEnvName" . -}}
{{- $caName := include "centaur.trustedCaSecretName" . -}}
{{- $caKeyName := include "centaur.trustedCaKeySecretName" . -}}
{{- $payload := dict "env" (include "centaur.secretResourceVersion" (dict "root" . "name" $envName)) "ca" (include "centaur.secretResourceVersion" (dict "root" . "name" $caName)) "caKey" (include "centaur.secretResourceVersion" (dict "root" . "name" $caKeyName)) -}}
{{- toJson $payload | sha256sum -}}
{{- end -}}

{{- /*
The upstream 1Password Connect subchart names its Service after
`connect.applicationName` (default `onepassword-connect`) and exposes the
API on `connect.api.httpPort` (default 8080). The Service is in the same
namespace as this release, so a short DNS name is enough.
*/ -}}
{{- define "centaur.onepasswordConnectAppName" -}}
{{- default "onepassword-connect" (((.Values.onepasswordConnect).connect).applicationName) -}}
{{- end -}}

{{- define "centaur.onepasswordConnectPort" -}}
{{- default 8080 ((((.Values.onepasswordConnect).connect).api).httpPort) -}}
{{- end -}}

{{- define "centaur.onepasswordConnectHost" -}}
{{- include "centaur.onepasswordConnectAppName" . -}}
{{- end -}}

{{- define "centaur.onepasswordConnectUrl" -}}
{{- printf "http://%s:%v" (include "centaur.onepasswordConnectHost" .) (include "centaur.onepasswordConnectPort" .) -}}
{{- end -}}

{{- /*
console — Rails control plane (formerly "iron-control") for authenticated API
access and encrypted secret storage. Flag-gated (console.enabled), in-cluster
ClusterIP Service.

Backwards compatibility: the canonical values key is `console`; `ironControl` is
a deprecated alias that is still honored. `centaur.consoleValues` returns the
effective config by deep-merging the two — chart defaults live under `console`,
and any explicitly set `ironControl.*` values are layered on top so existing
deployments that still configure `ironControl` keep working unchanged. If a key
is set under BOTH, the legacy `ironControl` value wins for that key. All
templates should consume this helper rather than `.Values.console` /
`.Values.ironControl` directly.

In-cluster Service/DNS names use the "console" component (e.g.
`<release>-centaur-console`). The api-rs-facing env vars stay IRON_CONTROL_URL /
IRON_CONTROL_API_KEY (their names are hardcoded in the Rust binaries); the URL
*value* is derived from `centaur.consoleUrl`, so it tracks the Service name.
*/ -}}
{{- define "centaur.consoleValues" -}}
{{- $console := deepCopy (.Values.console | default dict) -}}
{{- $legacy := .Values.ironControl | default dict -}}
{{- toYaml (mergeOverwrite $console $legacy) -}}
{{- end -}}

{{- define "centaur.consoleName" -}}
{{- include "centaur.componentName" (dict "root" . "component" "console") -}}
{{- end -}}

{{- define "centaur.consoleHost" -}}
{{- include "centaur.consoleName" . -}}
{{- end -}}

{{- define "centaur.consoleUrl" -}}
{{- $console := include "centaur.consoleValues" . | fromYaml -}}
{{- printf "http://%s:%v" (include "centaur.consoleHost" .) $console.service.httpPort -}}
{{- end -}}
