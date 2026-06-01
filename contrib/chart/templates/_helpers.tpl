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

{{- define "centaur.apiServiceAccountName" -}}
{{- printf "%s-api" (include "centaur.fullname" .) | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{- define "centaur.repoCacheGithubTokenSecretName" -}}
{{- if .Values.repoCache.githubToken.existingSecretName -}}
{{- .Values.repoCache.githubToken.existingSecretName | trunc 63 | trimSuffix "-" -}}
{{- else -}}
{{- printf "%s-repo-cache-github-token" (include "centaur.fullname" .) | trunc 63 | trimSuffix "-" -}}
{{- end -}}
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

{{- define "centaur.firewallProxyHost" -}}
centaur-api-proxy
{{- end -}}

{{- define "centaur.firewallProxyPort" -}}
{{- .Values.ironProxy.service.proxyPort -}}
{{- end -}}

{{- define "centaur.firewallProxyUrl" -}}
{{- printf "http://%s:%v" (include "centaur.firewallProxyHost" .) (include "centaur.firewallProxyPort" .) -}}
{{- end -}}

{{- define "centaur.firewallNoProxyHosts" -}}
{{- include "centaur.firewallProxyHost" . -}}
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
iron-token-broker — owns OAuth refresh-token state for credentials whose IdP
rotates refresh tokens with strict reuse detection (OpenAI Codex, Anthropic
Claude Code OAuth). One process, ClusterIP service, config rendered from
registered refresh_token OAuthTokenSecrets by the API server at startup.
*/ -}}
{{- define "centaur.tokenBrokerName" -}}
{{- include "centaur.componentName" (dict "root" . "component" "token-broker") -}}
{{- end -}}

{{- define "centaur.tokenBrokerHost" -}}
{{- include "centaur.tokenBrokerName" . -}}
{{- end -}}

{{- define "centaur.tokenBrokerUrl" -}}
{{- printf "http://%s:%v" (include "centaur.tokenBrokerHost" .) .Values.tokenBroker.service.httpPort -}}
{{- end -}}

{{- /*
iron-control — Rails control plane for authenticated API access and encrypted
secret storage. Flag-gated (ironControl.enabled), in-cluster ClusterIP Service.
*/ -}}
{{- define "centaur.ironControlName" -}}
{{- include "centaur.componentName" (dict "root" . "component" "iron-control") -}}
{{- end -}}

{{- define "centaur.ironControlHost" -}}
{{- include "centaur.ironControlName" . -}}
{{- end -}}

{{- define "centaur.ironControlUrl" -}}
{{- printf "http://%s:%v" (include "centaur.ironControlHost" .) .Values.ironControl.service.httpPort -}}
{{- end -}}
