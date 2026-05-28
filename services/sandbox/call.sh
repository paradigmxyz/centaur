#!/bin/bash
# call — token-efficient API tool caller (returns TOON)
# Usage:
#   call <tool> <method> [json_body]   → POST /tools/<tool>/<method>
#   call tools                          → GET /tools (list all)
#   call discover <tool>               → GET /tools/<tool>
U="${CENTAUR_API_URL:-http://api:8000}"
# Tools are served by an in-pod sidecar when CENTAUR_TOOLS_URL is set; otherwise
# fall back to the API server. Agent and workflow calls always go to the API.
TU="${CENTAUR_TOOLS_URL:-$U}"
T="Accept: text/plain"
J="Content-Type: application/json"

_host_from_url() {
  printf '%s\n' "$1" | sed -E 's#^[a-zA-Z][a-zA-Z0-9+.-]*://([^/:]+).*#\1#'
}

_append_no_proxy() {
  local additions="$1"
  export no_proxy="${no_proxy:+$no_proxy,}${additions}"
  export NO_PROXY="${NO_PROXY:+$NO_PROXY,}${additions}"
}

_api_host="$(_host_from_url "$U")"
_tools_host="$(_host_from_url "$TU")"
_append_no_proxy "localhost,127.0.0.1,api,centaur-api,centaur-centaur-api,centaur-api-proxy,.centaur.svc.cluster.local,${_api_host},${_tools_host}"
# Prefer refreshed token (written on warm-pool claim) over original env var
_KEY="${CENTAUR_API_KEY:-}"
if [ -f /home/agent/.api_key ]; then
  _KEY="$(cat /home/agent/.api_key)"
fi
_TRACE_ID="${CENTAUR_TRACE_ID:-}"
if [ -z "${_TRACE_ID:-}" ] && [ -f /home/agent/.trace_id ]; then
  _TRACE_ID="$(cat /home/agent/.trace_id)"
fi
A="Authorization: Bearer ${_KEY}"
tool="$1"
method="$2"
body="$3"

auth_headers=()
if [ -n "${_KEY}" ]; then
  auth_headers=(-H "$A")
fi

request() {
  local http_method="$1"
  local url="$2"
  local data="${3:-}"
  # Watchdog ceiling for the curl -> API hop, NOT a quality budget. Tool
  # plugins can legitimately run for many minutes (deep research crawls,
  # recursive people pulls, internal-priors aggregation, paradigm-pulse, etc.)
  # and Anthropic / OpenAI deep_research calls under the hood routinely take
  # 5-15 minutes. The previous 30s default made the helper report "failed"
  # while the upstream tool was still working, which trained agents to retry
  # the same call 3x in a row and waste compute — and to assume long-running
  # primitives are "broken" and pick a shallower alternative. 1800s is a
  # large-but-finite watchdog; for known-fast methods, set CALL_TIMEOUT_SECONDS
  # in the calling environment to bound more aggressively.
  local timeout_s="${CALL_TIMEOUT_SECONDS:-1800}"

  local curl_args=(
    -sS
    --max-time "$timeout_s"
    --retry 2
    --retry-connrefused
    -X "$http_method"
    "${auth_headers[@]}"
    -H "$T"
    "$url"
  )
  if [ -n "${_TRACE_ID:-}" ]; then
    curl_args+=(-H "X-Trace-Id: ${_TRACE_ID}")
  fi
  if [ -n "${CENTAUR_THREAD_KEY:-}" ]; then
    curl_args+=(-H "X-Centaur-Thread-Key: ${CENTAUR_THREAD_KEY}")
  fi
  if [ -n "$data" ]; then
    curl_args+=(-H "$J" -d "$data")
  fi

  local response
  response="$(curl "${curl_args[@]}" --write-out $'\n__HTTP_STATUS__:%{http_code}')"
  local curl_exit=$?
  if [ "$curl_exit" -ne 0 ]; then
    printf '{"error":"transport_error","exit_code":%d,"url":%s}\n' \
      "$curl_exit" \
      "$(printf '%s' "$url" | jq -Rs .)"
    return 1
  fi

  local status="${response##*__HTTP_STATUS__:}"
  local body="${response%$'\n'__HTTP_STATUS__:*}"
  if [[ "$status" =~ ^2 ]]; then
    printf '%s\n' "$body"
    return 0
  fi

  local snippet="${body:0:1200}"
  printf '{"error":"http_error","status":%s,"url":%s,"body":%s}\n' \
    "$status" \
    "$(printf '%s' "$url" | jq -Rs .)" \
    "$(printf '%s' "$snippet" | jq -Rs .)"
  return 1
}

