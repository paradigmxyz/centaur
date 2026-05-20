#!/usr/bin/env bash
set -euo pipefail

namespace="${CENTAUR_NAMESPACE:-centaur}"
release="${CENTAUR_RELEASE:-centaur}"
link="${1:-}"
since="${2:-24h}"

if [[ "$link" =~ /archives/([^/]+)/p([0-9]+) ]]; then
  channel="${BASH_REMATCH[1]}"
  raw_ts="${BASH_REMATCH[2]}"
else
  echo "expected Slack archive URL like https://app.slack.com/archives/C9Q8R7S6T5/p1234567890123456" >&2
  exit 2
fi

channel_lc="$(printf '%s' "$channel" | tr '[:upper:]' '[:lower:]')"
ts_prefix="${raw_ts:0:10}"
ts_exact="${raw_ts:0:10}.${raw_ts:10}"

echo "Searching ${namespace} logs for channel=${channel} ts=${ts_exact} since=${since}" >&2
{
  kubectl -n "$namespace" logs "deploy/${release}-centaur-api" --since="$since" --prefix=true || true
  kubectl -n "$namespace" logs "deploy/${release}-centaur-slackbot" --since="$since" --prefix=true || true
  while IFS= read -r pod; do
    kubectl -n "$namespace" logs "$pod" --since="$since" --prefix=true || true
  done < <(kubectl -n "$namespace" get pod -o name | rg "sandbox-slack.*${channel_lc}" || true)
} | rg "${channel}|${channel_lc}|${ts_exact}|${ts_prefix}" | while IFS= read -r line; do
  if [[ "$line" =~ ^(\[[^]]+\][[:space:]]+)(\{.*\})$ ]]; then
    prefix="${BASH_REMATCH[1]}"
    json="${BASH_REMATCH[2]}"
    printf '%s\n' "${prefix}"
    if ! jq . <<<"$json"; then
      printf '%s\n' "$json"
    fi
  else
    printf '%s\n' "$line"
  fi
done || true
