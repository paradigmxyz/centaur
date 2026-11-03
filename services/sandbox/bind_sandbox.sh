#!/bin/bash
# Bind hook for `harness-server serve`. A generic (for example warm-pool)
# sandbox boots through entrypoint.sh without a session; once harness-server has
# applied a `sandbox.bind` line's env and files, it runs this hook to redo the
# session-dependent tail of entrypoint.sh before starting the bound harness.
#
# harness-server sets:
#   CENTAUR_BIND_HARNESS      harness subcommand being started
#   CENTAUR_BIND_CHANGED_ENV  comma-separated env names the bind line changed
# The working directory is the workspace entrypoint.sh switched to.
set -e

HOME_DIR="$(eval echo ~)"
WORKSPACE_DIR="$PWD"

bind_changed() {
    case ",${CENTAUR_BIND_CHANGED_ENV:-}," in
        *",$1,"*) return 0 ;;
    esac
    return 1
}

# Tool filters decide which shims exist; rerun only when the session changed
# them, since a full shim install is the slow part of startup.
if [ -n "${TOOL_DIRS:-}" ] && { bind_changed TOOL_ALLOWLIST || bind_changed TOOL_BLOCKLIST; }; then
    install-tool-shims || echo "warning: failed to install Centaur tool CLI shims" >&2
fi
if bind_changed CENTAUR_SKILL_DIRS; then
    WORKSPACE_DIR="$WORKSPACE_DIR" install-tool-shims --refresh-skills \
        || echo "warning: failed to reload Centaur skills" >&2
fi

# The persona file and observability access are session state, so the prompt
# is always recomposed.
TARGET_PROMPT="$WORKSPACE_DIR/AGENTS.md"
compose-system-prompt \
    --home-dir "$HOME_DIR" \
    --target-prompt "$TARGET_PROMPT"

if [ "${CENTAUR_BIND_HARNESS:-}" = "amp" ] && [ -f "$TARGET_PROMPT" ]; then
    rm -f "$WORKSPACE_DIR/AGENT.md"
    ln -s "$(basename "$TARGET_PROMPT")" "$WORKSPACE_DIR/AGENT.md"
fi
