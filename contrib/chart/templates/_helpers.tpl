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

{{/*
NetworkPolicy egress rule for Postgres clients. With the bundled Postgres,
allow only its pod. With an external database (postgres.enabled=false), allow
externalDatabase.cidrs (e.g. RDS or Cloud SQL) and the pods matching
externalDatabase.podSelector (an in-cluster database) on externalDatabase.port.
NetworkPolicy cannot match hostnames, so the ranges must cover every address
the database endpoint can resolve to.
*/}}
{{- define "centaur.postgresEgress" -}}
{{- $root := . -}}
{{- if $root.Values.postgres.enabled -}}
- to:
    - podSelector:
        matchLabels:
{{ include "centaur.componentSelectorLabels" (dict "root" $root "component" "postgres") | indent 10 }}
  ports:
    - protocol: TCP
      port: 5432
{{- else -}}
{{- $external := $root.Values.externalDatabase -}}
{{- if and (not $external.cidrs) (not $external.podSelector) -}}
{{- fail "externalDatabase.cidrs or externalDatabase.podSelector must select the external database when postgres.enabled=false and networkPolicy.enabled=true" -}}
{{- end -}}
- to:
{{- range $external.cidrs }}
    - ipBlock:
        cidr: {{ . | quote }}
{{- end }}
{{- with $external.podSelector }}
    - podSelector:
        matchLabels:
{{ toYaml . | indent 10 }}
{{- with $external.namespace }}
      namespaceSelector:
        matchLabels:
          kubernetes.io/metadata.name: {{ . | quote }}
{{- end }}
{{- end }}
  ports:
    - protocol: TCP
      port: {{ $external.port }}
{{- end -}}
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

{{- define "centaur.repoCachePvcName" -}}
{{- if .Values.repoCache.storage.persistentVolumeClaim.existingClaim -}}
{{- .Values.repoCache.storage.persistentVolumeClaim.existingClaim | trunc 63 | trimSuffix "-" -}}
{{- else -}}
{{- printf "%s-repo-cache" (include "centaur.fullname" .) | trunc 63 | trimSuffix "-" -}}
{{- end -}}
{{- end -}}

{{- define "centaur.repoCacheStorageType" -}}
{{- $storageType := default "hostPath" .Values.repoCache.storage.type -}}
{{- if and (ne $storageType "hostPath") (ne $storageType "persistentVolumeClaim") -}}
{{- fail "repoCache.storage.type must be either hostPath or persistentVolumeClaim" -}}
{{- end -}}
{{- $storageType -}}
{{- end -}}

{{- define "centaur.repositoryVisibility" -}}
{{- $visibility := lower (default "private" .) -}}
{{- if eq $visibility "public" -}}public{{- else -}}private{{- end -}}
{{- end -}}

