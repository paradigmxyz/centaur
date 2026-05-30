#!/usr/bin/env bash
# Tool-server sidecar entrypoint: install overlay tool deps, then run uvicorn.
#
# The sidecar runs the API image but overrides its ENTRYPOINT, so it must do the
# overlay tool-dep install itself (the API container gets it via entrypoint.sh).
# The sidecar runs as a non-root user and cannot write the root-owned
# /app/.venv, so deps install into a writable --target dir that this script
# then puts on PYTHONPATH before exec'ing uvicorn.
#
# Overlay tools ship as source only, so their pyproject.toml dependencies are
# not baked into the image. Base tools under /app/tools are installed at build
# time and skipped here.
#
# Egress note: the sidecar reaches the package index through iron-proxy, so the
# index hosts (e.g. pypi.org, files.pythonhosted.org) must be firewall-allowed
# for the install to succeed.
#
# Args:
#   $1  port for uvicorn to listen on
#   $2  writable dir to install overlay tool deps into; also added to PYTHONPATH
#
# Best-effort: a failed install surfaces as a per-tool ImportError, so always
# exec uvicorn afterward so /healthz still comes up.
set -uo pipefail

port="${1:?port required}"
target="${2:?deps target dir required}"

if [[ -n "${TOOL_DIRS:-}" ]]; then
  deps_file="$(mktemp)"
  IFS=':' read -ra _dirs <<< "$TOOL_DIRS"
  for _d in "${_dirs[@]}"; do
    [[ "$_d" == "/app/tools" ]] && continue   # baked into the image at build time
    [[ -d "$_d" ]] || continue
    /app/.venv/bin/python - "$_d" >> "$deps_file" 2>/dev/null <<'PY' || true
import sys, tomllib, pathlib

deps: set[str] = set()
for path in pathlib.Path(sys.argv[1]).glob("**/pyproject.toml"):
    try:
        with open(path, "rb") as fh:
            data = tomllib.load(fh)
    except Exception:
        continue
    deps.update(data.get("project", {}).get("dependencies", []))
print("\n".join(sorted(deps)))
PY
  done

  sort -u "$deps_file" | grep -v '^[[:space:]]*$' > "${deps_file}.uniq" || true
  mv -f "${deps_file}.uniq" "$deps_file"
  if [[ -s "$deps_file" ]]; then
    mkdir -p "$target"
    # This container runs as a non-root user with no writable HOME, so uv would
    # default its cache to /.cache/uv and fail to create it. Point it at a
    # writable path. The cache is per-pod and ephemeral, which is fine.
    export UV_CACHE_DIR="${UV_CACHE_DIR:-/tmp/uv-cache}"
    echo "tool-server-startup: installing overlay tool deps into $target" >&2
    uv pip install --python /app/.venv/bin/python --target "$target" -r "$deps_file" \
      || echo "tool-server-startup: dep install failed; affected tools will error at call time" >&2
  fi
  rm -f "$deps_file" "${deps_file}.uniq"
fi

# Make the installed overlay deps importable by the in-process tool loader.
# Prepend so they take precedence, but keep any operator-provided PYTHONPATH.
export PYTHONPATH="$target${PYTHONPATH:+:$PYTHONPATH}"

exec /app/.venv/bin/uvicorn api.tool_server_app:app --host 0.0.0.0 --port "$port"