agent_execute() {
  local payload="$1"

  if [ -z "$payload" ]; then
    printf '%s\n' '{"error":"invalid_request","message":"call agent execute requires a JSON body"}'
    return 1
  fi

  if ! printf '%s' "$payload" | jq -e . >/dev/null 2>&1; then
    printf '{"error":"invalid_json","body":%s}\n' "$(printf '%s' "$payload" | jq -Rs .)"
    return 1
  fi

  if printf '%s' "$payload" | jq -e 'has("assignment_generation")' >/dev/null 2>&1; then
    request "POST" "$U/agent/execute" "$payload"
    return $?
  fi

  if ! printf '%s' "$payload" | jq -e '(.message | type) == "string" and (.message | length) > 0' >/dev/null 2>&1; then
    request "POST" "$U/agent/execute" "$payload"
    return $?
  fi

  local nonce
  nonce="call-agent-$(date +%s)-$RANDOM"

  local spawn_payload
  spawn_payload="$(printf '%s' "$payload" | jq -c --arg spawn_id "${nonce}:spawn" '
    {
      thread_key,
      spawn_id: (.spawn_id // $spawn_id)
    }
    + (if .harness != null then {harness: .harness} else {} end)
    + (if .engine != null then {engine: .engine} else {} end)
    + (if .persona_id != null then {persona_id: .persona_id} elif .harness == null and (env.AGENT_PERSONA // "") != "" then {persona_id: env.AGENT_PERSONA} else {} end)
    + (if .agents_md_override != null then {agents_md_override: .agents_md_override} else {} end)
  ')"

  local spawn_response
  spawn_response="$(request "POST" "$U/agent/spawn" "$spawn_payload")" || {
    printf '%s\n' "$spawn_response"
    return 1
  }

  local assignment_generation
  assignment_generation="$(printf '%s' "$spawn_response" | jq -r '.assignment_generation // empty')"
  if ! [[ "$assignment_generation" =~ ^[0-9]+$ ]] || [ "$assignment_generation" -le 0 ]; then
    printf '{"error":"invalid_spawn_response","body":%s}\n' "$(printf '%s' "$spawn_response" | jq -Rs .)"
    return 1
  fi

  local message_payload
  message_payload="$(printf '%s' "$payload" | jq -c --argjson assignment_generation "$assignment_generation" --arg message_id "${nonce}:message" '
    {
      thread_key,
      assignment_generation: $assignment_generation,
      message_id: (.message_id // $message_id),
      role: (.role // "user"),
      parts: [{type: "text", text: .message}]
    }
    + (if .user_id != null then {user_id: .user_id} else {} end)
    + (if .metadata != null then {metadata: .metadata} else {} end)
  ')"

  local message_response
  message_response="$(request "POST" "$U/agent/message" "$message_payload")" || {
    printf '%s\n' "$message_response"
    return 1
  }

  local execute_payload
  execute_payload="$(printf '%s' "$payload" | jq -c --argjson assignment_generation "$assignment_generation" --arg execute_id "${nonce}:execute" '
    {
      thread_key,
      assignment_generation: $assignment_generation,
      execute_id: (.execute_id // $execute_id)
    }
    + (if .harness != null then {harness: .harness} else {} end)
    + (if .delivery != null then {delivery: .delivery} else {} end)
    + (if .platform != null then {platform: .platform} else {} end)
    + (if .user_id != null then {user_id: .user_id} else {} end)
    + (if .metadata != null then {metadata: .metadata} else {} end)
  ')"

  request "POST" "$U/agent/execute" "$execute_payload"
}

case "$tool" in
  search)
    printf '%s\n' '{"error":"deprecated_command","command":"call search","replacement":"Use direct tool calls after `call tools` / `call discover <tool>` — for example `call websearch search '\''{\"query\":\"...\"}'\''` or another deployment-specific search method."}'
    exit 1
    ;;
  sql)
    printf '%s\n' '{"error":"deprecated_command","command":"call sql","replacement":"Use a tool-specific query method exposed by your deployment after `call discover <tool>` (for example a database or analytics tool that supports SQL)."}'
    exit 1
    ;;
  tools)
    # Inject the built-in agent sub-command into the tool listing
    response="$(request "GET" "$TU/tools")" || { printf '%s\n' "$response"; exit 1; }
    printf '%s' "$response" | jq -c '. + {"agent":{"description":"Sub-agent dispatch (built-in). Use: call agent execute, call agent status, call agent runtime, call agent stop","methods":["execute","status","runtime","stop"]}}'
    printf '\n'
    ;;
  discover)
    if [ "$2" = "agent" ]; then
      printf '%s\n' '{"tool":"agent","description":"Sub-agent dispatch (built-in, not a tool plugin)","methods":[{"name":"execute","description":"Spawn a sub-agent. Body: {\"thread_key\":\"task:<purpose>-<id>\",\"message\":\"...\",\"harness\":\"<persona>\"}. Returns {execution_id, status}."},{"name":"status","description":"Poll sub-agent. Usage: call agent status '\''?key=<thread_key>'\''"},{"name":"runtime","description":"Inspect active persona/overlay/available personas for a thread. Usage: call agent runtime '\''?key=<thread_key>'\''"},{"name":"stop","description":"Stop sub-agent. Body: {\"thread_key\":\"...\"}"}]}'
    else
      request "GET" "$TU/tools/$2"
    fi
    ;;
  agent)
    # Usage: call agent execute '{"thread_key":"...","message":"...","harness":"legal"}'
    #        call agent execute '{"thread_key":"...","assignment_generation":1,...}'
    #        call agent stop '{"thread_key":"..."}'
    #        call agent status '?key=...'
    #        call agent runtime '?key=...'
    if [ "$method" = "status" ]; then
      request "GET" "$U/agent/status$body"
    elif [ "$method" = "runtime" ]; then
      request "GET" "$U/agent/runtime$body"
    elif [ "$method" = "execute" ]; then
      agent_execute "$body"
    else
      request "POST" "$U/agent/$method" "$body"
    fi
    ;;
  workflow)
    # Usage: call workflow run '{"workflow_name":"agent_loop","input":{...}}'
    #        call workflow get <run_id>
    #        call workflow cancel <run_id>
    #        call workflow list
    if [ "$method" = "run" ]; then
      request "POST" "$U/workflows/runs" "$body"
    elif [ "$method" = "get" ]; then
      request "GET" "$U/workflows/runs/$body"
    elif [ "$method" = "cancel" ]; then
      request "POST" "$U/workflows/runs/$body/cancel"
    elif [ "$method" = "list" ]; then
      request "GET" "$U/workflows/runs${body:+?$body}"
    elif [ "$method" = "event" ]; then
      request "POST" "$U/workflows/events" "$body"
    else
      printf '{"error":"unknown_workflow_method","method":%s}\n' "$(printf '%s' "$method" | jq -Rs .)"
      exit 1
    fi
    ;;
  *)
    if [ -z "$body" ]; then
      request "POST" "$TU/tools/$tool/$method"
    else
      request "POST" "$TU/tools/$tool/$method" "$body"
    fi
    ;;
esac