{{- define "centaur.overlaySources" -}}
{{- $sources := list -}}
{{- with .Values.overlays.sources -}}
{{- range . -}}
{{- if .repo -}}
{{- $source := dict "repo" .repo -}}
{{- with .ref }}{{- $_ := set $source "ref" . -}}{{- end -}}
{{- $_ := set $source "visibility" (include "centaur.repositoryVisibility" .visibility) -}}
{{- /*
Subdir defaults: an omitted key falls back to the conventional layout
(tools, workflows, .agents/skills, connectors); a key explicitly set to ""
disables that surface for the source. Missing directories are skipped at
runtime, so the defaults are safe for repos that only carry some surfaces.
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
{{- if hasKey . "connectorsSubdir" -}}
{{- with .connectorsSubdir }}{{- $_ := set $source "connectorsSubdir" . -}}{{- end -}}
{{- else -}}
{{- $_ := set $source "connectorsSubdir" "connectors" -}}
{{- end -}}
{{- with .promptPath }}{{- $_ := set $source "promptPath" . -}}{{- end -}}
{{- with .personasSubdir }}{{- $_ := set $source "personasSubdir" . -}}{{- end -}}
{{- $sources = append $sources $source -}}
{{- end -}}
{{- end -}}
{{- else -}}
{{- if and .Values.toolServer.enabled .Values.toolServer.repo -}}
{{- $source := dict "repo" .Values.toolServer.repo "toolsSubdir" (default "tools" .Values.toolServer.subdir) "workflowsSubdir" "workflows" "skillsSubdir" ".agents/skills" "connectorsSubdir" "connectors" -}}
{{- with .Values.toolServer.ref }}{{- $_ := set $source "ref" . -}}{{- end -}}
{{- $_ := set $source "visibility" (include "centaur.repositoryVisibility" .Values.toolServer.visibility) -}}
{{- $sources = append $sources $source -}}
{{- range .Values.toolServer.extraSources -}}
{{- if .repo -}}
{{- $source := dict "repo" .repo "toolsSubdir" (default "tools" .subdir) "workflowsSubdir" (default "workflows" .workflowsSubdir) "skillsSubdir" (default ".agents/skills" .skillsSubdir) "connectorsSubdir" (default "connectors" .connectorsSubdir) -}}
{{- with .ref }}{{- $_ := set $source "ref" . -}}{{- end -}}
{{- $_ := set $source "visibility" (include "centaur.repositoryVisibility" .visibility) -}}
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
Render the standard Kubernetes PodSpec scheduling fields supported by
chart-managed workloads.
*/ -}}
{{- define "centaur.podScheduling" -}}
{{- $values := .values -}}
{{- $indent := .indent -}}
{{- with $values.nodeSelector -}}
{{- printf "nodeSelector:\n%s" (toYaml . | indent 2) | nindent $indent }}
{{- end -}}
{{- with $values.affinity -}}
{{- printf "affinity:\n%s" (toYaml . | indent 2) | nindent $indent }}
{{- end -}}
{{- with $values.tolerations -}}
{{- printf "tolerations:\n%s" (toYaml . | indent 2) | nindent $indent }}
{{- end -}}
{{- end -}}

{{- /*
console — control plane (formerly "iron-control"), served by api-rs, for authenticated API
access and encrypted secret storage. Required in-cluster ClusterIP Service.

Backwards compatibility: the canonical values key is `console`; `ironControl` is
a deprecated alias that is still honored. `centaur.consoleValues` returns the
effective config by deep-merging the two — chart defaults live under `console`,
and any explicitly set `ironControl.*` values are layered on top so existing
deployments that still configure `ironControl` keep working unchanged. If a key
is set under BOTH, the legacy `ironControl` value wins for that key. All
templates should consume this helper rather than `.Values.console` /
`.Values.ironControl` directly.

In-cluster Service/DNS names use the "console" component (e.g.
`<release>-centaur-console`). The api-rs-facing env var stays IRON_CONTROL_URL
(its name is hardcoded in the Rust binaries); the URL *value* is derived from
`centaur.consoleUrl`.

The Console is served by api-rs. The `console` Service survives only as a
compatibility alias (same name and port, forwarding to api-rs) for in-cluster
clients that default to `centaur-console:3000`, and `centaur.consoleUrl` returns
the api-rs Service URL. api-rs also owns the Console schema and the Slack
channel catalog.
*/ -}}
{{- define "centaur.consoleValues" -}}
{{- $console := deepCopy (.Values.console | default dict) -}}
{{- $legacy := .Values.ironControl | default dict -}}
{{- toYaml (mergeOverwrite $console $legacy) -}}
{{- end -}}

{{- /*
Company-context values. `companyContext` used to be named
`experimentalCompanyContext`; the old key is a deprecated alias that is still
honored. As with `ironControl`, explicitly set alias values are layered over
the chart defaults and win for any key set under both. Templates should
consume this helper rather than the raw values.
*/ -}}
{{- define "centaur.companyContextValues" -}}
{{- $companyContext := deepCopy (.Values.companyContext | default dict) -}}
{{- $legacy := .Values.experimentalCompanyContext | default dict -}}
{{- toYaml (mergeOverwrite $companyContext $legacy) -}}
{{- end -}}

{{- define "centaur.consoleName" -}}
{{- include "centaur.componentName" (dict "root" . "component" "console") -}}
{{- end -}}

{{- /*
The in-cluster Console URL. The Console is served by api-rs, so this is always
the api-rs Service: sandboxes, iron-proxy, proxy-sync, and api-rs itself
(`IRON_CONTROL_URL`) dial it. The Console is part of api-rs, so a deployment with
apiRs.enabled=false has none; fail rather than render a URL nothing answers.
*/ -}}
{{- define "centaur.consoleUrl" -}}
{{- if not .Values.apiRs.enabled -}}
{{- fail "the Console is served by api-rs: this chart requires apiRs.enabled=true" -}}
{{- end -}}
{{- printf "http://%s:%v" (include "centaur.componentName" (dict "root" . "component" "api-rs")) .Values.apiRs.port -}}
{{- end -}}

{{- /*
Init container that blocks on a psql predicate against a Console database.
Console-database clients use it so they start only after api-rs has applied
the schema.

Params: root, name (container name), database (PGDATABASE), query (SQL returning
t for ready), message (log line while waiting). It connects through
IRON_CONTROL_DATABASE_URL, which carries no database path, so PGDATABASE selects
the logical database.
*/ -}}
{{- define "centaur.consoleDbWait" -}}
{{- $root := .root -}}
- name: {{ .name }}
  image: {{ printf "%s:%s" $root.Values.postgres.image.repository $root.Values.postgres.image.tag | quote }}
  imagePullPolicy: {{ $root.Values.postgres.image.pullPolicy }}
  command:
    - /bin/sh
    - -ec
    - |
      until [ "$(psql "$CONSOLE_DATABASE_URL" -tAc "{{ .query }}")" = "t" ]; do
        echo "{{ .message }}"
        sleep 2
      done
  env:
    - name: CONSOLE_DATABASE_URL
      valueFrom:
        secretKeyRef:
          name: {{ include "centaur.secretEnvName" $root }}
          key: {{ printf "%sIRON_CONTROL_DATABASE_URL" $root.Values.secretManager.envPrefix }}
    - name: PGDATABASE
      value: {{ .database | quote }}
  securityContext:
{{ toYaml $root.Values.containerSecurityContext | nindent 4 }}
    seccompProfile:
      type: RuntimeDefault
{{- end -}}

{{- define "centaur.proxySyncName" -}}
{{- include "centaur.componentName" (dict "root" . "component" "proxy-sync") -}}
{{- end -}}

{{- define "centaur.proxySyncUrl" -}}
{{- printf "http://%s:%v" (include "centaur.proxySyncName" .) .Values.proxySync.port -}}
{{- end -}}

{{- /*
Code Mode connector roots: the image's baked connectors, then overlay copies in
source order (later roots shadow earlier ones). Code Mode sandboxes and the
executor load the same roots.
*/ -}}
{{- define "centaur.codemodeConnectorRoots" -}}
{{- $roots := list "/opt/centaur/connectors" -}}
{{- if .Values.repoCache.enabled -}}
{{- range $source := include "centaur.overlaySources" . | fromJsonArray -}}
{{- $repo := get $source "repo" | default "" -}}
{{- if $repo -}}
{{- with (get $source "connectorsSubdir") -}}
{{- $roots = append $roots (printf "/home/agent/github/%s/%s" $repo .) -}}
{{- end -}}
{{- end -}}
{{- end -}}
{{- end -}}
{{- join ":" $roots -}}
{{- end -}}

{{- /*
Code Mode executor and its tenancy iron-proxy (codemode.executor). Both listen
on fixed ports: the executor on 8790, the proxy on 8080 (the image's tenancy
config, services/iron-proxy/iron-proxy-tenancy.yaml).
*/ -}}
{{- define "centaur.codemodeExecutorName" -}}
{{- include "centaur.componentName" (dict "root" . "component" "codemode-executor") -}}
{{- end -}}

{{- define "centaur.codemodeIronProxyName" -}}
{{- include "centaur.componentName" (dict "root" . "component" "codemode-iron-proxy") -}}
{{- end -}}

{{- /*
One env entry from the Code Mode Secret (codemode.executor.existingSecretName).
Params: root, name (env var), key (token-signing-key, token-public-key, or
sync-token).
*/ -}}
{{- define "centaur.codemodeSecretEnv" -}}
- name: {{ .name }}
  valueFrom:
    secretKeyRef:
      name: {{ required "codemode.executor.existingSecretName is required when codemode.executor.enabled=true" .root.Values.codemode.executor.existingSecretName | quote }}
      key: {{ .key }}
{{- end -}}
